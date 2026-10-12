//! Audio source against fake `wpctl`/`pactl` scripts (no PipeWire needed).

use sliver_session::audio::{self, Tools};
use sliver_session::{Session, SessionSource, SessionState};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

mod common;
use common::Bus;

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn next(rx: &Receiver<SessionState>) -> SessionState {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("a state change within 10s")
}

#[test]
fn follows_pactl_events_and_reads_wpctl() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let dir = std::env::temp_dir().join(format!("sliver-audio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.display();
    std::fs::write(dir.join("sink"), "Volume: 0.30\n").unwrap();
    std::fs::write(dir.join("source"), "Volume: 1.00\n").unwrap();
    std::fs::write(dir.join("events"), "").unwrap();
    // wpctl get-volume <target> prints a state file; set-* append to a log.
    script(
        &dir.join("wpctl"),
        &format!(
            r#"echo "$@" >> {d}/calls
case "$1 $2" in
  "get-volume @DEFAULT_AUDIO_SINK@") cat {d}/sink;;
  "get-volume @DEFAULT_AUDIO_SOURCE@") cat {d}/source;;
esac"#
        ),
    );
    script(&dir.join("pactl"), &format!("exec tail -n0 -F {d}/events"));
    let tools = Tools {
        wpctl: dir.join("wpctl"),
        pactl: dir.join("pactl"),
    };

    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    let handle = session.handle();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut s = session;
        while let Ok(Some(st)) = s.next_change() {
            if tx.send(st).is_err() {
                return;
            }
        }
    });
    let _initial = next(&rx);

    handle.start_audio(tools.clone());
    let a = next(&rx).audio;
    assert_eq!(a.volume_pct, Some(30));
    assert_eq!(a.muted, Some(false));
    assert_eq!(a.mic_muted, Some(false));

    // an event burst (plus an irrelevant stream event) yields one coalesced update
    std::fs::write(dir.join("sink"), "Volume: 0.55 [MUTED]\n").unwrap();
    let mut ev = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events"))
        .unwrap();
    use std::io::Write;
    writeln!(ev, "Event 'change' on sink-input #9").unwrap();
    writeln!(ev, "Event 'change' on sink #54").unwrap();
    writeln!(ev, "Event 'change' on sink #54").unwrap();
    let a = next(&rx).audio;
    assert_eq!((a.volume_pct, a.muted), (Some(55), Some(true)));

    std::fs::write(dir.join("source"), "Volume: 1.00 [MUTED]\n").unwrap();
    writeln!(ev, "Event 'change' on source #55").unwrap();
    assert_eq!(next(&rx).audio.mic_muted, Some(true));
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "no extra updates"
    );

    // writes shell out to wpctl with the right arguments
    audio::set_volume(&tools, 42).unwrap();
    audio::change_volume(&tools, -5).unwrap();
    audio::set_mic_mute(&tools, audio::Mute::Toggle).unwrap();
    let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
    assert!(
        calls.contains("set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 0.42"),
        "{calls}"
    );
    assert!(
        calls.contains("set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%-"),
        "{calls}"
    );
    assert!(
        calls.contains("set-mute @DEFAULT_AUDIO_SOURCE@ toggle"),
        "{calls}"
    );

    drop(rx); // the audio thread notices on its next event and stops its child
    writeln!(ev, "Event 'change' on sink #54").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_tools_give_unknown_not_errors() {
    let tools = Tools {
        wpctl: "/nonexistent/wpctl".into(),
        pactl: "/nonexistent/pactl".into(),
    };
    assert_eq!(audio::read(&tools), audio::Audio::default());
    assert!(audio::set_volume(&tools, 10).is_err());
}
