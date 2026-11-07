//! `sliver-agent`: the per-user session agent.
//!
//! Its command line is [`sliver_agent::CLI`].
//!
//! The config, its includes and stylesheets reload on save (inotify), on
//! `sliverctl reload` and on SIGUSR2. A config with errors is never applied: the last good one
//! stays and the errors are logged and reported (`healthy = false`).

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Local;
use rustix::event::{PollFd, PollFlags, poll};
use sliver_agent::Agent;
use sliver_agent::agent::{Effect, SessionOp};
use sliver_agent::api::{ApiLink, Reply};
use sliver_agent::driver::{Driver, Input, Now, Output};
use sliver_agent::feeds::{Aggregate, Feed, Feeds};
use sliver_agent::icons::Icons;
use sliver_agent::link::{Event, Link};
use sliver_agent::modules::World;
use sliver_agent::ops::{Op, start_ops};
use sliver_agent::published::Published;
use sliver_agent::reload::Applied;
use sliver_api::registry::Focus;
use sliver_api::service::{Control, ControlReply, ControlState};
use sliver_config::Config;
use sliver_ipc::proto::{AgentMsg, DisplayState, Layer, Role, SOCKET_PATH};
use sliver_ipc::transport::Conn;
use sliver_render::{Fonts, Renderer};
use std::collections::HashMap;

mod control;
mod effects;
mod live;
mod reloader;

use control::*;
use effects::*;
use live::*;
use reloader::*;

struct Args {
    render: Option<PathBuf>,
    config: Option<PathBuf>,
    socket: PathBuf,
    size: (u32, u32),
    /// Take the bus name from a running agent (it gets it back when this one exits).
    replace: bool,
}

fn args() -> Args {
    let cli = &sliver_agent::CLI;
    let args = cli.args(std::env::args().skip(1));
    let size = args.value("--size").map_or((2008, 60), |v| {
        let wh = v
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
        wh.unwrap_or_else(|| cli.die("--size is WxH, say 2008x60"))
    });
    let a = Args {
        render: args.value("--render").map(PathBuf::from),
        config: args.value("--config").map(PathBuf::from),
        socket: args
            .value("--socket")
            .or_else(|| std::env::var("SLIVER_SOCKET").ok())
            .map_or_else(|| SOCKET_PATH.into(), PathBuf::from),
        size,
        replace: args.flag("--replace"),
    };
    if let Some(x) = args.operands().first() {
        die(&format!("unexpected argument {x:?} (try --help)"));
    }
    a
}

fn die(msg: &str) -> ! {
    eprintln!("sliver-agent: {msg}");
    std::process::exit(2)
}

/// An agent showing `a` on a `size` bar. Besides the live one, separate agents draw a config
/// without touching its frames, transitions and touch state (`--render`, `RenderPreview`).
fn new_agent(a: &Applied, size: (u32, u32)) -> Agent {
    let mut agent = Agent::new(
        a.config.clone(),
        a.sheet.clone(),
        Renderer::new(Fonts::system()),
        Icons::system(),
        size.0,
        size.1,
    );
    agent.apply(a);
    agent
}

fn main() {
    let a = args();
    let mut reloader = Reloader::start(a.config.clone());
    if let Some(out) = a.render {
        let mut agent = new_agent(reloader.applied(), a.size);
        // Give the live sources a moment to report, so the preview shows real state.
        // No API: a headless render must never take the bus name from the running agent.
        let world = match Live::start(agent.config(), None) {
            Ok(mut live) => {
                std::thread::sleep(Duration::from_millis(1500));
                live.pump();
                live.world(Now::read())
            }
            Err(_) => World::at(Local::now()),
        };
        agent.render(&world);
        agent
            .errors()
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
                    let content = match &c.look.content {
                        C::Label { icon, text } => match (icon.is_some(), text.is_empty()) {
                            (false, false) => text.clone(),
                            (true, false) => format!("[icon] {text}"),
                            (true, true) => "[icon]".into(),
                            (false, true) => "[blank]".into(),
                        },
                        C::Slider { value, .. } => format!("[slider {:.0}%]", value * 100.0),
                        C::Spacer => "[spacer]".into(),
                        _ => "[blank]".into(),
                    };
                    let classes: String = c.look.classes.iter().map(|k| format!(".{k}")).collect();
                    println!(
                        "  {:>5}+{:<4} {:<34} #{}{classes}  {content}",
                        r.x,
                        r.w,
                        c.key,
                        c.look.id.as_deref().unwrap_or_default()
                    );
                }
            }
        }
        return;
    }

    let mut live = Live::start(&reloader.applied().config, Some(a.replace))
        .unwrap_or_else(|e| die(&format!("starting feeds: {e}")));
    let mut backoff = Duration::from_millis(250);
    // Log the first failure and the recovery, not every retry (tiny-dfr may own the bar for days).
    let mut failing = false;
    loop {
        // Replaced by a `--replace` agent: stay off sliverd until the bus hands the name back.
        if let Some(api) = &live.api
            && !api.owns_name()
        {
            wait_for_name(api);
            live.published_stale = true;
            eprintln!("sliver-agent: the bus name is back; taking the bar again");
        }
        match Conn::connect(&a.socket)
            .and_then(|c| Link::handshake(c, concat!("sliver-agent ", env!("CARGO_PKG_VERSION"))))
        {
            Ok(link) => {
                backoff = Duration::from_millis(250);
                if std::mem::take(&mut failing) {
                    eprintln!("sliver-agent: sliverd is back");
                }
                let size = (link.panel.width, link.panel.height);
                let mut agent = new_agent(reloader.applied(), size);
                agent.set_emoji(live.emoji.clone());
                agent.set_reduce_motion(reduce_motion(&live));
                eprintln!(
                    "sliver-agent: connected to sliverd ({}, {}x{}, {:?})",
                    link.panel.model, link.panel.width, link.panel.height, link.role
                );
                if let Err(e) = run(agent, link, &mut live, &mut reloader) {
                    eprintln!("sliver-agent: {e}");
                }
                if live.api.as_ref().is_none_or(ApiLink::owns_name) {
                    eprintln!("sliver-agent: lost sliverd, reconnecting");
                }
                // Not driving the bar until we're back (sliverd shows its safe layer).
                if let Some(api) = &live.api {
                    api.set_disconnected();
                }
            }
            Err(e) => {
                if !failing {
                    eprintln!(
                        "sliver-agent: {}: {e}; waiting for sliverd (quietly until it's back)",
                        a.socket.display()
                    );
                    failing = true;
                }
                if e.kind() == std::io::ErrorKind::NotFound {
                    // No socket at all (tiny-dfr drives the bar): sleep on inotify until it
                    // appears.
                    sliver_agent::wait::wait_for_path(&a.socket, None);
                    backoff = Duration::from_millis(250);
                    continue;
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// Sleeps until we own the bus name again, answering `Control1` calls meanwhile (the
/// `--replace` agent drives the bar, not us).
fn wait_for_name(api: &ApiLink) {
    eprintln!("sliver-agent: another sliver-agent took over (--replace); waiting until it exits");
    while !api.owns_name() {
        let fd = api.fd();
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let _ = poll(&mut fds, None);
        let (_, requests) = api.drain();
        for (_, reply) in requests {
            let _ = reply.send(Err(
                "another sliver-agent (--replace) drives the bar right now".into(),
            ));
        }
    }
}

fn run(
    agent: Agent,
    mut link: Link,
    live: &mut Live,
    reloader: &mut Reloader,
) -> std::io::Result<()> {
    let mut driver = Driver::new(agent, link.panel.max_brightness);
    let mut children = Vec::new();
    let mut inputs = vec![Input::OnBattery(live.agg.session.power.on_battery)];
    loop {
        let now = Now::read();
        for input in inputs.drain(..) {
            let out = driver.handle(&now, input);
            perform(out, &driver, &mut link, &mut children, live)?;
        }
        let out = driver.draw(&now, || live.world(now));
        perform(out, &driver, &mut link, &mut children, live)?;
        // Outside the draw: with the bar off nothing draws, and the state (layer, profile,
        // health) must still be right.
        if let Some(api) = &live.api {
            api.set_control_state(control_state(driver.agent(), reloader, link.role));
        }

        let timeout = driver
            .next_deadline(&now)
            .map(|d| d.as_millis() as i64 + 1)
            .into_iter()
            .chain(reloader.timeout_ms())
            .min()
            .map(|ms| rustix::event::Timespec {
                tv_sec: ms / 1000,
                tv_nsec: (ms % 1000) * 1_000_000,
            });
        let (readable, world_changed, api_woke, watch_ready, usr2_ready) = {
            use std::os::fd::AsFd;
            let (fd, wake) = (link.fd(), live.feeds.as_fd());
            let optional = [
                live.api.as_ref().map(|a| a.fd()),
                reloader.watch_fd(),
                reloader.usr2.as_ref().map(|r| r.as_fd()),
            ];
            // While a content change is held back by the pace, more can't make it sooner: only
            // touches, the config and the deadline wake us. Control calls wait one frame at most.
            let content = if driver.wants_content(&now) {
                PollFlags::IN
            } else {
                PollFlags::empty()
            };
            let mut fds = vec![PollFd::new(&fd, PollFlags::IN), PollFd::new(&wake, content)];
            let mut at = [None; 3];
            for (i, (slot, f)) in at.iter_mut().zip(&optional).enumerate() {
                if let Some(f) = f {
                    *slot = Some(fds.len());
                    fds.push(PollFd::new(f, if i == 0 { content } else { PollFlags::IN }));
                }
            }
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
            let ready = |i: Option<usize>| {
                i.and_then(|i| fds.get(i))
                    .is_some_and(|f| !f.revents().is_empty())
            };
            (
                ready(Some(0)),
                ready(Some(1)),
                ready(at[0]),
                ready(at[1]),
                ready(at[2]),
            )
        };

        inputs.extend(
            reloader
                .on_wake(watch_ready, usr2_ready, live)
                .map(Input::Config),
        );
        if world_changed {
            if live.pump() {
                inputs.push(Input::Changed);
            }
            inputs.extend(reloader.sync_accent(live).map(Input::Config));
            inputs.push(Input::ReduceMotion(reduce_motion(live)));
            inputs.push(Input::OnBattery(live.agg.session.power.on_battery));
        }
        if api_woke && let Some(api) = &live.api {
            let (changed, requests) = api.drain();
            if changed {
                live.published_stale = true;
                live.sync_pushed_desktop();
                inputs.push(Input::Changed);
            }
            for (req, reply) in requests {
                let _ = reply.send(control(&mut driver, live, reloader, req, &mut inputs));
            }
        }
        if api_woke && live.api.as_ref().is_some_and(|a| !a.owns_name()) {
            // A `--replace` agent took over: let go of sliverd (main waits for the name).
            return Ok(());
        }
        inputs.push(Input::Tick);
        if !readable {
            continue;
        }
        for ev in link.poll()? {
            inputs.push(match ev {
                Event::Closed => return Ok(()),
                Event::Touch {
                    region,
                    slot,
                    phase,
                    x,
                    time_us,
                    ..
                } => Input::Touch {
                    region,
                    slot,
                    phase,
                    x,
                    time_us,
                },
                Event::Display(state) => {
                    display_changed(live, driver.agent().display(), state);
                    Input::Display(state)
                }
                Event::Hatch(open) => Input::Hatch(open),
                Event::Fn(held) => Input::Fn(held),
                Event::Role(_) => Input::Role,
                Event::Error { code, msg } => {
                    eprintln!("sliver-agent: sliverd: {code:?}: {msg}");
                    continue;
                }
            });
        }
    }
}

/// The rest of the session follows the bar's display state: the API reports it, the feeds and
/// user module timers sleep while it's off, and turning on looks again at what
/// may have changed in the dark.
fn display_changed(live: &mut Live, was: DisplayState, state: DisplayState) {
    if let Some(api) = &mut live.api {
        api.set_display(match state {
            DisplayState::On => "on",
            DisplayState::Dimmed => "dimmed",
            DisplayState::Off => "off",
        });
    }
    if was == DisplayState::Off && state != DisplayState::Off {
        // A locker may have started while we were dark and nothing told us.
        live.lock_watch.wake();
        live.push_focus();
    }
    live.feeds.set_awake(state != DisplayState::Off);
    if let Some(rt) = live.modules() {
        rt.set_active(state != DisplayState::Off);
    }
}

fn perform(
    outputs: Vec<Output>,
    driver: &Driver,
    link: &mut Link,
    children: &mut Vec<Child>,
    live: &mut Live,
) -> std::io::Result<()> {
    let mut presented = false;
    for o in outputs {
        match o {
            Output::Present(layer) => {
                let f = driver.agent().frame(layer).expect("just drawn");
                link.present(layer, &f.image, &f.pressed, &f.regions)?;
                presented = true;
            }
            Output::Policy(p) => link.send(&AgentMsg::DisplayPolicy(p))?,
            Output::Effect(e) => apply(e, link, children, live)?,
        }
    }
    // Reap finished children.
    children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
    if presented && let Some(api) = &mut live.api {
        let drawn = [Layer::Base, Layer::Fn]
            .iter()
            .filter_map(|l| driver.agent().frame(*l))
            .flat_map(|f| {
                f.cells
                    .iter()
                    .filter_map(|c| c.published.as_ref().map(|p| p.context))
            })
            .collect();
        api.set_on_screen(drawn);
    }
    Ok(())
}
