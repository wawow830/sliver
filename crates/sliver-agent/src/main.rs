//! `sliver-agent`: the per-user session agent.
//!
//! ```text
//! sliver-agent                      run: connect to sliverd and drive the bar
//! sliver-agent --render out         headless: write out-base.png and out-fn.png, no sliverd
//!   --config FILE   use this config instead of the search path (~/.config/sliver/config.toml)
//!   --socket PATH   sliverd's socket (default /run/sliver/sliverd.sock, or $SLIVER_SOCKET)
//!   --size WxH      bar size for --render (default 2008x60)
//! ```
//!
//! The config, its includes and stylesheets hot-reload on save (inotify, no polling), on
//! `sliverctl reload` and on SIGUSR2. A config with errors is never applied: the last good one
//! stays and the errors are logged and reported (`healthy = false`).

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
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
use sliver_api::registry::Focus;
use sliver_api::service::{Control, ControlReply, ControlState};
use sliver_config::Config;
use sliver_ipc::proto::{AgentMsg, DisplayState, Layer, Role, SOCKET_PATH};
use sliver_ipc::transport::Conn;
use sliver_render::{Fonts, Renderer, Stylesheet};
use std::collections::HashMap;

struct Args {
    render: Option<PathBuf>,
    config: Option<PathBuf>,
    socket: PathBuf,
    size: (u32, u32),
    /// Take the bus name from a running agent (it gets it back when this one exits).
    replace: bool,
}

fn args() -> Args {
    let mut a = Args {
        render: None,
        config: None,
        socket: std::env::var_os("SLIVER_SOCKET").map_or_else(|| SOCKET_PATH.into(), PathBuf::from),
        size: (2008, 60),
        replace: false,
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
                    "usage: sliver-agent [--render OUT] [--config FILE] [--socket PATH] [--size WxH] [--replace]"
                );
                std::process::exit(0);
            }
            "--render" => a.render = Some(val("--render").into()),
            "--config" => a.config = Some(val("--config").into()),
            "--socket" => a.socket = val("--socket").into(),
            "--replace" => a.replace = true,
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

/// The current config and stylesheet, hot-reloaded by [`sliver_agent::reload`] (sliver-config(5)):
/// file changes (inotify, debounced), SIGUSR2 and `Control1.ReloadConfig`. A config with errors is
/// never applied; the last good one stays and `errors` says why.
struct Reloader {
    inner: Option<sliver_agent::reload::Reloader>,
    config: Config,
    sheet: Stylesheet,
    /// Why the newest config on disk isn't live. Empty = it is.
    errors: Vec<String>,
    /// SIGUSR2 arrives here (signal-hook's self-pipe).
    usr2: Option<UnixStream>,
    /// The active profile and all profiles, read on (re)load rather than every frame. Empty with
    /// `--config FILE`.
    profile: (String, Vec<String>),
    /// The live config file (`None`: built-in defaults); relative icon paths start here.
    config_path: Option<PathBuf>,
}

impl Reloader {
    /// Config text checked and loaded as if saved over the live config (`sliver_agent::reload::load_text`).
    fn load_text(&self, text: &str) -> Result<sliver_agent::reload::Applied, Vec<String>> {
        match &self.inner {
            Some(r) => r.load_text(text),
            None => sliver_agent::reload::load_text(text, self.config_path.as_deref(), None),
        }
    }

    fn start(explicit: Option<PathBuf>) -> Reloader {
        let (inner, applied) = match sliver_agent::reload::Reloader::start(explicit.as_deref()) {
            Ok((r, a)) => (Some(r), a),
            Err(e) => {
                eprintln!(
                    "sliver-agent: no hot reload (inotify: {e}); using the built-in defaults"
                );
                let config = sliver_config::load_str("", Path::new(".")).config;
                let a = sliver_agent::reload::Applied {
                    config,
                    sheet: Stylesheet::sliver(),
                    config_path: None,
                    warnings: Vec::new(),
                    errors: vec![format!("hot reload unavailable: {e}")],
                };
                (None, a)
            }
        };
        let src = applied
            .config_path
            .as_ref()
            .map_or("built-in defaults".into(), |p| p.display().to_string());
        applied
            .warnings
            .iter()
            .for_each(|w| eprintln!("sliver-agent: {src}: warning: {w}"));
        applied
            .errors
            .iter()
            .for_each(|e| eprintln!("sliver-agent: {src}: error: {e}"));
        if !applied.errors.is_empty() {
            eprintln!("sliver-agent: using the built-in defaults until the config is fixed");
        }
        let usr2 = UnixStream::pair().ok().and_then(|(r, w)| {
            r.set_nonblocking(true).ok()?;
            signal_hook::low_level::pipe::register(signal_hook::consts::SIGUSR2, w)
                .inspect_err(|e| eprintln!("sliver-agent: SIGUSR2: {e}"))
                .ok()?;
            Some(r)
        });
        let mut me = Reloader {
            inner,
            config: applied.config,
            sheet: applied.sheet,
            errors: applied.errors,
            usr2,
            profile: Default::default(),
            config_path: applied.config_path,
        };
        me.read_profiles();
        me
    }

    fn read_profiles(&mut self) {
        self.profile = if self.explicit() {
            Default::default()
        } else {
            (
                sliver_config::profile::active(),
                sliver_config::profile::list(),
            )
        };
    }

    /// Running with `--config FILE` (no profiles).
    fn explicit(&self) -> bool {
        self.inner.as_ref().is_some_and(|r| r.explicit().is_some())
    }

    fn watch_fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
        self.inner
            .as_ref()
            .map(sliver_agent::reload::Reloader::as_fd)
    }

    /// How long poll may sleep before a debounced change is due.
    fn timeout_ms(&self) -> Option<i64> {
        let d = self.inner.as_ref()?.deadline()?;
        Some(d.saturating_duration_since(Instant::now()).as_millis() as i64 + 1)
    }

    /// After poll: feeds file events and SIGUSR2, reloads when due. Returns whether to redraw.
    fn on_wake(
        &mut self,
        watch_ready: bool,
        usr2_ready: bool,
        agent: &mut Agent,
        live: &mut Live,
    ) -> bool {
        let Some(inner) = &mut self.inner else {
            return false;
        };
        let now = Instant::now();
        if watch_ready {
            inner.on_readable(now);
        }
        let mut forced = false;
        if usr2_ready && let Some(r) = &mut self.usr2 {
            let mut buf = [0u8; 64];
            while matches!(r.read(&mut buf), Ok(n) if n > 0) {}
            forced = true;
        }
        let outcome = if forced {
            Some(inner.reload_now())
        } else {
            inner.poll_due(now)
        };
        outcome.is_some_and(|o| self.handle(o, Some(agent), live))
    }

    /// The desktop's accent colour changed (portal or KDE): rebuild the stylesheet with it.
    /// Returns whether something changed on screen.
    fn sync_accent(&mut self, agent: &mut Agent, live: &mut Live) -> bool {
        let accent = live
            .agg
            .session
            .appearance
            .accent
            .map(|[r, g, b]| sliver_render::Color::rgb(r, g, b));
        let Some(inner) = self.inner.as_mut() else {
            return false;
        };
        if !inner.set_accent(accent) {
            return false;
        }
        let outcome = inner.reload_now();
        self.handle(outcome, Some(agent), live)
    }

    /// Reloads now (`sliverctl reload`). Returns the errors (empty = the file's config is live).
    fn reload(&mut self, agent: Option<&mut Agent>, live: &mut Live) -> Vec<String> {
        match self
            .inner
            .as_mut()
            .map(sliver_agent::reload::Reloader::reload_now)
        {
            Some(o) => {
                self.handle(o, agent, live);
            }
            None => self.errors = vec!["hot reload unavailable (no inotify)".into()],
        }
        self.errors.clone()
    }

    fn handle(
        &mut self,
        o: sliver_agent::reload::Outcome,
        agent: Option<&mut Agent>,
        live: &mut Live,
    ) -> bool {
        use sliver_agent::reload::Outcome;
        self.read_profiles();
        match o {
            Outcome::Applied(a) => {
                let src = a
                    .config_path
                    .as_ref()
                    .map_or("built-in defaults".into(), |p| p.display().to_string());
                a.warnings
                    .iter()
                    .for_each(|w| eprintln!("sliver-agent: {src}: warning: {w}"));
                live.restart_modules(&a.config);
                if let Some(api) = &live.api {
                    api.set_policy(sliver_agent::api::policy_from_config(&a.config));
                }
                if let Some(agent) = agent {
                    agent.config = a.config.clone();
                    agent.sheet = a.sheet.clone();
                    agent.set_config_dir(a.config_path.as_deref().and_then(Path::parent));
                    let layers = &agent.config.layers;
                    agent.layers.retain(|l| layers.contains_key(l));
                }
                self.config = a.config;
                self.sheet = a.sheet;
                self.config_path = a.config_path;
                self.errors.clear();
                eprintln!("sliver-agent: reloaded {src}");
                true
            }
            Outcome::Rejected(errors) => {
                errors
                    .iter()
                    .for_each(|e| eprintln!("sliver-agent: config error: {e}"));
                eprintln!("sliver-agent: keeping the last good config");
                self.errors = errors;
                false
            }
            // Back to what's live (e.g. a broken save reverted): nothing to redraw, no errors.
            Outcome::Unchanged => {
                self.errors.clear();
                false
            }
        }
    }
}

/// The live world: the feeds (desktop, session, user modules) plus the ops thread.
struct Live {
    feeds: Feeds,
    agg: Aggregate,
    outputs: HashMap<String, sliver_modules::Output>,
    /// logind's raw `LockedHint`, before the plausibility check.
    locked_hint: bool,
    lock_watch: sliver_agent::context::LockWatch,
    ops: Option<std::sync::mpsc::Sender<Op>>,
    /// The public D-Bus API, if the session bus is there.
    api: Option<ApiLink>,
    published: Published,
    /// The registry changed since `published` was copied: copy it before the next frame
    /// (not on every change, which an app may make hundreds of times a second).
    published_stale: bool,
    /// Our own compositor backend (Hyprland/sway/niri IPC) is connected. Without one, what a
    /// GNOME/KDE integration pushes over `Desktop1` is the desktop.
    own_desktop: bool,
    /// cpu/memory/... sampler, kept across frames for CPU deltas.
    stats: sliver_agent::modules::Stats,
    /// Installed applications (taskbar icons).
    apps: std::sync::Arc<sliver_icons::desktop::DesktopEntries>,
    /// Emoji recents, shared with the agent.
    emoji: sliver_agent::modules::EmojiHandle,
}

impl Live {
    /// `api`: serve the public API (`Some(replace)`), or not (`--render`).
    fn start(config: &Config, api: Option<bool>) -> std::io::Result<Live> {
        // The API first: owning the bus name is what tells systemd (Type=dbus) and D-Bus
        // activation that the agent is up, so it must not wait on slow session sources.
        let api = match api.map(ApiLink::start) {
            None => None,
            Some(Ok(a)) => Some(a),
            // Another agent drives the bar. Two would revoke each other on sliverd forever.
            Some(Err(zbus::Error::NameTaken)) => {
                eprintln!(
                    "sliver-agent: another sliver-agent is running (it owns {}). Stop it \
                     (systemctl --user stop sliver-agent), or pass --replace to take over \
                     until this one exits.",
                    sliver_ipc::dbus::BUS_NAME
                );
                std::process::exit(1);
            }
            Some(Err(e)) => {
                eprintln!("sliver-agent: public D-Bus API unavailable: {e}");
                None
            }
        };
        if let Some(api) = &api {
            api.set_policy(sliver_agent::api::policy_from_config(config));
        }
        let mut feeds = Feeds::new()?;
        feeds.start_desktop(|k| std::env::var(k).ok());
        feeds.start_session();
        let (specs, errors) = sliver_agent::modules::user_module_specs(config);
        errors.iter().for_each(|e| eprintln!("sliver-agent: {e}"));
        feeds.start_modules(specs);
        if sliver_agent::modules::uses_kind(config, "systemd-failed-units") {
            feeds.watch_failed_units();
        }
        if sliver_agent::modules::uses_kind(config, "privacy") {
            feeds.watch_capture();
        }
        if sliver_agent::modules::uses_kind(config, "language") {
            feeds.watch_input_method();
        }
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
            lock_watch: Default::default(),
            ops,
            api,
            published: Published::default(),
            published_stale: false,
            own_desktop: false,
            stats: Default::default(),
            emoji: sliver_agent::modules::EmojiHandle::persistent(),
            apps: std::sync::Arc::new(sliver_icons::desktop::DesktopEntries::from_paths(
                &sliver_icons::SearchPaths::from_env(|k| std::env::var(k).ok()),
            )),
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
                Feed::Desktop(d) => {
                    self.own_desktop = d.connected;
                    // Each window has its own input context: a new focus can mean another
                    // fcitx5 input method (a no-op unless the `language` module follows fcitx5).
                    let focus = |d: &sliver_desktop::DesktopState| {
                        d.window.as_ref().map(|w| w.address.clone())
                    };
                    if focus(d) != focus(&self.agg.desktop) {
                        self.feeds.refresh_input_method();
                    }
                }
                Feed::Note(n) => eprintln!("sliver-agent: {n}"),
                Feed::Art { .. } | Feed::Spectrum(_) => {}
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
            && self.own_desktop
            && d.connected
        {
            api.set_focus(Focus {
                window: d.window.clone(),
                workspace: d.workspace.as_ref().map(|w| w.name.clone()),
                locked: self.world_locked(),
            });
            self.published_stale = true;
        }
    }

    /// Without our own backend, adopt what a DE integration pushed over `Desktop1` (focus,
    /// workspaces, windows) as the desktop, so rules, the workspaces and taskbar modules and the
    /// `app_id` context work on GNOME/KDE too. Returns whether it changed.
    fn sync_pushed_desktop(&mut self) -> bool {
        if self.own_desktop {
            return false;
        }
        let Some(api) = &self.api else {
            return false;
        };
        let backend = sliver_desktop::Backend::detect(|k| std::env::var(k).ok());
        let pushed = sliver_api::service::pushed_desktop(&api.api.state(), backend);
        match pushed {
            Some(d) if d != self.agg.desktop => {
                self.agg.apply(Feed::Desktop(d));
                true
            }
            _ => false,
        }
    }

    fn world_locked(&self) -> bool {
        self.lock_watch.locked(
            self.locked_hint,
            self.agg.session.screensaver,
            self.agg.desktop.backend,
        )
    }

    /// The world for a frame. Copies the API registry first if it changed since the last copy.
    fn world(&mut self) -> World {
        if self.published_stale
            && let Some(api) = &self.api
        {
            self.published = Published::snapshot(&api.api);
            self.published_stale = false;
        }
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
            stats: self.stats.clone(),
            apps: self.apps.clone(),
            emoji: self.emoji.clone(),
            media_art: self.agg.media_art().map(std::path::Path::to_path_buf),
            spectrum: self.agg.spectrum.clone(),
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

    /// New user module specs after a reload. Unchanged specs keep the running modules.
    fn restart_modules(&mut self, config: &Config) {
        let (specs, errors) = sliver_agent::modules::user_module_specs(config);
        errors.iter().for_each(|e| eprintln!("sliver-agent: {e}"));
        self.feeds.start_modules(specs);
        if sliver_agent::modules::uses_kind(config, "systemd-failed-units") {
            self.feeds.watch_failed_units();
        }
        if sliver_agent::modules::uses_kind(config, "privacy") {
            self.feeds.watch_capture();
        }
        if sliver_agent::modules::uses_kind(config, "language") {
            self.feeds.watch_input_method();
        }
        if let Some(rt) = &self.feeds.modules {
            let keys: std::collections::HashSet<String> = rt.keys().into_iter().collect();
            self.outputs.retain(|k, _| keys.contains(k));
        }
    }
}

/// A separate agent that draws a config without touching the live one (its frames, transitions
/// and touch state): `--render` and `RenderPreview`.
fn offscreen_agent(
    config: Config,
    sheet: Stylesheet,
    config_path: Option<&Path>,
    size: (u32, u32),
) -> Agent {
    let mut agent = Agent::new(
        config,
        sheet,
        Renderer::new(Fonts::system()),
        Icons::system(),
        size.0,
        size.1,
    );
    agent.set_config_dir(config_path.and_then(Path::parent));
    agent
}

fn main() {
    let a = args();
    let mut reloader = Reloader::start(a.config.clone());
    let (config, sheet) = (reloader.config.clone(), reloader.sheet.clone());

    if let Some(out) = a.render {
        let mut agent = offscreen_agent(config, sheet, reloader.config_path.as_deref(), a.size);
        // Give the live sources a moment to report, so the preview shows real state.
        // No API: a headless render must never take the bus name from the running agent.
        let world = match Live::start(&agent.config, None) {
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

    let mut live = Live::start(&reloader.config, Some(a.replace))
        .unwrap_or_else(|e| die(&format!("starting feeds: {e}")));
    drop((config, sheet));
    let mut backoff = Duration::from_millis(250);
    // Log the first failure and the recovery, not every retry (tiny-dfr may own the bar for days).
    let mut failing = false;
    loop {
        // Replaced by a `--replace` agent: stay off sliverd (we'd revoke each other) until it
        // exits and the bus hands the name back.
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
                let mut agent = Agent::new(
                    reloader.config.clone(),
                    reloader.sheet.clone(),
                    Renderer::new(Fonts::system()),
                    Icons::system(),
                    link.panel.width,
                    link.panel.height,
                );
                agent.set_emoji(live.emoji.clone());
                agent.set_reduce_motion(reduce_motion(&live));
                agent.set_config_dir(reloader.config_path.as_deref().and_then(Path::parent));
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
                    // appears. Zero wakeups while idle (invariant 3).
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

/// Whether the desktop asked for no animations (unknown: animate).
fn reduce_motion(live: &Live) -> bool {
    live.agg.session.appearance.reduce_motion.unwrap_or(false)
}

/// The `systemd-run` prefix that puts a command in its own transient scope, so it outlives
/// the agent: under `sliver-agent.service` (KillMode=control-group) children in our cgroup die
/// with every agent restart. Desktop launchers do the same.
fn scope_prefix(argv: &[String], seq: u64) -> Vec<String> {
    let name: String = argv
        .first()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("cmd")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(32)
        .collect();
    vec![
        "systemd-run".into(),
        "--user".into(),
        "--scope".into(),
        "--collect".into(),
        "--quiet".into(),
        format!(
            "--unit=app-sliver-{name}-{}-{seq}.scope",
            std::process::id()
        ),
        "--".into(),
    ]
}

/// Starts a user command: in its own systemd scope when we run as a systemd service (falling
/// back to a plain spawn), always in its own process group with stdin closed.
fn spawn_detached(argv: &[String]) -> std::io::Result<Child> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let plain = |argv: &[String]| {
        let (prog, args) = argv
            .split_first()
            .ok_or_else(|| std::io::Error::other("empty command"))?;
        Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
    };
    if std::env::var_os("INVOCATION_ID").is_some() {
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut full = scope_prefix(argv, seq);
        full.extend(argv.iter().cloned());
        if let Ok(child) = plain(&full) {
            return Ok(child);
        }
    }
    plain(argv)
}

/// Carries out one of the agent's effects.
fn apply(e: Effect, link: &Link, children: &mut Vec<Child>, live: &Live) -> std::io::Result<()> {
    {
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
                    SessionOp::ToggleWifi => {
                        Op::Wifi(!live.agg.session.network.wifi_enabled.unwrap_or(true))
                    }
                    SessionOp::ToggleBluetooth => match &live.agg.session.bluetooth.adapter {
                        Some(a) => Op::Bluetooth {
                            adapter: a.clone(),
                            on: !live.agg.session.bluetooth.powered,
                        },
                        None => return Ok(()),
                    },
                    SessionOp::OpenNotification | SessionOp::DismissNotification => {
                        match live.agg.session.notifications.latest.as_ref().map(|l| l.id) {
                            Some(id) if op == SessionOp::OpenNotification => {
                                Op::OpenNotification(id)
                            }
                            Some(id) => Op::CloseNotification(id),
                            None => return Ok(()),
                        }
                    }
                    SessionOp::DBus(call) => Op::DBus(call),
                    SessionOp::InputMethod(id) => Op::InputMethod(id),
                    SessionOp::ToggleDark => {
                        Op::Dark(!live.agg.session.appearance.dark.unwrap_or(false))
                    }
                    SessionOp::ToggleDnd => {
                        Op::Dnd(!live.agg.session.notifications.dnd.unwrap_or(false))
                    }
                    SessionOp::MediaOn { player, cmd } => Op::Media { player, cmd },
                    SessionOp::Seek { player, pct } => {
                        let s = &live.agg.session;
                        let m = match &player {
                            Some(p) => s.players.iter().find(|m| &m.player == p),
                            None => s.media.as_ref(),
                        };
                        match m.and_then(|m| {
                            Some((m.player.clone(), m.track_id.clone()?, m.length_us?))
                        }) {
                            Some((player, track, len)) => Op::Seek {
                                player,
                                track,
                                pos_us: len * i64::from(pct) / 100,
                            },
                            None => return Ok(()),
                        }
                    }
                    SessionOp::Media(cmd) => match &live.agg.session.media {
                        Some(m) => Op::Media {
                            player: m.player.clone(),
                            cmd,
                        },
                        None => return Ok(()),
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
            Effect::Profile(name) => {
                // The reloader watches `active-profile`, so writing it is the switch.
                if let Err(e) = sliver_config::profile::set_active(&name) {
                    eprintln!("sliver-agent: profile: {e}");
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
            Effect::HeldPressed {
                module,
                zone,
                index,
            } => {
                if let Some(api) = &live.api {
                    api.held_pressed(&module, &zone, index);
                }
            }
            Effect::Desktop(action) => {
                // A compositor round trip: keep it off the main loop.
                std::thread::spawn(move || {
                    let backend = sliver_desktop::Backend::detect(|k| std::env::var(k).ok());
                    if let Some(call) = sliver_desktop::control::bus_call(backend, &action) {
                        if let Err(e) = sliver_agent::ops::bus_call(&call) {
                            eprintln!("sliver-agent: {action:?}: {e}");
                        }
                        return;
                    }
                    if let Err(e) = sliver_desktop::control::run(|k| std::env::var(k).ok(), &action)
                    {
                        eprintln!("sliver-agent: {action:?}: {e}");
                    }
                });
            }
            // The driver draws; these never reach here.
            Effect::Redraw | Effect::RedrawPress => {}
            Effect::EmitKeys(keys, mode) => link.emit_keys(&keys, mode)?,
            Effect::Log(msg) => eprintln!("sliver-agent: {msg}"),
            Effect::Spawn(argv) => match spawn_detached(&argv) {
                Ok(child) => children.push(child),
                Err(e) => eprintln!(
                    "sliver-agent: {}: {e}",
                    argv.first().map_or("", |s| s.as_str())
                ),
            },
        }
    }
    Ok(())
}

/// A `Control1` request from D-Bus (sliverctl and friends), answered by the core.
fn control(
    agent: &mut Agent,
    live: &mut Live,
    reloader: &mut Reloader,
    req: Control,
) -> (Reply, Vec<Effect>) {
    use sliver_config::{Action, LayerMode};
    let layer = |name: String, mode| Action::Layer { name, mode };
    let mut fx = Vec::new();
    let done = Ok(ControlReply::Done);
    let reply = match req {
        Control::SwitchLayer(name) | Control::PushLayer(name, _) => {
            agent.run_action(
                &layer(name, LayerMode::Toggle),
                Now::read().mono_us,
                &mut fx,
            );
            if !fx.contains(&Effect::Redraw) {
                Err("no such layer".into())
            } else {
                done
            }
        }
        Control::ToggleLayer(name) => {
            agent.run_action(
                &layer(name, LayerMode::Toggle),
                Now::read().mono_us,
                &mut fx,
            );
            done
        }
        Control::PopLayer => {
            agent.run_action(&Action::CloseLayer, Now::read().mono_us, &mut fx);
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
            agent.trigger(&module, g, Now::read().mono_us, &mut fx);
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
        Control::Preview(..) => {
            Err("Preview isn't implemented; RenderPreview draws a config".into())
        }
        // No config: what the bar shows now. Otherwise that config, drawn offscreen with the
        // live state.
        Control::RenderPreview(toml, _) if toml.is_empty() => agent
            .frame(if agent.fn_held {
                Layer::Fn
            } else {
                Layer::Base
            })
            .ok_or_else(|| "nothing drawn yet".to_string())
            .and_then(|f| f.image.encode_png().map_err(|e| e.to_string()))
            .map(ControlReply::Png),
        Control::RenderPreview(toml, _) => match reloader.load_text(&toml) {
            Ok(a) => {
                let size = (agent.width, agent.height);
                let mut p = offscreen_agent(a.config, a.sheet, a.config_path.as_deref(), size);
                p.render(&live.world());
                p.frame(Layer::Base)
                    .ok_or_else(|| "nothing to draw".to_string())
                    .and_then(|f| f.image.encode_png().map_err(|e| e.to_string()))
                    .map(ControlReply::Png)
            }
            Err(errors) => Err(errors.join("\n")),
        },
        Control::ValidateConfig(toml) => Ok(ControlReply::Errors(
            reloader.load_text(&toml).err().unwrap_or_default(),
        )),
        Control::ListModules => Ok(ControlReply::Modules(agent.list_modules())),
        Control::HoldLayout {
            layout,
            select_only,
        } => match agent.hold_layout(&layout, select_only) {
            Ok(()) => {
                fx.push(Effect::Redraw);
                done
            }
            Err(e) => Err(e),
        },
        Control::SetLayout(name) => match agent.set_layout(&name) {
            Ok(()) => {
                fx.push(Effect::Redraw);
                done
            }
            Err(e) => Err(e),
        },
        Control::Hide(m) | Control::Show(m) | Control::Toggle(m) if m.is_empty() => {
            Err("which module?".into())
        }
        Control::Hide(m) => {
            agent.hidden_modules.insert(m);
            fx.push(Effect::Redraw);
            done
        }
        Control::Show(m) => {
            agent.hidden_modules.remove(&m);
            fx.push(Effect::Redraw);
            done
        }
        Control::Toggle(m) => {
            if !agent.hidden_modules.remove(&m) {
                agent.hidden_modules.insert(m);
            }
            fx.push(Effect::Redraw);
            done
        }
        Control::ResetVisibility => {
            agent.hidden_modules.clear();
            fx.push(Effect::Redraw);
            done
        }
        Control::Update(module, props) => {
            use sliver_api::item::Prop;
            let out = agent.pushed.entry(module).or_default();
            for (k, v) in props {
                match (k.as_str(), v) {
                    ("text", Prop::Str(s)) => out.text = s,
                    ("icon", Prop::Str(s)) => out.icon = Some(s),
                    ("alt", Prop::Str(s)) => out.alt = Some(s),
                    ("class", Prop::Str(s)) => {
                        out.classes = s.split_whitespace().map(str::to_owned).collect()
                    }
                    ("percentage", Prop::Num(n)) => out.percentage = Some(n),
                    ("percentage", Prop::Int(n)) => out.percentage = Some(n as f64),
                    ("value", Prop::Num(n)) => out.value = Some(n),
                    ("value", Prop::Int(n)) => out.value = Some(n as f64),
                    ("hidden", Prop::Bool(b)) => out.hidden = b,
                    ("tooltip" | "detail", Prop::Str(s)) => out.detail = Some(s),
                    (k, _) => {
                        return (Err(format!("unknown or mistyped output field {k:?}")), fx);
                    }
                }
            }
            fx.push(Effect::Redraw);
            done
        }
        Control::SetProfile(name) => {
            if reloader.explicit() {
                Err("the agent runs with --config FILE, so profiles are off".into())
            } else {
                let before = sliver_config::profile::active();
                match sliver_config::profile::set_active(&name) {
                    Err(e) => Err(e),
                    Ok(()) => {
                        let errors = reloader.reload(Some(agent), live);
                        fx.push(Effect::Redraw);
                        if errors.is_empty() {
                            Ok(ControlReply::Done)
                        } else {
                            // Keep the profile that works, and say why the new one didn't.
                            let _ = sliver_config::profile::set_active(&before);
                            reloader.reload(Some(agent), live);
                            Err(format!(
                                "profile {name:?} not applied: {}",
                                errors.join("; ")
                            ))
                        }
                    }
                }
            }
        }
        Control::ReloadConfig => {
            let errors = reloader.reload(Some(agent), live);
            fx.push(Effect::Redraw);
            Ok(ControlReply::Errors(errors))
        }
    };
    (reply, fx)
}

/// Drives one sliverd connection until it closes.
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

/// The `Control1` properties while connected to sliverd (api.md §7). `Healthy`: connected and
/// driving the bar (sliverd's active agent, not standby for another seat user); problems with
/// the config, stylesheet or modules are `Errors`.
fn control_state(agent: &Agent, reloader: &Reloader, role: Role) -> ControlState {
    let (active_layout, rule, active_rule_name, source) = agent.active_layout().unwrap_or_default();
    ControlState {
        active_layer: agent.base_layer().to_owned(),
        active_layout,
        active_rule: rule.map_or(-1, |i| i as i32),
        active_rule_name,
        layout_source: source.to_owned(),
        layers: agent.config.layers.keys().cloned().collect(),
        active_profile: reloader.profile.0.clone(),
        profiles: reloader.profile.1.clone(),
        variables: Default::default(),
        healthy: role == Role::Active,
        errors: reloader
            .errors
            .iter()
            .chain(&agent.errors)
            .cloned()
            .collect(),
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
        let out = driver.draw(&now, || live.world());
        perform(out, &driver, &mut link, &mut children, live)?;
        // Not only after a draw: with the bar off nothing draws, and the state (layer, profile,
        // health) must still be right. Sent only when it changed.
        if let Some(api) = &live.api {
            api.set_control_state(control_state(driver.agent(), reloader, link.role));
        }

        // Sleep until sliverd, a feed, the API or the config says something, or something is due.
        // Nothing else wakes us (invariant 3).
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

        if reloader.on_wake(watch_ready, usr2_ready, driver.agent_mut(), live) {
            inputs.push(Input::Redraw);
        }
        if world_changed {
            if live.pump() {
                inputs.push(Input::Changed);
            }
            if reloader.sync_accent(driver.agent_mut(), live) {
                inputs.push(Input::Redraw);
            }
            driver.agent_mut().set_reduce_motion(reduce_motion(live));
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
                let (answer, fx) = control(driver.agent_mut(), live, reloader, req);
                let _ = reply.send(answer);
                inputs.push(Input::Effects(fx));
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
                    display_changed(live, driver.agent().display, state);
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
/// user module timers sleep while it's off (invariant 3), and turning on looks again at what
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

/// Carries out the driver's outputs.
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
            Output::Spectrum(config) => live.feeds.run_spectrum(config.as_deref()),
            Output::Effect(e) => apply(e, link, children, live)?,
        }
    }
    // Reap whatever finished, so no zombies pile up.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_unit_names_are_safe() {
        let p = scope_prefix(&["/usr/bin/fire fox".into(), "x".into()], 3);
        assert_eq!(
            &p[..5],
            ["systemd-run", "--user", "--scope", "--collect", "--quiet"]
        );
        assert_eq!(
            p[5],
            format!("--unit=app-sliver-fire_fox-{}-3.scope", std::process::id())
        );
        assert_eq!(p[6], "--");
    }
}
