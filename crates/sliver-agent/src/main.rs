//! `sliver-agent`: the per-user session agent.
//!
//! ```text
//! sliver-agent                      run: connect to sliverd and drive the bar
//! sliver-agent --render out         headless: write out-base.png and out-fn.png, no sliverd
//!   --config FILE   use this config instead of the search path (~/.config/sliver/config.toml)
//!   --socket PATH   sliverd's socket (default /run/sliver/sliverd.sock, or $SLIVER_SOCKET)
//!   --size WxH      bar size for --render (default 2008x60)
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Local;
use rustix::event::{PollFd, PollFlags, poll};
use sliver_agent::Agent;
use sliver_agent::icons::Icons;
use sliver_agent::link::{Event, Link};
use sliver_agent::modules::World;
use sliver_config::{Config, Loaded};
use sliver_ipc::proto::{DisplayState, Layer, SOCKET_PATH};
use sliver_ipc::transport::Conn;
use sliver_render::{Fonts, Renderer, Stylesheet};

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

fn world() -> World {
    World { now: Local::now() }
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
        agent.render(&world());
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
            }
        }
        return;
    }

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
                if let Err(e) = run(agent, link) {
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

/// Drives one sliverd connection until it closes.
fn run(mut agent: Agent, mut link: Link) -> std::io::Result<()> {
    let mut dirty = true;
    loop {
        if dirty && agent.display != DisplayState::Off {
            for layer in agent.render(&world()) {
                let f = agent.frame(layer).expect("just rendered");
                link.present(layer, &f.image, &f.pressed, &f.regions)?;
            }
            agent
                .errors
                .iter()
                .for_each(|e| eprintln!("sliver-agent: {e}"));
            dirty = false;
        }
        // Sleep until sliverd says something or a module's content changes on its own.
        let timeout = agent.next_change().map(|t| {
            let ms = (t - Local::now()).num_milliseconds().max(0) + 1;
            rustix::event::Timespec {
                tv_sec: ms / 1000,
                tv_nsec: (ms % 1000) * 1_000_000,
            }
        });
        let readable = {
            let fd = link.fd();
            let mut fds = [PollFd::new(&fd, PollFlags::IN)];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
            !fds[0].revents().is_empty()
        };
        if !readable {
            // Timed out: a module's content is due to change (an early wakeup just re-renders
            // the same pixels, which the link then drops).
            dirty = true;
            continue;
        }
        for ev in link.poll()? {
            match ev {
                Event::Closed => return Ok(()),
                Event::Display(state) => {
                    let was_off = agent.display == DisplayState::Off;
                    agent.display = state;
                    dirty |= was_off && state != DisplayState::Off;
                }
                Event::Hatch(active) => agent.hatch = active,
                Event::Role(_) => dirty = true,
                Event::Error { code, msg } => eprintln!("sliver-agent: sliverd: {code:?}: {msg}"),
                Event::Fn(_) | Event::Touch { .. } => {}
            }
        }
    }
}
