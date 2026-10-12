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
use sliver_agent::api::{ApiLink, Reply};
use sliver_agent::icons::Icons;
use sliver_agent::link::{Event, Link};
use sliver_agent::modules::World;
use sliver_agent::ops::{Op, start_ops};
use sliver_agent::published::Published;
use sliver_api::registry::Focus;
use sliver_api::service::{Control, ControlReply, ControlState};
use sliver_config::{Config, Loaded};
use sliver_feeds::{Aggregate, Feed, Feeds};
use sliver_ipc::proto::{DisplayState, Layer, SOCKET_PATH};
use sliver_ipc::transport::Conn;
use sliver_render::{Fonts, Renderer, Stylesheet};
use std::collections::HashMap;

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

/// The live world: sliver-feeds (desktop, session, user modules) plus the ops thread.
struct Live {
    feeds: Feeds,
    agg: Aggregate,
    outputs: HashMap<String, sliver_modules::Output>,
    /// logind's raw `LockedHint`, before the plausibility check.
    locked_hint: bool,
    ops: Option<std::sync::mpsc::Sender<Op>>,
    /// The public D-Bus API, if the session bus is there.
    api: Option<ApiLink>,
    published: Published,
}

impl Live {
    fn start(config: &Config) -> std::io::Result<Live> {
        // The API first: owning the bus name is what tells systemd (Type=dbus) and D-Bus
        // activation that the agent is up, so it must not wait on slow session sources.
        let api = match ApiLink::start() {
            Ok(a) => Some(a),
            Err(e) => {
                eprintln!("sliver-agent: public D-Bus API unavailable: {e}");
                None
            }
        };
        let mut feeds = Feeds::new()?;
        feeds.start_desktop(|k| std::env::var(k).ok());
        feeds.start_session();
        let (specs, errors) = sliver_agent::modules::user_module_specs(config);
        errors.iter().for_each(|e| eprintln!("sliver-agent: {e}"));
        feeds.start_modules(specs);
        let ops = feeds.session.clone().and_then(|h| match start_ops(h) {
            Ok(tx) => Some(tx),
            Err(e) => {
                eprintln!("sliver-agent: session controls unavailable: {e}");
                None
            }
        });
        Ok(Live {
            feeds,
            agg: Aggregate::from_env(),
            outputs: HashMap::new(),
            locked_hint: false,
            ops,
            api,
            published: Published::default(),
        })
    }

    /// Takes in everything the feeds sent. Returns whether anything did.
    fn pump(&mut self) -> bool {
        let feeds = self.feeds.drain();
        let any = !feeds.is_empty();
        for f in feeds {
            match &f {
                Feed::Module(u) => {
                    self.outputs.insert(u.module.clone(), u.output.clone());
                }
                Feed::Session(s) => self.locked_hint = s.locked,
                Feed::Note(n) => eprintln!("sliver-agent: {n}"),
                Feed::Desktop(_) => {}
            }
            self.agg.apply(f);
        }
        if any {
            self.push_focus();
        }
        any
    }

    /// Our compositor backend's focus decides who fills App controls (GNOME/KDE push theirs
    /// over `Desktop1` instead).
    fn push_focus(&mut self) {
        let d = &self.agg.desktop;
        if let Some(api) = &self.api
            && d.connected
        {
            api.set_focus(Focus {
                window: d.window.clone(),
                workspace: d.workspace.as_ref().map(|w| w.name.clone()),
                locked: self.world_locked(),
            });
            self.published = Published::snapshot(&api.api);
        }
    }

    fn world_locked(&self) -> bool {
        use sliver_agent::context::{plausibly_locked, process_names};
        plausibly_locked(self.locked_hint, self.agg.desktop.backend, process_names)
    }

    fn world(&self) -> World {
        // logind's LockedHint can be stale; see context::plausibly_locked.
        let locked = self.world_locked();
        let mut session = self.agg.session.clone();
        session.locked = locked;
        let mut context = self.agg.context();
        context.set("locked", locked);
        World {
            now: Local::now(),
            session,
            desktop: self.agg.desktop.clone(),
            context,
            outputs: self.outputs.clone(),
            published: self.published.clone(),
            preview: false,
        }
    }

    fn op(&self, op: Op) {
        if let Some(tx) = &self.ops {
            let _ = tx.send(op);
        }
    }

    fn modules(&self) -> Option<&sliver_modules::Runtime> {
        self.feeds.modules.as_ref()
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
        let world = match Live::start(&agent.config) {
            Ok(mut live) => {
                std::thread::sleep(Duration::from_millis(1500));
                live.pump();
                live.world()
            }
            Err(_) => World::at(Local::now()),
        };
        agent.render(&world);
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

    let mut live = Live::start(&config).unwrap_or_else(|e| die(&format!("starting feeds: {e}")));
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
                if let Err(e) = run(agent, link, &mut live) {
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
                    SessionOp::ToggleCaffeine => Op::Caffeine(!live.agg.session.caffeine),
                    SessionOp::Media(cmd) => match &live.agg.session.media {
                        Some(m) => Op::Media {
                            player: m.player.clone(),
                            cmd,
                        },
                        None => continue,
                    },
                };
                live.op(op);
            }
            Effect::ModuleFired(key) => {
                if let Some(rt) = live.modules() {
                    rt.action_fired(&key);
                }
            }
            Effect::Signal(n) => {
                if let Some(rt) = live.modules() {
                    rt.signal(n.min(u8::MAX as u32) as u8);
                }
            }
            Effect::Update(key) => {
                if !live.modules().is_some_and(|rt| rt.trigger(&key)) {
                    eprintln!("sliver-agent: update: no user module {key:?}");
                }
            }
            Effect::Api {
                context,
                item,
                what,
            } => {
                if let Some(api) = &live.api {
                    api.interact(context, &item, what);
                }
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

/// A `Control1` request from D-Bus (sliverctl and friends), answered by the core.
fn control(agent: &mut Agent, live: &mut Live, req: Control) -> (Reply, Vec<Effect>) {
    use sliver_config::{Action, LayerMode};
    let layer = |name: String, mode| Action::Layer { name, mode };
    let mut fx = Vec::new();
    let done = Ok(ControlReply::Done);
    let reply = match req {
        Control::SwitchLayer(name) | Control::PushLayer(name, _) => {
            agent.run_action(&layer(name, LayerMode::Toggle), now_us(), &mut fx);
            if !fx.contains(&Effect::Redraw) {
                Err("no such layer".into())
            } else {
                done
            }
        }
        Control::ToggleLayer(name) => {
            agent.run_action(&layer(name, LayerMode::Toggle), now_us(), &mut fx);
            done
        }
        Control::PopLayer => {
            agent.run_action(&Action::CloseLayer, now_us(), &mut fx);
            done
        }
        Control::Signal(n) => {
            fx.push(Effect::Signal(n));
            done
        }
        Control::Refresh(m) => match live.modules().map(|rt| rt.trigger(&m)) {
            Some(true) => done,
            _ => Err(format!("no user module {m:?}")),
        },
        Control::Trigger(module, g) => {
            agent.trigger(&module, g, now_us(), &mut fx);
            done
        }
        Control::Bar(what) => {
            agent.hidden = match what.as_str() {
                "show" => false,
                "hide" => true,
                _ => !agent.hidden,
            };
            fx.push(Effect::Redraw);
            done
        }
        Control::SetVariable(k, v) => {
            agent.ctx_overrides.retain(|(x, _)| *x != k);
            let value = match v {
                sliver_api::item::Prop::Bool(b) => Some(sliver_config::CtxValue::Bool(b)),
                sliver_api::item::Prop::Str(s) => Some(sliver_config::CtxValue::Str(s)),
                sliver_api::item::Prop::Num(n) => Some(sliver_config::CtxValue::Num(n)),
                sliver_api::item::Prop::Int(n) => Some(sliver_config::CtxValue::Num(n as f64)),
                _ => None,
            };
            if let Some(v) = value {
                agent.ctx_overrides.push((k, v));
            }
            fx.push(Effect::Redraw);
            done
        }
        Control::Preview(name, _) | Control::RenderPreview(name, _) => {
            let mut frames = Vec::new();
            let saved = std::mem::replace(&mut agent.layers, vec![name.clone()]);
            agent.render(&live.world());
            if let Some(f) = agent.frame(Layer::Base) {
                frames.push(f.image.encode_png());
            }
            agent.layers = saved;
            fx.push(Effect::Redraw);
            match frames.pop() {
                Some(Ok(png)) => Ok(ControlReply::Png(png)),
                _ => Err(format!("can't render layer {name:?}")),
            }
        }
        Control::ValidateConfig(path) => {
            let l = sliver_config::load_file(Path::new(&path));
            Ok(ControlReply::Errors(
                l.diagnostics.errors.iter().map(|e| e.to_string()).collect(),
            ))
        }
        Control::ListModules => Ok(ControlReply::Modules(agent.list_modules())),
        other => Err(format!("not supported yet: {other:?}")),
    };
    (reply, fx)
}

/// Drives one sliverd connection until it closes.
fn run(mut agent: Agent, mut link: Link, live: &mut Live) -> std::io::Result<()> {
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
            if let Some(api) = &mut live.api {
                let drawn = [Layer::Base, Layer::Fn]
                    .iter()
                    .filter_map(|l| agent.frame(*l))
                    .flat_map(|f| {
                        f.cells
                            .iter()
                            .filter_map(|c| c.published.as_ref().map(|p| p.context))
                    })
                    .collect();
                api.set_on_screen(drawn);
                api.set_control_state(ControlState {
                    active_layer: agent.base_layer().to_owned(),
                    layers: agent.config.layers.keys().cloned().collect(),
                    active_profile: "default".into(),
                    profiles: vec!["default".into()],
                    variables: Default::default(),
                    healthy: agent.errors.is_empty(),
                });
            }
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
        let (readable, world_changed, api_woke) = {
            let (fd, wake) = (link.fd(), live.feeds.as_fd());
            let api_fd = live.api.as_ref().map(|a| a.fd());
            let mut fds = vec![
                PollFd::new(&fd, PollFlags::IN),
                PollFd::new(&wake, PollFlags::IN),
            ];
            if let Some(a) = &api_fd {
                fds.push(PollFd::new(a, PollFlags::IN));
            }
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
            let ready = |i: usize| fds.get(i).is_some_and(|f| !f.revents().is_empty());
            (ready(0), ready(1), ready(2))
        };
        if world_changed {
            dirty |= live.pump();
        }
        if api_woke && let Some(api) = &live.api {
            let (changed, requests) = api.drain();
            if changed {
                live.published = Published::snapshot(&api.api);
                dirty = true;
            }
            for (req, reply) in requests {
                let (answer, fx) = control(&mut agent, live, req);
                let _ = reply.send(answer);
                dirty |= apply(fx, &link, &mut children, live)?;
            }
        }
        let world_changed = world_changed || api_woke;
        let fx = agent.tick(now_us());
        dirty |= apply(fx, &link, &mut children, live)?;
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
                    if let Some(api) = &mut live.api {
                        api.set_display(match state {
                            DisplayState::On => "on",
                            DisplayState::Dimmed => "dimmed",
                            DisplayState::Off => "off",
                        });
                    }
                    let was_off = agent.display == DisplayState::Off;
                    agent.display = state;
                    dirty |= was_off && state != DisplayState::Off;
                    // User module timers stop while the bar is off (invariant 3).
                    if let Some(rt) = live.modules() {
                        rt.set_active(state != DisplayState::Off);
                    }
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
            dirty |= apply(fx, &link, &mut children, live)?;
        }
    }
}
