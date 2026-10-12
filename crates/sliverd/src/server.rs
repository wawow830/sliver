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
}

enum Ready {
    Listener,
    Conn(ClientId),
    Input(usize),
    Session,
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
        };
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
                        Err(_) if p.fds.is_empty() => Attachment::None,
                        Err(_) => Attachment::Rejected("fd with a bad packet".into()),
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
            for out in self.daemon.handle(now, input) {
                match out {
                    Out::Send(client, msg) => {
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
                        self.conns.remove(&client);
                    }
                }
            }
        }
    }
}

fn hw_input(e: HwEvent) -> Input {
    match e {
        HwEvent::Touch(t) => Input::Touch(t),
        HwEvent::FnKey { held } => Input::FnKey { held },
        HwEvent::Activity => Input::Activity,
        HwEvent::Lid { closed } => Input::Lid { closed },
    }
}
