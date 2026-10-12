//! Audio: default output volume/mute and default input (mic) mute through PipeWire.
//!
//! Reads and writes use `wpctl` (WirePlumber), which is the one stable CLI for "the default
//! sink/source". Change events come from `pactl subscribe` (pipewire-pulse), a long-lived
//! child that prints one line per change, so the agent sleeps until something happens. No
//! build-time dependencies, nothing polled. Both tools are injectable ([`Tools`]) for tests.
//! A native `pipewire` crate backend can replace this behind the same [`Audio`] state later
//! (docs/integration.md §3).

use crate::Handle;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

const SINK: &str = "@DEFAULT_AUDIO_SINK@";
const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";

/// `None` = unknown (no PipeWire, no such device).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Audio {
    /// Output volume, percent; may exceed 100 when boosted.
    pub volume_pct: Option<u8>,
    pub muted: Option<bool>,
    pub mic_volume_pct: Option<u8>,
    pub mic_muted: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Tools {
    pub wpctl: PathBuf,
    pub pactl: PathBuf,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            wpctl: "wpctl".into(),
            pactl: "pactl".into(),
        }
    }
}

/// Parse `wpctl get-volume` output: `Volume: 0.69` or `Volume: 0.40 [MUTED]`.
pub fn parse_volume(out: &str) -> Option<(u8, bool)> {
    let rest = out.trim().strip_prefix("Volume:")?;
    let mut words = rest.split_whitespace();
    let v: f64 = words.next()?.parse().ok()?;
    let muted = words.any(|w| w == "[MUTED]");
    Some(((v * 100.0).round().clamp(0.0, 255.0) as u8, muted))
}

/// Does a `pactl subscribe` line concern the default devices? Stream events (`sink-input`,
/// `source-output`) are ignored: they fire constantly while audio plays.
pub fn is_device_event(line: &str) -> bool {
    let Some((_, target)) = line.split_once(" on ") else {
        return false;
    };
    let kind = target.split_whitespace().next().unwrap_or("");
    matches!(kind, "sink" | "source" | "server" | "card")
}

/// A `wpctl` that does not answer (WirePlumber stuck or absent) must not wedge the audio thread
/// or startup: give it this long, then kill it.
const TOOL_TIMEOUT: Duration = Duration::from_secs(3);

/// `cmd.output()` with a deadline. Returns `None` on spawn failure or timeout (the child is killed).
pub(crate) fn output_with_timeout(
    mut cmd: Command,
    limit: Duration,
) -> Option<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Tied to this process: a stuck tool never outlives the agent.
    let child = sliver_proc::spawn_tied(cmd).ok()?;
    let pid = child.id();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(r) => r.ok(),
        Err(_) => {
            // Stuck: kill it so the waiting thread (and its pipe) end too.
            if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
            None
        }
    }
}

fn get(tools: &Tools, target: &str) -> Option<(u8, bool)> {
    let mut cmd = Command::new(&tools.wpctl);
    cmd.args(["get-volume", target]);
    let out = output_with_timeout(cmd, TOOL_TIMEOUT)?;
    out.status
        .success()
        .then(|| parse_volume(&String::from_utf8_lossy(&out.stdout)))?
}

/// Read the current state of the default devices.
pub fn read(tools: &Tools) -> Audio {
    let sink = get(tools, SINK);
    let source = get(tools, SOURCE);
    Audio {
        volume_pct: sink.map(|s| s.0),
        muted: sink.map(|s| s.1),
        mic_volume_pct: source.map(|s| s.0),
        mic_muted: source.map(|s| s.1),
    }
}

fn run(tools: &Tools, args: &[&str]) -> io::Result<()> {
    let st = Command::new(&tools.wpctl)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    st.success()
        .then_some(())
        .ok_or_else(|| io::Error::other(format!("wpctl {args:?} failed")))
}

/// What to do to a mute switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mute {
    On,
    Off,
    Toggle,
}

impl Mute {
    fn arg(self) -> &'static str {
        match self {
            Mute::On => "1",
            Mute::Off => "0",
            Mute::Toggle => "toggle",
        }
    }
}

/// Set the output volume, capped at 150% (`wpctl -l`).
pub fn set_volume(tools: &Tools, pct: u8) -> io::Result<()> {
    let v = format!("{:.2}", f64::from(pct.min(150)) / 100.0);
    run(tools, &["set-volume", "-l", "1.5", SINK, &v])
}

/// Relative change, clamped to 0..=100% (a slider shouldn't push past unity by accident).
pub fn change_volume(tools: &Tools, delta_pct: i8) -> io::Result<()> {
    let d = format!(
        "{}%{}",
        delta_pct.unsigned_abs(),
        if delta_pct < 0 { "-" } else { "+" }
    );
    run(tools, &["set-volume", "-l", "1.0", SINK, &d])
}

pub fn set_mute(tools: &Tools, m: Mute) -> io::Result<()> {
    run(tools, &["set-mute", SINK, m.arg()])
}

pub fn set_mic_mute(tools: &Tools, m: Mute) -> io::Result<()> {
    run(tools, &["set-mute", SOURCE, m.arg()])
}

impl Handle {
    /// Read the audio state now and keep it current until the session is dropped. Spawns a
    /// thread that follows `pactl subscribe` (restarting it with backoff if it dies, e.g. when
    /// pipewire-pulse restarts) and re-reads on every device event, coalescing bursts.
    pub fn start_audio(&self, tools: Tools) {
        let hub = self.hub.clone();
        // The first read happens on the audio thread, so a wpctl that never answers cannot hold
        // up the caller (the agent's startup).
        std::thread::Builder::new()
            .name("sliver-session-audio".into())
            .spawn(move || {
                let first = read(&tools);
                if !hub.update(|s| s.audio = first) {
                    return;
                }
                let mut backoff = Duration::from_millis(500);
                loop {
                    // Tied to this process (T47): `pactl subscribe` never runs on as an orphan
                    // after the agent (or a test binary) exits, however it exits.
                    let mut cmd = Command::new(&tools.pactl);
                    cmd.arg("subscribe")
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::null());
                    let Ok(mut child) = sliver_proc::spawn_tied(cmd) else {
                        if !tools.pactl.exists() && tools.pactl.is_absolute() {
                            return;
                        }
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(10));
                        continue;
                    };
                    // Only a pactl that stays up counts as recovered (below): without a PulseAudio
                    // server it starts fine and exits at once, and resetting the backoff here made
                    // that a ~1 Hz respawn loop of pactl + wpctl + threads (T101, invariant 3).
                    let started = std::time::Instant::now();
                    // Missed events while it was down: resync once connected again.
                    if !hub.update(|s| s.audio = read(&tools)) {
                        let _ = child.kill();
                        return;
                    }
                    let (tx, rx) = channel();
                    // `pactl` needs a moment to subscribe; changes in that window are lost, so
                    // re-read once more shortly after it started.
                    let late = tx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(250));
                        let _ = late.send(());
                    });
                    let stdout = child.stdout.take().expect("piped");
                    std::thread::spawn(move || {
                        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                            if is_device_event(&line) && tx.send(()).is_err() {
                                return;
                            }
                        }
                    });
                    // recv blocks (no polling); after one event, swallow the burst that follows.
                    while rx.recv().is_ok() {
                        loop {
                            match rx.recv_timeout(Duration::from_millis(40)) {
                                Ok(()) => {}
                                Err(RecvTimeoutError::Timeout) => break,
                                Err(RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        if !hub.update(|s| s.audio = read(&tools)) {
                            let _ = child.kill();
                            let _ = child.wait();
                            return;
                        }
                    }
                    let _ = child.wait();
                    backoff = next_backoff(backoff, started.elapsed());
                    std::thread::sleep(backoff);
                }
            })
            .expect("spawn thread");
    }
}

/// After `pactl subscribe` ran for `lived`: back to a quick retry if it had been up a while
/// (pipewire-pulse restarted), else keep backing off (no server at all), up to 30 s.
fn next_backoff(current: Duration, lived: Duration) -> Duration {
    if lived >= Duration::from_secs(10) {
        Duration::from_millis(500)
    } else {
        (current * 2).min(Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_resets_only_after_a_real_run() {
        let ms = Duration::from_millis;
        // No server: pactl dies at once every time -> 1 s, 2 s, ... capped at 30 s.
        let mut b = ms(500);
        let mut seen = vec![];
        for _ in 0..8 {
            b = next_backoff(b, ms(5));
            seen.push(b.as_millis());
        }
        assert_eq!(seen, [1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
        // It had been up for a while (pipewire-pulse restarted): retry quickly.
        assert_eq!(
            next_backoff(Duration::from_secs(30), Duration::from_secs(60)),
            ms(500)
        );
    }

    #[test]
    fn parses_wpctl_output() {
        assert_eq!(parse_volume("Volume: 0.69\n"), Some((69, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]"), Some((40, true)));
        assert_eq!(parse_volume("Volume: 1.50"), Some((150, false)));
        assert_eq!(parse_volume("Could not find"), None);
        assert_eq!(parse_volume(""), None);
    }

    #[test]
    fn filters_events() {
        assert!(is_device_event("Event 'change' on sink #54"));
        assert!(is_device_event("Event 'change' on source #55"));
        assert!(is_device_event("Event 'change' on server"));
        assert!(!is_device_event("Event 'new' on sink-input #12"));
        assert!(!is_device_event("Event 'change' on source-output #3"));
        assert!(!is_device_event("garbage"));
    }
}
