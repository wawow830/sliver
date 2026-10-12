//! In-process fake `sliverd` for headless tests of the agent (and a reference for T8).
//!
//! It speaks the real protocol over a real `SOCK_SEQPACKET` pair with real sealed memfds, applies
//! the same [`validate`](crate::validate) rules and the same [`TouchRouter`] as sliverd. Instead of
//! DRM/uinput it keeps the presented frames as bytes and records emitted keys. Drive it from the
//! test thread: call [`FakeDaemon::pump`] after the agent sends something.
//!
//! ```
//! use sliver_ipc::{fake::FakeDaemon, proto::*};
//! let (mut d, agent) = FakeDaemon::new(FakeDaemon::mac14_7()).unwrap();
//! agent.send(&AgentMsg::Hello { proto: PROTO, agent: "doc".into() }).unwrap();
//! d.pump().unwrap();
//! assert!(matches!(agent.recv::<DaemonMsg>().unwrap().unwrap().msg, Ok(DaemonMsg::Welcome { .. })));
//! ```

use crate::keys;
use crate::proto::*;
use crate::router::{Routed, TouchRouter};
use crate::shm::MappedBuffer;
use crate::transport::{Conn, Packet};
use crate::validate;
use std::collections::HashMap;
use std::io;

/// A frame as sliverd copied it: tightly packed pixels plus its regions.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub serial: u32,
    pub pixels: Vec<u8>,
    pub pressed: Option<Vec<u8>>,
    pub regions: Vec<Region>,
}

#[derive(Debug)]
pub struct FakeDaemon {
    conn: Conn,
    pub panel: PanelInfo,
    /// Keys that must stay reachable (defaults to [`keys::floor_for_panel`]).
    pub floor: Vec<u16>,
    /// `(negotiated version, agent name)` after a successful hello.
    pub hello: Option<(ProtoVersion, String)>,
    buffers: HashMap<u32, MappedBuffer>,
    /// `None` = built-in safe base layer.
    pub base: Option<Frame>,
    /// `None` = built-in fn layer.
    pub fn_layer: Option<Frame>,
    pub fn_held: bool,
    pub display: DisplayState,
    pub policy: DisplayPolicy,
    router: TouchRouter,
    /// Everything that went out through "uinput", in order.
    pub keys: Vec<(u16, bool)>,
    /// Errors sent to the agent.
    pub errors: Vec<(ErrorCode, String)>,
    pub pongs: Vec<u64>,
    pub wakes: u32,
    /// The agent said `bye` or broke the protocol fatally. The safe layer is showing.
    pub closed: bool,
    next_nonce: u64,
    clock_us: u64,
}

impl FakeDaemon {
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

    /// Returns the fake and the agent's end of the connection.
    pub fn new(panel: PanelInfo) -> io::Result<(FakeDaemon, Conn)> {
        let (ours, agent) = Conn::pair()?;
        ours.set_nonblocking(true)?;
        let floor = keys::floor_for_panel(panel.width).to_vec();
        Ok((
            FakeDaemon {
                conn: ours,
                panel,
                floor,
                hello: None,
                buffers: HashMap::new(),
                base: None,
                fn_layer: None,
                fn_held: false,
                display: DisplayState::On,
                policy: DisplayPolicy::default(),
                router: TouchRouter::new(),
                keys: Vec::new(),
                errors: Vec::new(),
                pongs: Vec::new(),
                wakes: 0,
                closed: false,
                next_nonce: 1,
                clock_us: 0,
            },
            agent,
        ))
    }

    /// Handle everything the agent has sent so far. Returns the number of packets handled.
    pub fn pump(&mut self) -> io::Result<usize> {
        let mut n = 0;
        while !self.closed {
            match self.conn.recv::<AgentMsg>() {
                Ok(Some(p)) => {
                    n += 1;
                    self.handle(p)?;
                }
                Ok(None) => self.safe_mode(),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    /// Whether the built-in safe layer is what's on the bar.
    pub fn safe_layer_active(&self) -> bool {
        self.base.is_none()
    }

    /// The regions a touch would hit right now (agent frame or built-in layer).
    pub fn visible_regions(&self) -> Vec<Region> {
        let frame = if self.fn_held {
            &self.fn_layer
        } else {
            &self.base
        };
        match frame {
            Some(f) => f.regions.clone(),
            None => builtin_layer(&self.panel),
        }
    }

    pub fn touch(&mut self, slot: u8, phase: TouchPhase, x: f32, y: f32) -> io::Result<()> {
        let regions = self.visible_regions();
        let out = self.router.touch(&regions, slot, phase, x, y);
        self.apply(out)
    }

    pub fn tap(&mut self, x: f32, y: f32) -> io::Result<()> {
        self.touch(0, TouchPhase::Down, x, y)?;
        self.touch(0, TouchPhase::Up, x, y)
    }

    pub fn set_fn(&mut self, held: bool) -> io::Result<()> {
        self.fn_held = held;
        self.send(&DaemonMsg::FnKey { held })
    }

    pub fn set_display(&mut self, state: DisplayState) -> io::Result<()> {
        if state == DisplayState::Off {
            let out = self.router.cancel_all();
            self.apply(out)?;
        }
        self.display = state;
        self.send(&DaemonMsg::Display { state })
    }

    /// Send a ping and return its nonce. Check [`FakeDaemon::pongs`] after the next pump.
    pub fn ping(&mut self) -> io::Result<u64> {
        let nonce = self.next_nonce;
        self.next_nonce += 1;
        self.send(&DaemonMsg::Ping { nonce })?;
        Ok(nonce)
    }

    fn send(&mut self, m: &DaemonMsg) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        match self.conn.send(m) {
            // The agent hung up: that's what the safe layer is for, not a test failure.
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                self.safe_mode();
                Ok(())
            }
            r => r,
        }
    }

    fn error(
        &mut self,
        code: ErrorCode,
        msg: impl Into<String>,
        serial: Option<u32>,
    ) -> io::Result<()> {
        let msg = msg.into();
        self.errors.push((code, msg.clone()));
        self.send(&DaemonMsg::Error { code, msg, serial })
    }

    fn safe_mode(&mut self) {
        self.closed = true;
        self.base = None;
        self.fn_layer = None;
        let out = self.router.cancel_all();
        self.record_keys(&out);
    }

    fn record_keys(&mut self, out: &[Routed]) {
        for r in out {
            if let Routed::Key { code, down } = r {
                self.keys.push((*code, *down));
            }
        }
    }

    fn apply(&mut self, out: Vec<Routed>) -> io::Result<()> {
        self.record_keys(&out);
        for r in out {
            if let Routed::Agent {
                region,
                slot,
                phase,
                x,
                y,
            } = r
            {
                self.clock_us += 1_000;
                let time_us = self.clock_us;
                self.send(&DaemonMsg::Touch {
                    region,
                    slot,
                    phase,
                    x,
                    y,
                    time_us,
                })?;
            }
        }
        Ok(())
    }

    fn handle(&mut self, p: Packet<AgentMsg>) -> io::Result<()> {
        let Packet { msg, mut fds } = p;
        let msg = match msg {
            Ok(m) => m,
            Err(e) => return self.error(e.code(), e.to_string(), None),
        };
        if msg.carries_fd() != (fds.len() == 1) || fds.len() > 1 {
            return self.error(ErrorCode::Malformed, "wrong number of fds", None);
        }
        if self.hello.is_none() && !matches!(msg, AgentMsg::Hello { .. }) {
            return self.error(ErrorCode::Malformed, "hello first", None);
        }
        match msg {
            AgentMsg::Hello { proto, agent } => match PROTO.negotiate(proto) {
                Some(v) => {
                    self.hello = Some((v, agent));
                    let panel = self.panel.clone();
                    self.send(&DaemonMsg::Welcome {
                        proto: v,
                        panel,
                        role: Role::Active,
                        features: vec![],
                    })?;
                }
                None => {
                    self.error(
                        ErrorCode::Proto,
                        format!("unsupported major {}", proto.0),
                        None,
                    )?;
                    self.safe_mode();
                }
            },
            AgentMsg::AttachBuffer {
                id,
                width,
                height,
                stride,
                format,
            } => {
                let replacing = usize::from(self.buffers.contains_key(&id));
                if let Err(e) = validate::validate_buffer(
                    &self.panel,
                    self.buffers.len() - replacing,
                    width,
                    height,
                    stride,
                    format,
                ) {
                    return self.error(e.code(), e.to_string(), None);
                }
                let fd = fds.pop().expect("checked above");
                match MappedBuffer::import(fd, width, height, stride, format) {
                    Ok(b) => {
                        self.buffers.insert(id, b);
                    }
                    Err(e) => return self.error(ErrorCode::InvalidBuffer, e.to_string(), None),
                }
            }
            AgentMsg::DetachBuffer { id } => {
                self.buffers.remove(&id);
            }
            AgentMsg::Present(p) => self.present(p)?,
            AgentMsg::ClearLayer { layer: Layer::Fn } => self.fn_layer = None,
            AgentMsg::ClearLayer { layer: Layer::Base } => {
                self.error(
                    ErrorCode::InvalidFrame,
                    "base can't be cleared; send bye",
                    None,
                )?;
            }
            AgentMsg::DisplayPolicy(d) => {
                let p = &mut self.policy;
                p.brightness = d.brightness.or(p.brightness);
                p.dim_after_ms = d.dim_after_ms.or(p.dim_after_ms);
                p.dim_brightness = d.dim_brightness.or(p.dim_brightness);
                p.off_after_ms = d.off_after_ms.or(p.off_after_ms);
                p.follow_screen = d.follow_screen.or(p.follow_screen);
            }
            AgentMsg::Wake => {
                self.wakes += 1;
                if self.display != DisplayState::On {
                    self.set_display(DisplayState::On)?;
                }
            }
            AgentMsg::EmitKeys { keys, mode } => {
                if let Err(e) = validate::validate_keys(&keys) {
                    return self.error(e.code(), e.to_string(), None);
                }
                match mode {
                    KeyMode::Down => self.keys.extend(keys.iter().map(|&k| (k, true))),
                    KeyMode::Up => self.keys.extend(keys.iter().map(|&k| (k, false))),
                    KeyMode::Tap => {
                        self.keys.extend(keys.iter().map(|&k| (k, true)));
                        self.keys.extend(keys.iter().rev().map(|&k| (k, false)));
                    }
                }
            }
            AgentMsg::Pong { nonce } => self.pongs.push(nonce),
            AgentMsg::Bye => self.safe_mode(),
        }
        Ok(())
    }

    fn present(&mut self, p: Present) -> io::Result<()> {
        let serial = Some(p.serial);
        if let Err(e) = validate::validate_present(&self.panel, &p) {
            return self.error(e.code(), e.to_string(), serial);
        }
        let copy = |id: u32| -> Option<Vec<u8>> {
            let mut v = Vec::new();
            self.buffers.get(&id)?.copy_to(&mut v);
            Some(v)
        };
        let (Some(pixels), Ok(pressed)) = (
            copy(p.image),
            p.pressed.map(|id| copy(id).ok_or(())).transpose(),
        ) else {
            return self.error(ErrorCode::InvalidFrame, "unknown buffer id", serial);
        };
        let frame = Frame {
            serial: p.serial,
            pixels,
            pressed,
            regions: p.regions,
        };
        let (base, fn_regions) = match p.layer {
            Layer::Base => (
                frame.regions.as_slice(),
                self.fn_layer.as_ref().map(|f| f.regions.as_slice()),
            ),
            Layer::Fn => (
                self.base.as_ref().map_or(&[][..], |f| f.regions.as_slice()),
                Some(frame.regions.as_slice()),
            ),
        };
        let missing = validate::escape_floor(base, fn_regions, &self.floor);
        match p.layer {
            Layer::Base => self.base = Some(frame),
            Layer::Fn => self.fn_layer = Some(frame),
        }
        if !missing.is_empty() {
            self.fn_layer = None;
            self.error(
                ErrorCode::EscapeFloor,
                format!("unreachable keys {missing:?}; built-in fn layer in use"),
                serial,
            )?;
        }
        self.send(&DaemonMsg::Presented { serial: p.serial })
    }
}

/// The built-in safe layer's regions: Esc + F1–F12 in equal slots (pixels are sliverd's business).
pub fn builtin_layer(panel: &PanelInfo) -> Vec<Region> {
    let all = keys::ESCAPE_FLOOR;
    let w = panel.width / all.len() as u32;
    all.iter()
        .enumerate()
        .map(|(i, &k)| Region {
            id: i as u32,
            rect: Rect::new(i as u32 * w, 0, w, panel.height),
            kind: RegionKind::Key { keys: vec![k] },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::*;
    use crate::shm::ShmBuffer;
    use std::os::fd::AsFd;

    fn recv(c: &Conn) -> DaemonMsg {
        c.recv::<DaemonMsg>().unwrap().unwrap().msg.unwrap()
    }

    /// Hello + one attached buffer filled with `fill`.
    fn connect() -> (FakeDaemon, Conn, ShmBuffer) {
        let (mut d, a) = FakeDaemon::new(FakeDaemon::mac14_7()).unwrap();
        a.send(&AgentMsg::Hello {
            proto: ProtoVersion(1, 7),
            agent: "test".into(),
        })
        .unwrap();
        d.pump().unwrap();
        match recv(&a) {
            DaemonMsg::Welcome {
                proto, panel, role, ..
            } => {
                assert_eq!(proto, ProtoVersion(1, 0));
                assert_eq!((panel.width, panel.height), (2008, 60));
                assert_eq!(role, Role::Active);
            }
            m => panic!("{m:?}"),
        }
        let mut buf = ShmBuffer::new(2008, 60, PixelFormat::Xrgb8888).unwrap();
        buf.pixels_mut().fill(0x11);
        a.send_with_fd(&buf.attach_msg(1), Some(buf.as_fd()))
            .unwrap();
        d.pump().unwrap();
        assert!(d.errors.is_empty(), "{:?}", d.errors);
        (d, a, buf)
    }

    fn present(layer: Layer, serial: u32, regions: Vec<Region>) -> AgentMsg {
        AgentMsg::Present(Present {
            serial,
            layer,
            image: 1,
            pressed: None,
            regions,
            damage: None,
        })
    }

    fn key(id: u32, x: u32, w: u32, k: u16) -> Region {
        Region {
            id,
            rect: Rect::new(x, 0, w, 60),
            kind: RegionKind::Key { keys: vec![k] },
        }
    }

    fn fkeys() -> Vec<Region> {
        F_KEYS
            .iter()
            .enumerate()
            .map(|(i, &k)| key(i as u32, i as u32 * 160, 160, k))
            .collect()
    }

    #[test]
    fn present_copies_and_acks() {
        let (mut d, a, mut buf) = connect();
        assert!(d.safe_layer_active());
        a.send(&present(Layer::Base, 5, fkeys())).unwrap();
        d.pump().unwrap();
        assert_eq!(recv(&a), DaemonMsg::Presented { serial: 5 });
        let f = d.base.as_ref().unwrap();
        assert_eq!(f.pixels.len(), 2008 * 60 * 4);
        assert!(f.pixels.iter().all(|&b| b == 0x11));
        // The agent may reuse the buffer after `presented` without affecting the shown frame.
        buf.pixels_mut().fill(0);
        assert!(d.base.as_ref().unwrap().pixels.iter().all(|&b| b == 0x11));
        assert!(!d.safe_layer_active());
    }

    #[test]
    fn escape_floor_swaps_in_builtin_fn_layer() {
        let (mut d, a, _buf) = connect();
        // Base with Esc + an agent area but no F-keys, and an agent fn layer without them either.
        let base = vec![
            key(0, 0, 100, KEY_ESC),
            Region {
                id: 1,
                rect: Rect::new(100, 0, 1908, 60),
                kind: RegionKind::Agent,
            },
        ];
        a.send(&present(Layer::Base, 1, base)).unwrap();
        a.send(&present(Layer::Fn, 2, vec![key(0, 0, 100, 113)]))
            .unwrap();
        d.pump().unwrap();
        assert_eq!(recv(&a), DaemonMsg::Presented { serial: 1 });
        match recv(&a) {
            DaemonMsg::Error {
                code: ErrorCode::EscapeFloor,
                serial: Some(2),
                ..
            } => {}
            m => panic!("{m:?}"),
        }
        assert_eq!(recv(&a), DaemonMsg::Presented { serial: 2 });
        assert!(d.fn_layer.is_none(), "built-in fn layer must be in use");
        // Holding Fn gives F-keys from the built-in layer.
        d.set_fn(true).unwrap();
        let f5 = builtin_layer(&d.panel)
            .into_iter()
            .find(|r| r.kind == RegionKind::Key { keys: vec![KEY_F5] })
            .unwrap();
        d.tap(f5.rect.x as f32 + 1.0, 30.0).unwrap();
        assert_eq!(d.keys, vec![(KEY_F5, true), (KEY_F5, false)]);
        // An agent fn layer with F1–F12 is accepted.
        a.send(&present(Layer::Fn, 3, fkeys())).unwrap();
        d.pump().unwrap();
        assert_eq!(recv(&a), DaemonMsg::FnKey { held: true });
        assert_eq!(recv(&a), DaemonMsg::Presented { serial: 3 });
        assert!(d.fn_layer.is_some());
    }

    #[test]
    fn touches_route_to_keys_and_agent() {
        let (mut d, a, _buf) = connect();
        let mut regions = fkeys();
        regions.push(Region {
            id: 99,
            rect: Rect::new(1920, 0, 88, 60),
            kind: RegionKind::Agent,
        });
        a.send(&present(Layer::Base, 1, regions)).unwrap();
        d.pump().unwrap();
        recv(&a);
        d.tap(10.0, 30.0).unwrap();
        assert_eq!(d.keys, vec![(KEY_F1, true), (KEY_F1, false)]);
        d.touch(1, TouchPhase::Down, 1950.0, 30.0).unwrap();
        d.touch(1, TouchPhase::Move, 1000.0, 30.0).unwrap();
        match recv(&a) {
            DaemonMsg::Touch {
                region: 99,
                phase: TouchPhase::Down,
                ..
            } => {}
            m => panic!("{m:?}"),
        }
        match recv(&a) {
            DaemonMsg::Touch {
                region: 99,
                phase: TouchPhase::Move,
                x,
                ..
            } => assert_eq!(x, 1000.0),
            m => panic!("{m:?}"),
        }
        assert_eq!(
            d.keys.len(),
            2,
            "sliding an agent touch over keys presses nothing"
        );
    }

    #[test]
    fn bad_input_is_refused_not_followed() {
        let (mut d, a, _buf) = connect();
        a.send(&present(Layer::Base, 1, vec![key(0, 0, 100, KEY_SYSRQ)]))
            .unwrap();
        a.send(&AgentMsg::EmitKeys {
            keys: vec![56, KEY_SYSRQ],
            mode: KeyMode::Tap,
        })
        .unwrap();
        a.send(&present(Layer::Base, 2, vec![key(0, 2000, 100, KEY_ESC)]))
            .unwrap();
        a.send(&AgentMsg::Present(Present {
            serial: 3,
            layer: Layer::Base,
            image: 42,
            pressed: None,
            regions: vec![],
            damage: None,
        }))
        .unwrap();
        // fd on a message that shouldn't carry one
        a.send_with_fd(&AgentMsg::Wake, Some(_buf.as_fd())).unwrap();
        d.pump().unwrap();
        let codes: Vec<ErrorCode> = d.errors.iter().map(|e| e.0).collect();
        assert_eq!(
            codes,
            vec![
                ErrorCode::ForbiddenKey,
                ErrorCode::ForbiddenKey,
                ErrorCode::InvalidFrame,
                ErrorCode::InvalidFrame,
                ErrorCode::Malformed
            ]
        );
        assert!(d.keys.is_empty());
        assert!(d.safe_layer_active());
        assert_eq!(d.wakes, 0);
    }

    #[test]
    fn unsealed_buffer_rejected() {
        let (mut d, a, _buf) = connect();
        let fd = rustix::fs::memfd_create("x", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, 2008 * 60 * 4).unwrap();
        let m = AgentMsg::AttachBuffer {
            id: 2,
            width: 2008,
            height: 60,
            stride: 8032,
            format: PixelFormat::Xrgb8888,
        };
        a.send_with_fd(&m, Some(fd.as_fd())).unwrap();
        d.pump().unwrap();
        assert_eq!(d.errors[0].0, ErrorCode::InvalidBuffer);
    }

    #[test]
    fn hangup_and_bye_mean_safe_layer() {
        let (mut d, a, _buf) = connect();
        a.send(&present(Layer::Base, 1, fkeys())).unwrap();
        d.pump().unwrap();
        // Finger down on F1 when the agent dies: the key must be released.
        d.touch(0, TouchPhase::Down, 10.0, 30.0).unwrap();
        drop(a);
        d.pump().unwrap();
        assert!(d.safe_layer_active() && d.closed);
        assert_eq!(d.keys, vec![(KEY_F1, true), (KEY_F1, false)]);

        let (mut d, a, _buf) = connect();
        a.send(&present(Layer::Base, 1, fkeys())).unwrap();
        a.send(&AgentMsg::Bye).unwrap();
        d.pump().unwrap();
        assert!(d.safe_layer_active());
    }

    #[test]
    fn version_mismatch_and_hello_first() {
        let (mut d, a) = FakeDaemon::new(FakeDaemon::mac14_7()).unwrap();
        a.send(&AgentMsg::Wake).unwrap();
        a.send(&AgentMsg::Hello {
            proto: ProtoVersion(2, 0),
            agent: "future".into(),
        })
        .unwrap();
        d.pump().unwrap();
        assert_eq!(
            d.errors.iter().map(|e| e.0).collect::<Vec<_>>(),
            vec![ErrorCode::Malformed, ErrorCode::Proto]
        );
        assert!(d.closed && d.hello.is_none());
    }

    #[test]
    fn ping_policy_wake_emit() {
        let (mut d, a, _buf) = connect();
        let n = d.ping().unwrap();
        let DaemonMsg::Ping { nonce } = recv(&a) else {
            panic!()
        };
        assert_eq!(nonce, n);
        a.send(&AgentMsg::Pong { nonce }).unwrap();
        a.send(&AgentMsg::DisplayPolicy(DisplayPolicy {
            dim_after_ms: Some(30_000),
            ..Default::default()
        }))
        .unwrap();
        a.send(&AgentMsg::DisplayPolicy(DisplayPolicy {
            brightness: Some(90),
            ..Default::default()
        }))
        .unwrap();
        a.send(&AgentMsg::EmitKeys {
            keys: vec![29, 46],
            mode: KeyMode::Tap,
        })
        .unwrap();
        d.pump().unwrap();
        d.set_display(DisplayState::Off).unwrap();
        a.send(&AgentMsg::Wake).unwrap();
        d.pump().unwrap();
        assert_eq!(d.pongs, vec![n]);
        assert_eq!(
            (d.policy.brightness, d.policy.dim_after_ms),
            (Some(90), Some(30_000))
        );
        assert_eq!(
            d.keys,
            vec![(29, true), (46, true), (46, false), (29, false)]
        );
        assert_eq!(
            recv(&a),
            DaemonMsg::Display {
                state: DisplayState::Off
            }
        );
        assert_eq!(
            recv(&a),
            DaemonMsg::Display {
                state: DisplayState::On
            }
        );
        assert_eq!(d.wakes, 1);
    }
}
