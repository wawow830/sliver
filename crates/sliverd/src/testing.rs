//! An in-process sliverd for headless agent tests: the **real** daemon core on fake hardware,
//! over a real `SOCK_SEQPACKET` pair with real sealed memfds.
//!
//! API-compatible with the former `sliver_ipc::fake::FakeDaemon` (same method and field names), so switching
//! is an import change. It adds [`TestDaemon::frame`] (what the bar shows, as a pixmap with pressed
//! feedback and the hatch included) and a simulated clock ([`TestDaemon::advance`]) for idle/ping
//! deadlines. Single-threaded: call [`TestDaemon::pump`] after the agent sends something.
//!
//! ```
//! use sliver_ipc::proto::*;
//! use sliverd::testing::TestDaemon;
//! let (mut d, agent) = TestDaemon::new(TestDaemon::mac14_7()).unwrap();
//! agent.send(&AgentMsg::Hello { proto: PROTO, agent: "doc".into() }).unwrap();
//! d.pump().unwrap();
//! assert!(matches!(agent.recv::<DaemonMsg>().unwrap().unwrap().msg, Ok(DaemonMsg::Welcome { .. })));
//! ```

use std::io;

use sliver_ipc::AgentMsg;
use sliver_ipc::proto::{
    DaemonMsg, DisplayPolicy, DisplayState, ErrorCode, Layer, PanelInfo, PixelFormat, ProtoVersion,
    Region, TouchPhase,
};
use sliver_ipc::transport::Conn;
use sliver_render::Pixmap;
use sliver_render::panel::{Orientation, blit_xrgb8888};

use crate::backend::TouchEvent;
use crate::daemon::{ClientId, Config, Daemon, Input, Out};
use crate::fake::{FakeHardware, KeyEvent};
use crate::ipc::{Attachment, import_attachment};

const CLIENT: ClientId = ClientId(1);

/// A frame as sliverd copied it: tightly packed XRGB8888 plus its regions (FakeDaemon's shape).
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub serial: u32,
    pub pixels: Vec<u8>,
    pub pressed: Option<Vec<u8>>,
    pub regions: Vec<Region>,
}

pub struct TestDaemon {
    conn: Option<Conn>,
    daemon: Daemon,
    pub hw: FakeHardware,
    pub panel: PanelInfo,
    /// Daemon time in ms. Advance with [`TestDaemon::advance`].
    pub now_ms: u64,
    /// `(negotiated version, agent name)` after a successful hello.
    pub hello: Option<(ProtoVersion, String)>,
    /// The agent's accepted base frame. `None` = built-in safe layer.
    pub base: Option<Frame>,
    /// The agent's fn frame. `None` = built-in fn layer.
    pub fn_layer: Option<Frame>,
    pub fn_held: bool,
    pub display: DisplayState,
    pub policy: DisplayPolicy,
    /// Everything that went out through "uinput", in order.
    pub keys: Vec<(u16, bool)>,
    /// Errors sent to the agent.
    pub errors: Vec<(ErrorCode, String)>,
    pub pongs: Vec<u64>,
    pub wakes: u32,
    /// sliverd closed the connection (bye, protocol violation, hung agent) or the agent hung up.
    pub closed: bool,
    /// Answer the daemon's own liveness pings on the agent's behalf instead of forwarding them
    /// (default: on, like FakeDaemon, which never pinged). Set it to false to test that the agent
    /// pongs, and that a silent one is dropped after 2 s of simulated time.
    pub auto_pong: bool,
    next_nonce: u64,
    lid_closed: bool,
}

impl TestDaemon {
    /// This machine's bar (docs/hardware.md).
    pub fn mac14_7() -> PanelInfo {
        PanelInfo {
            width: 2008,
            height: 60,
            format: PixelFormat::Xrgb8888,
            max_brightness: 255,
            model: "Mac14,7".into(),
        }
    }

    /// Returns the daemon and the agent's end of the connection. The agent's uid is the active one.
    pub fn new(panel: PanelInfo) -> io::Result<(TestDaemon, Conn)> {
        Self::with_config(panel, &Config::default())
    }

    pub fn with_config(panel: PanelInfo, config: &Config) -> io::Result<(TestDaemon, Conn)> {
        let (ours, agent) = Conn::pair()?;
        ours.set_nonblocking(true)?;
        let uid = ours.peer_cred()?.uid;
        let hw = FakeHardware::new(panel.width, panel.height);
        let daemon = Daemon::new(hw.backends(), panel.clone(), config, Some(uid), 0);
        let mut d = TestDaemon {
            conn: Some(ours),
            daemon,
            hw,
            panel,
            now_ms: 0,
            hello: None,
            base: None,
            fn_layer: None,
            fn_held: false,
            display: DisplayState::On,
            policy: DisplayPolicy::default(),
            keys: Vec::new(),
            errors: Vec::new(),
            pongs: Vec::new(),
            wakes: 0,
            closed: false,
            next_nonce: 1 << 32, // never collides with the daemon's own ping nonces
            lid_closed: false,
            auto_pong: true,
        };
        d.input(Input::Connected {
            client: CLIENT,
            uid,
        });
        Ok((d, agent))
    }

    /// The real daemon core, for anything this wrapper doesn't surface.
    pub fn daemon(&self) -> &Daemon {
        &self.daemon
    }

    /// Handle everything the agent has sent so far. Returns the number of packets handled.
    pub fn pump(&mut self) -> io::Result<usize> {
        let mut n = 0;
        while let Some(conn) = &self.conn {
            match conn.recv::<AgentMsg>() {
                Ok(Some(p)) => {
                    n += 1;
                    let fd = match &p.msg {
                        Ok(msg) => {
                            let fd = import_attachment(msg, p.fds);
                            if !matches!(fd, Attachment::WrongCount) {
                                self.observe(msg);
                            }
                            fd
                        }
                        Err(_) => Attachment::None,
                    };
                    self.input(Input::Packet {
                        client: CLIENT,
                        msg: p.msg,
                        fd,
                    });
                }
                Ok(None) => {
                    self.hang_up();
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    /// Whether the built-in safe layer is what's on the bar (no agent frames shown).
    pub fn safe_layer_active(&self) -> bool {
        !self.daemon.agent_drives()
    }

    /// Whether the safe hatch is showing the built-in layer.
    pub fn hatch_active(&self) -> bool {
        self.daemon.hatch_active()
    }

    /// The regions a touch would hit right now (agent frame or built-in layer).
    pub fn visible_regions(&self) -> Vec<Region> {
        self.daemon.visible_regions().to_vec()
    }

    /// What the bar shows right now, read back from the fake panel's framebuffer.
    pub fn frame(&self) -> Pixmap {
        self.hw.display.frame()
    }

    pub fn touch(&mut self, slot: u8, phase: TouchPhase, x: f32, y: f32) -> io::Result<()> {
        let time_us = self.now_ms * 1000;
        self.input(Input::Touch(TouchEvent {
            slot,
            phase,
            x,
            y,
            time_us,
        }));
        Ok(())
    }

    pub fn tap(&mut self, x: f32, y: f32) -> io::Result<()> {
        self.touch(0, TouchPhase::Down, x, y)?;
        self.touch(0, TouchPhase::Up, x, y)
    }

    /// A physical Fn event a second after the previous one (never a double press).
    pub fn set_fn(&mut self, held: bool) -> io::Result<()> {
        let t = self.now_ms + 1_000;
        self.fn_key(held, t)
    }

    /// A physical Fn event at `time_ms` (monotonic). Two quick presses open the hatch.
    pub fn fn_key(&mut self, held: bool, time_ms: u64) -> io::Result<()> {
        self.now_ms = self.now_ms.max(time_ms);
        self.input(Input::FnKey { held });
        Ok(())
    }

    /// Drives the real display-power logic to `state` by letting time pass: off = past the off
    /// timeout (lid closed if the agent disabled it), dimmed = past the dim timeout, on = activity.
    pub fn set_display(&mut self, state: DisplayState) -> io::Result<()> {
        use crate::idle::{DEFAULT_DIM_AFTER_MS, DEFAULT_OFF_AFTER_MS};
        match state {
            DisplayState::On => {
                if self.lid_closed {
                    self.lid_closed = false;
                    self.input(Input::Lid { closed: false });
                }
                self.input(Input::Activity);
            }
            DisplayState::Off => match self.policy.off_after_ms.unwrap_or(DEFAULT_OFF_AFTER_MS) {
                0 => {
                    self.lid_closed = true;
                    self.input(Input::Lid { closed: true });
                }
                ms => self.advance(ms),
            },
            DisplayState::Dimmed => {
                self.input(Input::Activity);
                self.advance(self.policy.dim_after_ms.unwrap_or(DEFAULT_DIM_AFTER_MS));
            }
        }
        Ok(())
    }

    /// Lets `ms` pass (dim/off, pong timeouts).
    pub fn advance(&mut self, ms: u64) {
        self.now_ms += ms;
        self.input(Input::Tick);
    }

    /// Send a ping and return its nonce. Check [`TestDaemon::pongs`] after the next pump. (The
    /// daemon also pings on its own, per §6. Those pongs are handled inside and not recorded.)
    pub fn ping(&mut self) -> io::Result<u64> {
        let nonce = self.next_nonce;
        self.next_nonce += 1;
        if let Some(c) = &self.conn {
            c.send(&DaemonMsg::Ping { nonce })?;
        }
        Ok(nonce)
    }

    fn observe(&mut self, msg: &AgentMsg) {
        match msg {
            AgentMsg::Hello { proto, agent } if self.hello.is_none() => {
                if let Some(v) = sliver_ipc::PROTO.negotiate(*proto) {
                    self.hello = Some((v, agent.clone()));
                }
            }
            AgentMsg::Pong { nonce } if *nonce >= 1 << 32 => self.pongs.push(*nonce),
            AgentMsg::Wake => self.wakes += 1,
            AgentMsg::DisplayPolicy(p) => {
                let q = &mut self.policy;
                q.brightness = p.brightness.or(q.brightness);
                q.dim_after_ms = p.dim_after_ms.or(q.dim_after_ms);
                q.dim_brightness = p.dim_brightness.or(q.dim_brightness);
                q.off_after_ms = p.off_after_ms.or(q.off_after_ms);
                q.follow_screen = p.follow_screen.or(q.follow_screen);
                q.pixel_shift = p.pixel_shift.or(q.pixel_shift);
            }
            _ => {}
        }
    }

    fn hang_up(&mut self) {
        self.conn = None;
        self.closed = true;
        self.input(Input::Disconnected { client: CLIENT });
    }

    fn input(&mut self, input: Input) {
        let mut queue = vec![input];
        while let Some(input) = queue.pop() {
            self.handle(input, &mut queue);
        }
        self.sync();
    }

    fn handle(&mut self, input: Input, queue: &mut Vec<Input>) {
        for out in self.daemon.handle(self.now_ms, input) {
            match out {
                Out::Send(_, DaemonMsg::Ping { nonce }) if self.auto_pong => {
                    queue.push(Input::Packet {
                        client: CLIENT,
                        msg: Ok(AgentMsg::Pong { nonce }),
                        fd: Attachment::None,
                    });
                }
                Out::Send(_, msg) => {
                    match &msg {
                        DaemonMsg::Error { code, msg, .. } => {
                            self.errors.push((*code, msg.clone()))
                        }
                        DaemonMsg::FnKey { held } => self.fn_held = *held,
                        _ => {}
                    }
                    if let Some(c) = &self.conn {
                        // The agent hung up: that's what the safe layer is for, not a test failure.
                        if c.send(&msg).is_err() {
                            self.conn = None;
                            self.closed = true;
                        }
                    }
                }
                Out::Close(_) => {
                    self.conn = None;
                    self.closed = true;
                }
            }
        }
    }

    /// Mirrors the daemon's state into the FakeDaemon-style public fields.
    fn sync(&mut self) {
        for e in self.hw.keys.take() {
            match e {
                KeyEvent::Down(k) => self.keys.push((k, true)),
                KeyEvent::Up(k) => self.keys.push((k, false)),
                KeyEvent::Sync => {}
            }
        }
        self.display = self.daemon.display_state();
        // Re-copy only when the frame changed (serials increase per present).
        let copy = |layer, cached: &Option<Frame>| match self.daemon.agent_frame(layer) {
            Some(f) if cached.as_ref().is_some_and(|c| c.serial == f.serial) => cached.clone(),
            Some(f) => Some(Frame {
                serial: f.serial,
                pixels: xrgb(&f.image),
                pressed: f.pressed.as_ref().map(xrgb),
                regions: f.regions.clone(),
            }),
            None => None,
        };
        let base = copy(Layer::Base, &self.base);
        let fn_layer = copy(Layer::Fn, &self.fn_layer);
        self.base = base;
        self.fn_layer = fn_layer;
    }
}

fn xrgb(p: &Pixmap) -> Vec<u8> {
    let stride = p.width() as usize * 4;
    let mut out = vec![0; stride * p.height() as usize];
    blit_xrgb8888(p, Orientation::Landscape, None, &mut out, stride);
    out
}
