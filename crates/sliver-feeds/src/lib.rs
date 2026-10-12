//! `sliver-feeds`: everything the agent learns from outside, in one place.
//!
//! Three kinds of input run on their own threads and arrive on one channel:
//!
//! * the **desktop** (focused window, workspace, layout, outputs: `sliver-desktop`),
//! * the **session** (lock, power, media, audio, brightness, notifications, tray:
//!   `sliver-session`),
//! * **user modules** (`custom/*` output: `sliver-modules`).
//!
//! The agent's loop polls [`Feeds::as_fd`] together with its other descriptors; when it is
//! readable it calls [`Feeds::drain`], folds each [`Feed`] into an [`Aggregate`], and uses
//! [`Aggregate::context`] for rule matching and [`Aggregate::module_output`] for modules.
//! Nothing here polls on a timer: every thread sleeps in a blocking read.
//!
//! Every source degrades to "unknown" (a missing key in the context) instead of failing, and
//! reports problems as [`Feed::Note`] for the status page.

mod context;

pub use context::{add_desktop, add_session};

use sliver_config::Context;
use sliver_desktop::{DesktopSource, DesktopState};
use sliver_icons::SearchPaths;
use sliver_icons::desktop::DesktopEntries;
use sliver_modules::{Output, Runtime, Spec, Update};
use sliver_session::{Handle, SessionSource, SessionState};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// One thing that happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Feed {
    Desktop(DesktopState),
    Session(SessionState),
    Module(Update),
    /// A source could not start or stopped. For logs and the status page.
    Note(String),
}

#[derive(Clone)]
struct Waker(Arc<UnixStream>);

impl Waker {
    fn wake(&self) {
        // A full pipe already means "readable"; losing this byte is fine.
        let _ = (&*self.0).write(&[1]);
    }
}

#[derive(Clone)]
struct Out {
    tx: Sender<Feed>,
    waker: Waker,
}

impl Out {
    /// `false` = the receiver is gone, stop.
    fn send(&self, f: Feed) -> bool {
        let ok = self.tx.send(f).is_ok();
        self.waker.wake();
        ok
    }
}

/// The merged inputs. Dropping it stops the module runtime; source threads end the next time
/// they would send.
pub struct Feeds {
    rx: Receiver<Feed>,
    out: Out,
    wake_rx: UnixStream,
    /// Session actions (caffeine, brightness refresh) once [`Feeds::start_session`] ran.
    pub session: Option<Handle>,
    /// Module controls (`trigger`, `signal`, `set_active`) once [`Feeds::start_modules`] ran.
    pub modules: Option<Runtime>,
}

impl Feeds {
    pub fn new() -> std::io::Result<Feeds> {
        let (tx, rx) = channel();
        let (wake_rx, wake_tx) = UnixStream::pair()?;
        wake_rx.set_nonblocking(true)?;
        wake_tx.set_nonblocking(true)?;
        Ok(Feeds {
            rx,
            out: Out {
                tx,
                waker: Waker(Arc::new(wake_tx)),
            },
            wake_rx,
            session: None,
            modules: None,
        })
    }

    /// Readable when [`Feeds::drain`] has something. Poll it with the agent's other fds.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.wake_rx.as_fd()
    }

    /// Take everything that arrived since the last call (and reset the wake fd).
    pub fn drain(&mut self) -> Vec<Feed> {
        let mut buf = [0u8; 256];
        while matches!((&self.wake_rx).read(&mut buf), Ok(n) if n == buf.len()) {}
        self.rx.try_iter().collect()
    }

    /// Run a desktop source: its current state first, then every change; a final
    /// "disconnected" state when it ends.
    pub fn spawn_desktop(&self, mut src: impl DesktopSource + Send + 'static) {
        let out = self.out.clone();
        std::thread::Builder::new()
            .name("feed desktop".into())
            .spawn(move || {
                if !out.send(Feed::Desktop(src.state().clone())) {
                    return;
                }
                while let Ok(Some(s)) = src.next_change() {
                    if !out.send(Feed::Desktop(s)) {
                        return;
                    }
                }
                out.send(Feed::Desktop(DesktopState::default()));
            })
            .expect("spawn thread");
    }

    /// Run the compositor backend for this session (Hyprland, sway, niri). GNOME and KDE
    /// have no in-process backend: their extension pushes focus over `Desktop1.SetFocus`.
    pub fn start_desktop(&self, get: impl Fn(&str) -> Option<String> + Send + 'static) {
        let out = self.out.clone();
        std::thread::Builder::new()
            .name("feed desktop".into())
            .spawn(move || {
                let o = out.clone();
                let r = sliver_desktop::watch_auto(get, move |s| {
                    if o.send(Feed::Desktop(s.clone())) {
                        std::ops::ControlFlow::Continue(())
                    } else {
                        std::ops::ControlFlow::Break(())
                    }
                });
                if let Err(e) = r {
                    out.send(Feed::Note(format!("desktop backend: {e}")));
                }
            })
            .expect("spawn thread");
    }

    /// Forward a session source (the live one, or a scripted fake in tests).
    pub fn spawn_session(&self, mut src: impl SessionSource + Send + 'static) {
        let out = self.out.clone();
        std::thread::Builder::new()
            .name("feed session".into())
            .spawn(move || {
                while let Ok(Some(s)) = src.next_change() {
                    if !out.send(Feed::Session(s)) {
                        return;
                    }
                }
            })
            .expect("spawn thread");
    }

    /// Start the live session sources on the real buses: logind, UPower, MPRIS, audio,
    /// brightness, notifications and the tray. Each optional part that cannot start leaves a
    /// [`Feed::Note`] and its keys stay unknown.
    pub fn start_session(&mut self) {
        let (system, session) = match (
            zbus::blocking::Connection::system(),
            zbus::blocking::Connection::session(),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            (a, b) => {
                let why = a
                    .err()
                    .or(b.err())
                    .map(|e| e.to_string())
                    .unwrap_or_default();
                self.out
                    .send(Feed::Note(format!("session sources: no D-Bus ({why})")));
                return;
            }
        };
        let s = sliver_session::Session::start(system, session);
        let handle = s.handle();
        handle.start_audio(sliver_session::audio::Tools::default());
        handle.refresh_brightness(&sliver_session::brightness::Sysfs::default());
        match (
            zbus::blocking::Connection::session(),
            zbus::blocking::Connection::session(),
        ) {
            (Ok(m), Ok(q)) => {
                if let Err(e) = handle.start_notifications(m, q) {
                    self.out.send(Feed::Note(format!("notifications: {e}")));
                }
            }
            _ => {
                self.out.send(Feed::Note(
                    "notifications: no second session connection".into(),
                ));
            }
        }
        if let Ok(c) = zbus::blocking::Connection::session() {
            handle.start_tray(c);
        }
        self.session = Some(handle);
        self.spawn_session(s);
    }

    /// Start the user modules and forward their updates. Also routes `pkill -RTMIN+N`.
    pub fn start_modules(&mut self, specs: Vec<Spec>) {
        let (rt, rx) = Runtime::start(specs);
        if let Err(e) = sliver_modules::forward_realtime_signals(rt.clone()) {
            self.out.send(Feed::Note(format!("realtime signals: {e}")));
        }
        let out = self.out.clone();
        std::thread::Builder::new()
            .name("feed modules".into())
            .spawn(move || {
                while let Ok(u) = rx.recv() {
                    if !out.send(Feed::Module(u)) {
                        return;
                    }
                }
            })
            .expect("spawn thread");
        self.modules = Some(rt);
    }
}

/// What a [`Feed`] changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Changed {
    /// Rules may now match differently: re-resolve (and re-render).
    Context,
    /// This module's output changed: re-render.
    Module(String),
    Nothing,
}

/// The latest state of every feed.
pub struct Aggregate {
    pub desktop: DesktopState,
    pub session: SessionState,
    modules: HashMap<String, Output>,
    apps: Option<DesktopEntries>,
}

impl Aggregate {
    /// `apps` resolves the focused window's `.desktop` id (`desktop_id` rules, app icons).
    pub fn new(apps: Option<DesktopEntries>) -> Aggregate {
        Aggregate {
            desktop: DesktopState::default(),
            session: SessionState::default(),
            modules: HashMap::new(),
            apps,
        }
    }

    /// With the user's installed applications.
    pub fn from_env() -> Aggregate {
        let paths = SearchPaths::from_env(|k| std::env::var(k).ok());
        Aggregate::new(Some(DesktopEntries::from_paths(&paths)))
    }

    pub fn apps(&self) -> Option<&DesktopEntries> {
        self.apps.as_ref()
    }

    /// The rule context from the compositor and session. The agent adds `time`, `weekday`,
    /// `physical_esc` and anything else it knows.
    pub fn context(&self) -> Context {
        let mut c = Context::new();
        add_desktop(&mut c, &self.desktop, self.apps.as_ref());
        add_session(&mut c, &self.session);
        c
    }

    pub fn module_output(&self, key: &str) -> Option<&Output> {
        self.modules.get(key)
    }

    pub fn apply(&mut self, feed: Feed) -> Changed {
        match feed {
            Feed::Desktop(d) => {
                let before = self.context();
                self.desktop = d;
                if self.context() != before {
                    Changed::Context
                } else {
                    Changed::Nothing
                }
            }
            Feed::Session(s) => {
                let before = self.context();
                self.session = s;
                // Tray, notification counts and slider positions change often without
                // touching any rule key; modules still want a redraw for them.
                if self.context() != before {
                    Changed::Context
                } else {
                    Changed::Nothing
                }
            }
            Feed::Module(u) => {
                let changed = self.modules.get(&u.module) != Some(&u.output);
                self.modules.insert(u.module.clone(), u.output);
                if changed {
                    Changed::Module(u.module)
                } else {
                    Changed::Nothing
                }
            }
            Feed::Note(_) => Changed::Nothing,
        }
    }
}

#[cfg(test)]
mod tests;
