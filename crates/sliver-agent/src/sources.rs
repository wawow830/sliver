//! The live world: session (logind, UPower, MPRIS, audio, notifications, brightness) and desktop
//! (compositor) sources on their own threads, plus an ops thread that carries out session
//! changes. Everything reaches the main loop through one channel and a wake pipe it polls, so
//! the loop sleeps until something actually changed (invariant 3).

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use sliver_desktop::DesktopState;
use sliver_session::mpris::Command;
use sliver_session::{SessionSource, SessionState, audio, brightness};
use zbus::blocking::Connection;

pub enum Msg {
    Session(SessionState),
    Desktop(DesktopState),
}

/// A session change for the ops thread. Built by the main loop, which knows the current state
/// (which player, whether caffeine is on).
#[derive(Debug, Clone)]
pub enum Op {
    Brightness { keyboard: bool, pct: u8 },
    Volume { mic: bool, pct: u8 },
    Mute { mic: bool },
    Media { player: String, cmd: Command },
    Caffeine(bool),
}

#[derive(Clone)]
struct Waker(Arc<OwnedFd>);

impl Waker {
    fn wake(&self) {
        // A full pipe already means "wake up".
        let _ = rustix::io::write(&*self.0, &[1]);
    }
}

pub struct Sources {
    rx: Receiver<Msg>,
    wake_rx: OwnedFd,
    ops: Sender<Op>,
}

impl Sources {
    /// Starts every source that's available. Missing ones (no compositor IPC, no system bus)
    /// are logged and leave their part of the world at "unknown".
    pub fn start() -> io::Result<Sources> {
        let (wake_rx, wake_tx) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )?;
        let waker = Waker(Arc::new(wake_tx));
        let (tx, rx) = channel();
        let (ops_tx, ops_rx) = channel::<Op>();

        let (t, w) = (tx.clone(), waker.clone());
        thread::Builder::new()
            .name("agent-desktop".into())
            .spawn(move || {
                let r = sliver_desktop::watch_auto(
                    |k| std::env::var(k).ok(),
                    |s| {
                        if t.send(Msg::Desktop(s.clone())).is_err() {
                            return std::ops::ControlFlow::Break(());
                        }
                        w.wake();
                        std::ops::ControlFlow::Continue(())
                    },
                );
                if let Err(e) = r {
                    eprintln!("sliver-agent: no compositor context: {e}");
                }
            })?;

        let (t, w) = (tx, waker);
        thread::Builder::new()
            .name("agent-session".into())
            .spawn(move || {
                let (sys, ses) = match (Connection::system(), Connection::session()) {
                    (Ok(a), Ok(b)) => (a, b),
                    (a, b) => {
                        eprintln!(
                            "sliver-agent: no session context: {:?} {:?}",
                            a.err(),
                            b.err()
                        );
                        return;
                    }
                };
                let mut session = sliver_session::Session::start(sys.clone(), ses.clone());
                let handle = session.handle();
                handle.start_audio(audio::Tools::default());
                if let (Ok(m), Ok(q)) = (Connection::session(), Connection::session())
                    && let Err(e) = handle.start_notifications(m, q)
                {
                    eprintln!("sliver-agent: notifications: {e}");
                }
                let sysfs = brightness::Sysfs::default();
                handle.refresh_brightness(&sysfs);
                let ops_handle = handle.clone();
                let _ = thread::Builder::new()
                    .name("agent-ops".into())
                    .spawn(move || run_ops(ops_rx, ops_handle, sys, ses, sysfs));
                while let Ok(Some(s)) = session.next_change() {
                    if t.send(Msg::Session(s)).is_err() {
                        return;
                    }
                    w.wake();
                }
            })?;

        Ok(Sources {
            rx,
            wake_rx,
            ops: ops_tx,
        })
    }

    /// Readable when [`Sources::drain`] has something.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.wake_rx.as_fd()
    }

    /// Everything that arrived since the last call.
    pub fn drain(&self) -> Vec<Msg> {
        let mut buf = [0u8; 64];
        while matches!(rustix::io::read(&self.wake_rx, &mut buf), Ok(n) if n > 0) {}
        self.rx.try_iter().collect()
    }

    pub fn op(&self, op: Op) {
        let _ = self.ops.send(op);
    }
}

fn run_ops(
    rx: Receiver<Op>,
    handle: sliver_session::Handle,
    sys: Connection,
    ses: Connection,
    sysfs: brightness::Sysfs,
) {
    let tools = audio::Tools::default();
    let (screen, keyboard) = (sysfs.screen(), sysfs.keyboard());
    for op in rx {
        let r: Result<(), String> = match op {
            Op::Brightness { keyboard: kbd, pct } => match if kbd { &keyboard } else { &screen } {
                Some(d) => {
                    // Keep the main screen visible: never all the way off from a slider.
                    let pct = if kbd { pct } else { pct.max(1) };
                    let r = brightness::set(&sys, d, pct).map_err(|e| e.to_string());
                    // sysfs has no change events: read back after our own write.
                    handle.refresh_brightness(&sysfs);
                    r
                }
                None => Err("no such backlight".into()),
            },
            Op::Volume { mic: false, pct } => {
                audio::set_volume(&tools, pct).map_err(|e| e.to_string())
            }
            Op::Volume { mic: true, .. } => Err("mic volume isn't supported yet".into()),
            Op::Mute { mic } => {
                let f = if mic {
                    audio::set_mic_mute
                } else {
                    audio::set_mute
                };
                f(&tools, audio::Mute::Toggle).map_err(|e| e.to_string())
            }
            Op::Media { player, cmd } => {
                sliver_session::mpris::control(&ses, &player, cmd).map_err(|e| e.to_string())
            }
            Op::Caffeine(on) => handle.set_caffeine(on).map_err(|e| e.to_string()),
        };
        if let Err(e) = r {
            eprintln!("sliver-agent: {e}");
        }
    }
}
