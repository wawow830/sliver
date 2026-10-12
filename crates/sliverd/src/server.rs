//! The event loop: one `poll(2)` over the IPC listener, agent connections, hardware input and the
//! seat watch, with the timeout taken from [`Daemon::next_deadline`]. With nothing to time (display
//! off, no ping outstanding) it blocks until input arrives, so there are no idle wakeups (invariant 3).
//!
//! Single-threaded and bounded. Each wakeup reads at most [`MAX_PACKETS_PER_WAKE`] packets per
//! connection, and an agent whose socket won't take a reply is disconnected, never waited for.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use sliver_ipc::AgentMsg;
use sliver_ipc::transport::{Conn, Listener};

use crate::backend::{HwEvent, InputSource};
use crate::daemon::{ClientId, Daemon, Input, Out};
use crate::ipc::{Attachment, import_attachment};
use crate::session::SessionSource;

/// Fairness bound: packets read from one connection per wakeup.
pub const MAX_PACKETS_PER_WAKE: usize = 64;

pub struct Server {
    daemon: Daemon,
    listener: Listener,
    conns: BTreeMap<ClientId, Conn>,
    next_client: u64,
    inputs: Vec<Box<dyn InputSource>>,
    session: Box<dyn SessionSource>,
    active_uid: Option<u32>,
    clock: Instant,
    sleep: SleepDetector,
}

enum Ready {
    Listener,
    Conn(ClientId),
    Input(usize),
    Session,
}

/// Notices that the machine was suspended, without D-Bus and without a timer: `CLOCK_BOOTTIME`
/// keeps counting while suspended, `CLOCK_MONOTONIC` (the daemon's clock) doesn't. Checked
/// whenever the loop wakes anyway. After a resume something always wakes it: the lid, a key,
/// the touchpad, the bar.
#[derive(Debug, Default)]
pub struct SleepDetector {
    gap_ms: Option<i64>,
}

/// More than this between the two clocks since the last look = the machine slept.
const SLEPT_MS: i64 = 1000;

impl SleepDetector {
    /// Feeds the current `BOOTTIME - MONOTONIC` gap. True if it grew: a suspend happened since.
    pub fn observe(&mut self, gap_ms: i64) -> bool {
        let slept = self.gap_ms.is_some_and(|g| gap_ms - g > SLEPT_MS);
        self.gap_ms = Some(gap_ms);
        slept
    }

    pub fn check(&mut self) -> bool {
        use rustix::time::{ClockId, clock_gettime};
        let ms = |t: rustix::time::Timespec| t.tv_sec * 1000 + t.tv_nsec / 1_000_000;
        let gap = ms(clock_gettime(ClockId::Boottime)) - ms(clock_gettime(ClockId::Monotonic));
        self.observe(gap)
    }
}

impl Server {
    /// `clock` must be the instant the daemon's time 0 refers to.
    pub fn new(
        daemon: Daemon,
        listener: Listener,
        inputs: Vec<Box<dyn InputSource>>,
        mut session: Box<dyn SessionSource>,
        clock: Instant,
    ) -> io::Result<Server> {
        listener.set_nonblocking(true)?;
        let active_uid = session.active_uid();
        let mut s = Server {
            daemon,
            listener,
            conns: BTreeMap::new(),
            next_client: 1,
            inputs,
            session,
            active_uid,
            clock,
            sleep: SleepDetector::default(),
        };
        s.sleep.check();
        s.dispatch(Input::SessionChanged { active_uid });
        Ok(s)
    }

    pub fn daemon(&self) -> &Daemon {
        &self.daemon
    }

    /// Daemon time: milliseconds since `clock`.
    pub fn now(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }

    /// Runs forever.
    pub fn run(&mut self) -> io::Result<()> {
        loop {
            self.run_once(None)?;
        }
    }

    /// Waits for one batch of events (or the daemon's next deadline, or `max_wait`) and handles it.
    pub fn run_once(&mut self, max_wait: Option<Duration>) -> io::Result<()> {
        let now = self.now();
        let until_deadline = self
            .daemon
            .next_deadline()
            .map(|d| Duration::from_millis(d.saturating_sub(now)));
        let wait = match (until_deadline, max_wait) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let ready = self.poll(wait)?;
        // Before the event that woke us (e.g. the lid opening): repaint what s2idle may have lost.
        if self.sleep.check() {
            self.dispatch(Input::Resumed);
        }
        for r in ready {
            match r {
                Ready::Listener => self.accept(),
                Ready::Conn(id) => self.read_conn(id),
                Ready::Input(i) => {
                    let events = self.inputs[i].drain();
                    for e in events {
                        self.dispatch(hw_input(e));
                    }
                }
                Ready::Session => {
                    let uid = self.session.active_uid();
                    if uid != self.active_uid {
                        self.active_uid = uid;
                        self.dispatch(Input::SessionChanged { active_uid: uid });
                    }
                }
            }
        }
        if self.daemon.next_deadline().is_some_and(|d| d <= self.now()) {
            self.dispatch(Input::Tick);
        }
        Ok(())
    }

    fn poll(&self, wait: Option<Duration>) -> io::Result<Vec<Ready>> {
        let mut tags = vec![Ready::Listener];
        let mut fds: Vec<BorrowedFd<'_>> = vec![self.listener.as_fd()];
        for (id, c) in &self.conns {
            tags.push(Ready::Conn(*id));
            fds.push(c.as_fd());
        }
        for (i, src) in self.inputs.iter().enumerate() {
            tags.push(Ready::Input(i));
            fds.push(src.as_fd());
        }
        tags.push(Ready::Session);
        fds.push(self.session.as_fd());
        let mut pfds: Vec<PollFd<'_>> = fds
            .into_iter()
            .map(|fd| PollFd::from_borrowed_fd(fd, PollFlags::IN))
            .collect();
        let timeout = wait.map(|d| Timespec {
            tv_sec: d.as_secs() as _,
            tv_nsec: d.subsec_nanos() as _,
        });
        match rustix::event::poll(&mut pfds, timeout.as_ref()) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        }
        let flags: Vec<bool> = pfds.iter().map(|p| !p.revents().is_empty()).collect();
        Ok(tags
            .into_iter()
            .zip(flags)
            .filter_map(|(t, r)| r.then_some(t))
            .collect())
    }

    fn accept(&mut self) {
        loop {
            let conn = match self.listener.accept() {
                Ok(c) => c,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    eprintln!("sliverd: accept: {e}");
                    return;
                }
            };
            let uid = match (conn.set_nonblocking(true), conn.peer_cred()) {
                (Ok(()), Ok(cred)) => cred.uid,
                _ => continue, // dropped
            };
            let client = ClientId(self.next_client);
            self.next_client += 1;
            self.conns.insert(client, conn);
            self.dispatch(Input::Connected { client, uid });
        }
    }

    fn read_conn(&mut self, client: ClientId) {
        for _ in 0..MAX_PACKETS_PER_WAKE {
            let Some(conn) = self.conns.get(&client) else {
                return;
            };
            match conn.recv::<AgentMsg>() {
                Ok(Some(p)) => {
                    let fd = match &p.msg {
                        Ok(msg) => import_attachment(msg, p.fds),
                        // Bad packets close the connection anyway; their fds are dropped.
                        Err(_) => Attachment::None,
                    };
                    self.dispatch(Input::Packet {
                        client,
                        msg: p.msg,
                        fd,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Ok(None) | Err(_) => {
                    self.conns.remove(&client);
                    self.dispatch(Input::Disconnected { client });
                    return;
                }
            }
        }
    }

    fn dispatch(&mut self, input: Input) {
        let mut queue = vec![input];
        while let Some(input) = queue.pop() {
            let now = self.now();
            let before = crate::log::enabled().then(|| self.snapshot());
            if crate::log::enabled() {
                log_input(&input);
            }
            let outs = self.daemon.handle(now, input);
            if let Some(before) = before {
                let after = self.snapshot();
                if after != before {
                    crate::debug!("state {after}");
                }
            }
            for out in outs {
                match out {
                    Out::Send(client, msg) => {
                        crate::debug!("-> {} {}", client.0, describe_daemon(&msg));
                        let Some(conn) = self.conns.get(&client) else {
                            continue;
                        };
                        if conn.send(&msg).is_err() {
                            // Full or broken socket: a misbehaving agent is dropped, not awaited.
                            self.conns.remove(&client);
                            queue.push(Input::Disconnected { client });
                        }
                    }
                    Out::Close(client) => {
                        crate::debug!("close {}", client.0);
                        self.conns.remove(&client);
                    }
                }
            }
        }
    }
}

impl Server {
    /// One line of the state worth logging when it changes.
    fn snapshot(&self) -> String {
        let d = &self.daemon;
        format!(
            "view={:?} display={:?} hatch={} agent={:?} drives={}",
            d.shown_view(),
            d.display_state(),
            d.hatch_active(),
            d.active_client().map(|c| c.0),
            d.agent_drives()
        )
    }
}

fn log_input(input: &Input) {
    match input {
        Input::Connected { client, uid } => crate::debug!("connect {} uid {uid}", client.0),
        Input::Packet { client, msg, fd } => {
            let what = match msg {
                Ok(m) => describe_agent(m),
                Err(e) => format!("bad packet: {e}"),
            };
            let fd = match fd {
                Attachment::None => "",
                Attachment::Buffer(_) => " +fd",
                Attachment::Rejected(_) => " +bad fd",
                Attachment::WrongCount => " +wrong fd count",
            };
            crate::debug!("<- {} {what}{fd}", client.0);
        }
        Input::Disconnected { client } => crate::debug!("hangup {}", client.0),
        Input::Touch(t) => crate::debug!(
            "touch slot {} {:?} x {:.1} y {:.1}",
            t.slot,
            t.phase,
            t.x,
            t.y
        ),
        Input::FnKey { held } => crate::debug!("fn {}", if *held { "down" } else { "up" }),
        Input::Activity => {}
        Input::Lid { closed } => crate::debug!("lid {}", if *closed { "closed" } else { "open" }),
        Input::ScreenBacklight => crate::debug!("screen backlight changed"),
        Input::SessionChanged { active_uid } => crate::debug!("seat0 active uid {active_uid:?}"),
        Input::Tick => crate::debug!("tick"),
        Input::Resumed => crate::debug!("resumed from suspend"),
    }
}

fn describe_agent(m: &AgentMsg) -> String {
    match m {
        AgentMsg::Present(p) => format!(
            "present #{} {:?} image {} pressed {:?} {} regions",
            p.serial,
            p.layer,
            p.image,
            p.pressed,
            p.regions.len()
        ),
        AgentMsg::EmitKeys { keys, mode } => format!("emit_keys {keys:?} {mode:?}"),
        m => format!("{m:?}"),
    }
}

fn describe_daemon(m: &sliver_ipc::DaemonMsg) -> String {
    use sliver_ipc::DaemonMsg as D;
    match m {
        D::Welcome { role, proto, .. } => format!("welcome {role:?} {proto:?}"),
        m => format!("{m:?}"),
    }
}

fn hw_input(e: HwEvent) -> Input {
    match e {
        HwEvent::Touch(t) => Input::Touch(t),
        HwEvent::FnKey { held } => Input::FnKey { held },
        HwEvent::Activity => Input::Activity,
        HwEvent::Lid { closed } => Input::Lid { closed },
        HwEvent::ScreenBacklight => Input::ScreenBacklight,
    }
}

#[cfg(test)]
mod tests {
    use super::SleepDetector;

    #[test]
    fn notices_a_suspend_once() {
        let mut s = SleepDetector::default();
        assert!(!s.observe(5_000), "first look is the baseline");
        assert!(!s.observe(5_003), "clock jitter is not a sleep");
        assert!(s.observe(65_003), "a minute suspended");
        assert!(!s.observe(65_004), "reported once");
        // The real clocks answer and never report a sleep within one instant.
        let mut real = SleepDetector::default();
        real.check();
        assert!(!real.check());
    }
}
