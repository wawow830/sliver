//! `sliver-agent`: the per-user session agent.
//!
//! ```text
//! sliver-agent                      run: connect to sliverd and drive the bar
//! sliver-agent --render out         headless: write out-base.png and out-fn.png, no sliverd
//!   --config FILE   use this config instead of the search path (~/.config/sliver/config.toml)
//!   --socket PATH   sliverd's socket (default /run/sliver/sliverd.sock, or $SLIVER_SOCKET)
//!   --size WxH      bar size for --render (default 2008x60)
//! ```

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use chrono::Local;
use rustix::event::{PollFd, PollFlags, poll};
use sliver_agent::Agent;
use sliver_agent::agent::{Effect, SessionOp};
use sliver_agent::icons::Icons;
use sliver_agent::link::{Event, Link};
use sliver_agent::modules::World;
use sliver_agent::sources::{Msg, Op, Sources};
use sliver_config::{Config, Loaded};
use sliver_desktop::DesktopState;
use sliver_ipc::proto::{DisplayState, Layer, SOCKET_PATH};
use sliver_ipc::transport::Conn;
use sliver_render::{Fonts, Renderer, Stylesheet};
use sliver_session::SessionState;

struct Args {
    render: Option<PathBuf>,
    config: Option<PathBuf>,
    socket: PathBuf,
    size: (u32, u32),
}

fn args() -> Args {
    let mut a = Args {
        render: None,
        config: None,
        socket: std::env::var_os("SLIVER_SOCKET").map_or_else(|| SOCKET_PATH.into(), PathBuf::from),
        size: (2008, 60),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .unwrap_or_else(|| die(&format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--version" | "-V" => {
                println!("sliver-agent {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!(
                    "usage: sliver-agent [--render OUT] [--config FILE] [--socket PATH] [--size WxH]"
                );
                std::process::exit(0);
            }
            "--render" => a.render = Some(val("--render").into()),
            "--config" => a.config = Some(val("--config").into()),
            "--socket" => a.socket = val("--socket").into(),
            "--size" => {
                let v = val("--size");
                let (w, h) = v.split_once('x').unwrap_or_else(|| die("--size WxH"));
                a.size = (
                    w.parse().unwrap_or_else(|_| die("--size WxH")),
                    h.parse().unwrap_or_else(|_| die("--size WxH")),
                );
            }
            other => die(&format!("unknown argument {other:?} (try --help)")),
        }
    }
    a
}

fn die(msg: &str) -> ! {
    eprintln!("sliver-agent: {msg}");
    std::process::exit(2)
}

/// The config, or the built-in defaults if it has errors (they're printed either way).
fn load_config(path: Option<&Path>) -> (Config, Option<PathBuf>) {
    let found = path
        .map(Path::to_path_buf)
        .or_else(sliver_config::find_config);
    let loaded: Loaded = match &found {
        Some(p) => sliver_config::load_file(p),
        None => sliver_config::load(),
    };
    let src = found
        .as_ref()
        .map_or("built-in defaults".into(), |p| p.display().to_string());
    for w in &loaded.diagnostics.warnings {
        eprintln!("sliver-agent: {src}: warning: {w}");
    }
    for e in &loaded.diagnostics.errors {
        eprintln!("sliver-agent: {src}: error: {e}");
    }
    if loaded.diagnostics.ok() {
        (loaded.config, found)
    } else {
        eprintln!("sliver-agent: {src} has errors, using the built-in defaults");
        (sliver_config::load_str("", Path::new(".")).config, found)
    }
}

/// The default `sliver` theme, then the user's stylesheet (`stylesheet = …` in the config, or
/// `style.css` next to it) on top.
fn load_stylesheet(config: &Config, config_path: Option<&Path>) -> Stylesheet {
    let mut sheet = Stylesheet::sliver();
    let dir = config_path.and_then(Path::parent);
    let user = match (&config.stylesheet, dir) {
        (Some(s), Some(d)) => Some(d.join(s)),
        (Some(s), None) => Some(PathBuf::from(s)),
        (None, Some(d)) => Some(d.join("style.css")).filter(|p| p.is_file()),
        (None, None) => None,
    };
    if let Some(p) = user {
        let mut diags = Vec::new();
        match sheet.add_file(&p, &mut diags) {
            Ok(()) => diags.iter().for_each(|d| eprintln!("sliver-agent: {d}")),
            Err(e) => eprintln!("sliver-agent: {}: {e}", p.display()),
        }
    }
    sheet
}

/// What the sources told us so far.
#[derive(Default)]
struct Live {
    session: SessionState,
    desktop: DesktopState,
    /// logind's raw `LockedHint`, before plausibility checks.
    locked_hint: bool,
}

impl Live {
    fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Session(s) => {
                self.locked_hint = s.locked;
                self.session = s;
            }
            Msg::Desktop(d) => self.desktop = d,
        }
        // logind's LockedHint can be stale; see context::plausibly_locked.
        self.session.locked = sliver_agent::context::plausibly_locked(
            self.locked_hint,
            self.desktop.backend,
            sliver_agent::context::process_names,
        );
    }

    fn world(&self) -> World {
        World {
            now: Local::now(),
            session: self.session.clone(),
            desktop: self.desktop.clone(),
        }
    }
}

fn main() {
    let a = args();
    let (config, config_path) = load_config(a.config.as_deref());
    let sheet = load_stylesheet(&config, config_path.as_deref());

    if let Some(out) = a.render {
        let mut agent = Agent::new(
            config,
            sheet,
            Renderer::new(Fonts::system()),
            Icons::system(),
            a.size.0,
            a.size.1,
        );
        // Give the live sources a moment to report, so the preview shows real state.
        let mut live = Live::default();
        if let Ok(sources) = Sources::start() {
            std::thread::sleep(Duration::from_millis(1500));
            for msg in sources.drain() {
                live.update(msg);
            }
        }
        agent.render(&live.world());
        agent
            .errors
            .iter()
            .for_each(|e| eprintln!("sliver-agent: {e}"));
        for (layer, suffix) in [(Layer::Base, "base"), (Layer::Fn, "fn")] {
            if let Some(f) = agent.frame(layer) {
                let p = format!("{}-{suffix}.png", out.display());
                f.image
                    .save_png(&p)
                    .unwrap_or_else(|e| die(&format!("{p}: {e}")));
                println!("{p}");
                // What's on it, for config debugging.
                for (c, r) in f.cells.iter().zip(&f.layout.items) {
                    use sliver_render::Content as C;
                    let content = match &c.content {
                        C::Markup(t) | C::Text(t) => t.clone(),
                        C::IconMarkup(_, t) | C::IconText(_, t) => format!("[icon] {t}"),
                        C::Icon(_) => "[icon]".into(),
                        C::Slider { value, .. } => format!("[slider {:.0}%]", value * 100.0),
                        C::Spacer => "[spacer]".into(),
                        _ => "[blank]".into(),
                    };
                    let classes: String = c.classes.iter().map(|k| format!(".{k}")).collect();
                    println!(
                        "  {:>5}+{:<4} {:<34} #{}{classes}  {content}",
                        r.x, r.w, c.key, c.css_id
                    );
                }
            }
        }
        return;
    }

    let sources = Sources::start().unwrap_or_else(|e| die(&format!("starting sources: {e}")));
    let mut live = Live::default();
    let mut backoff = Duration::from_millis(250);
    loop {
        match Conn::connect(&a.socket)
            .and_then(|c| Link::handshake(c, concat!("sliver-agent ", env!("CARGO_PKG_VERSION"))))
        {
            Ok(link) => {
                backoff = Duration::from_millis(250);
                let agent = Agent::new(
                    config.clone(),
                    sheet.clone(),
                    Renderer::new(Fonts::system()),
                    Icons::system(),
                    link.panel.width,
                    link.panel.height,
                );
                eprintln!(
                    "sliver-agent: connected to sliverd ({}, {}x{}, {:?})",
                    link.panel.model, link.panel.width, link.panel.height, link.role
                );
                if let Err(e) = run(agent, link, &sources, &mut live) {
                    eprintln!("sliver-agent: {e}");
                }
                eprintln!("sliver-agent: lost sliverd, reconnecting");
            }
            Err(e) => eprintln!("sliver-agent: {}: {e}", a.socket.display()),
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

/// Monotonic clock in µs: the same clock sliverd stamps touches with (evdev, CLOCK_MONOTONIC).
fn now_us() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1_000
}

/// Carries out the agent's effects. Returns whether a redraw is needed.
fn apply(
    effects: Vec<Effect>,
    link: &Link,
    children: &mut Vec<Child>,
    sources: &Sources,
    live: &Live,
) -> std::io::Result<bool> {
    let mut redraw = false;
    for e in effects {
        match e {
            Effect::Session(op) => {
                let op = match op {
                    SessionOp::ScreenBrightness(pct) => Op::Brightness {
                        keyboard: false,
                        pct,
                    },
                    SessionOp::KeyboardBrightness(pct) => Op::Brightness {
                        keyboard: true,
                        pct,
                    },
                    SessionOp::Volume { mic, pct } => Op::Volume { mic, pct },
                    SessionOp::ToggleMute { mic } => Op::Mute { mic },
                    SessionOp::ToggleCaffeine => Op::Caffeine(!live.session.caffeine),
                    SessionOp::Media(cmd) => match &live.session.media {
                        Some(m) => Op::Media {
                            player: m.player.clone(),
                            cmd,
                        },
                        None => continue,
                    },
                };
                sources.op(op);
            }
            Effect::Redraw => redraw = true,
            Effect::EmitKeys(keys, mode) => link.emit_keys(&keys, mode)?,
            Effect::Log(msg) => eprintln!("sliver-agent: {msg}"),
            Effect::Spawn(argv) => {
                let Some((prog, args)) = argv.split_first() else {
                    continue;
                };
                // Own process group, so a signal to the agent doesn't take user commands down.
                match Command::new(prog)
                    .args(args)
                    .stdin(Stdio::null())
                    .process_group(0)
                    .spawn()
                {
                    Ok(child) => children.push(child),
                    Err(e) => eprintln!("sliver-agent: {prog}: {e}"),
                }
            }
        }
    }
    // Reap whatever finished, so no zombies pile up.
    children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
    Ok(redraw)
}

/// Drives one sliverd connection until it closes.
fn run(
    mut agent: Agent,
    mut link: Link,
    sources: &Sources,
    live: &mut Live,
) -> std::io::Result<()> {
    let mut dirty = true;
    let mut children = Vec::new();
    loop {
        if dirty && agent.display != DisplayState::Off {
            for layer in agent.render(&live.world()) {
                let f = agent.frame(layer).expect("just rendered");
                link.present(layer, &f.image, &f.pressed, &f.regions)?;
            }
            agent
                .errors
                .iter()
                .for_each(|e| eprintln!("sliver-agent: {e}"));
            dirty = false;
        }
        // Sleep until sliverd says something, a gesture/macro deadline, or a module's content
        // changes on its own. Nothing else wakes us (invariant 3).
        let change_ms = agent
            .next_change()
            .map(|t| (t - Local::now()).num_milliseconds().max(0) + 1);
        let deadline_ms = agent
            .next_deadline()
            .map(|t| (t.saturating_sub(now_us()) / 1000) as i64 + 1);
        let timeout =
            change_ms
                .into_iter()
                .chain(deadline_ms)
                .min()
                .map(|ms| rustix::event::Timespec {
                    tv_sec: ms / 1000,
                    tv_nsec: (ms % 1000) * 1_000_000,
                });
        let (readable, world_changed) = {
            let (fd, wake) = (link.fd(), sources.fd());
            let mut fds = [
                PollFd::new(&fd, PollFlags::IN),
                PollFd::new(&wake, PollFlags::IN),
            ];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
            (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
        };
        if world_changed {
            for msg in sources.drain() {
                live.update(msg);
                dirty = true;
            }
        }
        let fx = agent.tick(now_us());
        dirty |= apply(fx, &link, &mut children, sources, live)?;
        if world_changed && !readable {
            continue;
        }
        if !readable {
            // A module's content may be due to change. An early wakeup just re-renders the same
            // pixels, which the link then drops.
            dirty |= agent
                .next_change()
                .is_some_and(|t| t <= Local::now() + chrono::Duration::milliseconds(5));
            continue;
        }
        for ev in link.poll()? {
            let fx = match ev {
                Event::Closed => return Ok(()),
                Event::Touch {
                    region,
                    slot,
                    phase,
                    x,
                    time_us,
                    ..
                } => agent.touch(region, slot, phase, x, time_us),
                Event::Display(state) => {
                    let was_off = agent.display == DisplayState::Off;
                    agent.display = state;
                    dirty |= was_off && state != DisplayState::Off;
                    if state == DisplayState::Off {
                        agent.cancel_touches()
                    } else {
                        Vec::new()
                    }
                }
                Event::Hatch(active) => {
                    agent.hatch = active;
                    if active {
                        agent.cancel_touches()
                    } else {
                        Vec::new()
                    }
                }
                Event::Fn(held) => {
                    agent.fn_held = held;
                    Vec::new()
                }
                Event::Role(_) => {
                    dirty = true;
                    Vec::new()
                }
                Event::Error { code, msg } => {
                    eprintln!("sliver-agent: sliverd: {code:?}: {msg}");
                    Vec::new()
                }
            };
            dirty |= apply(fx, &link, &mut children, sources, live)?;
        }
    }
}
