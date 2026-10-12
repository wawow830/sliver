//! The daemon core: a sans-IO state machine (docs/api.md §6).
//!
//! The event loop feeds it [`Input`]s with a monotonic timestamp in milliseconds, arms one timer
//! for [`Daemon::next_deadline`], and carries out the returned [`Out`]s on the agent sockets.
//! Hardware effects (frames, backlight, keys) go straight to the [`Backends`]. Nothing here
//! blocks, sleeps or polls, so tests drive it with simulated time and fake hardware.

use std::collections::{BTreeMap, HashMap};

use sliver_ipc::hatch::FnHatch;
use sliver_ipc::proto::{
    CodecError, DaemonMsg, DisplayState, ErrorCode, KeyMode, Layer, PING_MIN_INTERVAL_MS,
    PONG_TIMEOUT_MS, PROTO, PanelInfo, Present, Rect, Region, Role, TouchPhase,
};
use sliver_ipc::router::{Routed, TouchRouter};
use sliver_ipc::{AgentMsg, validate};
use sliver_render::panel::{self, Span};
use sliver_render::{Fonts, Pixmap, Renderer, Stylesheet};

use crate::backend::{Backends, PixelSource, TouchEvent};
use crate::builtin::{BuiltinKind, BuiltinLayer};
use crate::idle::Idle;
use crate::ipc::Attachment;
use crate::keystate::KeyState;
use crate::profile::DeviceProfile;

/// Identifies one agent connection. Assigned by the transport, increasing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u64);

/// Bounded everything: connections beyond this are closed at once.
pub const MAX_CLIENTS: usize = 8;
/// `emit_keys` messages per second per client.
pub const EMIT_KEYS_PER_SEC: u32 = 100;
/// `wake` at most this often.
pub const WAKE_MIN_INTERVAL_MS: u64 = 1_000;
/// Damage spans closer than this are merged into one flush clip.
const DAMAGE_MERGE_GAP: u32 = 16;

/// System config (`/etc/sliver/sliverd.toml`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// Overrides the physical-Esc heuristic (bars ≥ 2170 px have none).
    pub physical_esc: Option<bool>,
    /// Safe layer shows the media row normally and F-keys while Fn is held (tiny-dfr's
    /// `MediaLayerDefault`).
    pub media_layer_default: bool,
}

pub enum Input {
    /// A new agent connection. `uid` is from `SO_PEERCRED`.
    Connected {
        client: ClientId,
        uid: u32,
    },
    /// One packet, decoded, with its fd imported ([`crate::ipc::import_attachment`]).
    Packet {
        client: ClientId,
        msg: Result<AgentMsg, CodecError>,
        fd: Attachment,
    },
    /// Hangup or read error.
    Disconnected {
        client: ClientId,
    },
    Touch(TouchEvent),
    /// `KEY_FN` on any seat0 keyboard.
    FnKey {
        held: bool,
    },
    /// Any other user input on seat0 (keyboard, pointer): keeps the bar awake.
    Activity,
    Lid {
        closed: bool,
    },
    /// The uid owning the active seat0 session changed (logind).
    SessionChanged {
        active_uid: Option<u32>,
    },
    /// [`Daemon::next_deadline`] has passed.
    Tick,
}

#[derive(Debug, PartialEq)]
pub enum Out {
    Send(ClientId, DaemonMsg),
    /// Close the connection. The daemon has already forgotten the client.
    Close(ClientId),
}

/// What a layer's pixels and regions come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Agent(Layer),
    Builtin(BuiltinKind),
}

struct Frame {
    image: Pixmap,
    pressed: Option<Pixmap>,
    regions: Vec<Region>,
}

struct Attached {
    width: u32,
    height: u32,
    buf: Box<dyn PixelSource>,
}

struct Client {
    uid: u32,
    said_hello: bool,
    buffers: HashMap<u32, Attached>,
    base: Option<Frame>,
    fn_layer: Option<Frame>,
    /// Keys held down by `emit_keys` `down`, released if the agent goes away.
    held_keys: Vec<u16>,
    last_wake_ms: Option<u64>,
    emit_window: (u64, u32),
}

impl Client {
    fn new(uid: u32) -> Client {
        Client {
            uid,
            said_hello: false,
            buffers: HashMap::new(),
            base: None,
            fn_layer: None,
            held_keys: Vec::new(),
            last_wake_ms: None,
            emit_window: (0, 0),
        }
    }
}

#[derive(Default)]
struct Liveness {
    last_ping_ms: Option<u64>,
    /// `(nonce, deadline)`
    outstanding: Option<(u64, u64)>,
}

pub struct Daemon {
    hw: Backends,
    panel: PanelInfo,
    profile: DeviceProfile,
    renderer: Renderer,
    sheet: Stylesheet,
    fkeys: BuiltinLayer,
    media: BuiltinLayer,
    safe_base: BuiltinKind,
    safe_fn: BuiltinKind,
    clients: BTreeMap<ClientId, Client>,
    active: Option<ClientId>,
    active_uid: Option<u32>,
    liveness: Liveness,
    next_nonce: u64,
    router: TouchRouter,
    /// Whose regions the router's captures refer to: the driving agent, or `None` (built-in).
    touch_source: Option<ClientId>,
    /// Keys held by `emit_keys` (touch keys are refcounted by the router).
    keys: KeyState,
    scratch: Vec<u8>,
    fn_held: bool,
    /// Fn double-press + hold: built-in layer, whatever the agent does (invariant 1).
    hatch: FnHatch,
    idle: Idle,
    backlight: Option<u32>,
    /// The composed frame.
    frame: Pixmap,
    /// What the panel shows. `None` until the first flush.
    shown: Option<Pixmap>,
    out: Vec<Out>,
}

impl Daemon {
    /// Starts with the safe layer on screen. `panel` comes from the display backend (logical
    /// size) plus sysfs (`max_brightness`) and DMI (`model`).
    pub fn new(
        hw: Backends,
        panel: PanelInfo,
        config: &Config,
        active_uid: Option<u32>,
        now: u64,
    ) -> Daemon {
        let profile = DeviceProfile::detect(
            &panel.model,
            panel.width,
            panel.max_brightness,
            config.physical_esc,
        );
        let mut renderer = Renderer::new(Fonts::embedded());
        let sheet = Stylesheet::classic();
        let (w, h) = (panel.width, panel.height);
        let esc = !profile.physical_esc;
        let fkeys = BuiltinLayer::new(BuiltinKind::FKeys, esc, &mut renderer, &sheet, w, h);
        let media = BuiltinLayer::new(BuiltinKind::Media, esc, &mut renderer, &sheet, w, h);
        let (safe_base, safe_fn) = if config.media_layer_default {
            (BuiltinKind::Media, BuiltinKind::FKeys)
        } else {
            (BuiltinKind::FKeys, BuiltinKind::Media)
        };
        let idle = Idle::new(
            panel.max_brightness,
            profile.active_brightness,
            profile.dim_brightness,
            now,
        );
        let frame = Pixmap::new(w, h).expect("non-empty panel");
        let mut d = Daemon {
            hw,
            panel,
            profile,
            renderer,
            sheet,
            fkeys,
            media,
            safe_base,
            safe_fn,
            clients: BTreeMap::new(),
            active: None,
            active_uid,
            liveness: Liveness::default(),
            next_nonce: 0,
            router: TouchRouter::new(),
            touch_source: None,
            keys: KeyState::default(),
            scratch: Vec::new(),
            fn_held: false,
            hatch: FnHatch::new(),
            idle,
            backlight: None,
            frame,
            shown: None,
            out: Vec::new(),
        };
        d.recompose();
        d.apply_backlight();
        d
    }

    pub fn panel(&self) -> &PanelInfo {
        &self.panel
    }

    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }

    pub fn display_state(&self) -> DisplayState {
        self.idle.state()
    }

    /// The connection currently driving the bar, if any.
    pub fn active_client(&self) -> Option<ClientId> {
        self.active
    }

    /// Whether the agent's frames are on screen (as opposed to the built-in safe layer).
    pub fn agent_drives(&self) -> bool {
        matches!(self.layers().0, View::Agent(_))
    }

    /// The layer on screen right now.
    pub fn shown_view(&self) -> View {
        if self.hatch.active() {
            return View::Builtin(BuiltinKind::FKeys);
        }
        let (base, fn_layer) = self.layers();
        if self.fn_held { fn_layer } else { base }
    }

    /// Whether the safe hatch is open.
    pub fn hatch_active(&self) -> bool {
        self.hatch.active()
    }

    /// The composed frame (what the panel shows, unless the display is off).
    pub fn frame(&self) -> &Pixmap {
        &self.frame
    }

    /// When to deliver the next [`Input::Tick`], if ever. `None` means sleep until input.
    pub fn next_deadline(&self) -> Option<u64> {
        let pong = self.liveness.outstanding.map(|(_, at)| at);
        [self.idle.deadline(), pong].into_iter().flatten().min()
    }

    pub fn handle(&mut self, now: u64, input: Input) -> Vec<Out> {
        self.run_timers(now);
        match input {
            Input::Connected { client, uid } => self.connected(client, uid),
            Input::Packet { client, msg, fd } => self.packet(now, client, msg, fd),
            Input::Disconnected { client } => {
                if self.active == Some(client) {
                    self.drop_agent();
                }
                self.clients.remove(&client);
            }
            Input::Touch(ev) => self.touch(now, ev),
            Input::FnKey { held } => self.fn_key(now, held),
            Input::Activity => self.activity(now),
            Input::Lid { closed } => {
                let s = self.idle.set_lid(closed, now);
                self.display_changed(now, s);
            }
            Input::SessionChanged { active_uid } => self.session_changed(now, active_uid),
            Input::Tick => {}
        }
        std::mem::take(&mut self.out)
    }

    // ---- timers -------------------------------------------------------------------------------

    fn run_timers(&mut self, now: u64) {
        let hung = self.liveness.outstanding.is_some_and(|(_, at)| now >= at);
        if let (true, Some(c)) = (hung, self.active) {
            // Hung agent: safe layer at once. It's disconnected, never followed.
            self.error(c, ErrorCode::Other, "no pong in time".into(), None);
            self.close(c);
        }
        let before = self.idle.state();
        let after = self.idle.tick(now);
        if after != before {
            self.display_changed(now, after);
        }
    }

    fn fn_key(&mut self, now: u64, held: bool) {
        self.activity(now);
        let was = self.hatch.active();
        let open = self.hatch.fn_key(held, now);
        if open != was {
            // The visible layer changes owner: nothing captured on the old one stays pressed.
            let routed = self.router.cancel_all();
            self.route(now, routed, 0);
            if let Some(c) = self.active {
                self.send(c, DaemonMsg::Hatch { active: open });
            }
        }
        if self.fn_held != held || open != was {
            self.fn_held = held;
            if let Some(c) = self.active {
                self.send(c, DaemonMsg::FnKey { held });
            }
            self.recompose();
        }
    }

    fn activity(&mut self, now: u64) {
        let before = self.idle.state();
        let after = self.idle.activity(now);
        if after != before {
            self.display_changed(now, after);
        } else if self.idle.follow_screen() {
            self.apply_backlight();
        }
    }

    fn display_changed(&mut self, now: u64, state: DisplayState) {
        self.apply_backlight();
        if let Some(c) = self.active {
            self.send(c, DaemonMsg::Display { state });
        }
        if state == DisplayState::Off {
            // Nobody holds a key on a dark bar.
            let routed = self.router.cancel_all();
            self.route(now, routed, 0);
        } else {
            self.flush(); // whatever changed while off
        }
        if state == DisplayState::On {
            self.ping(now, true);
        }
    }

    fn apply_backlight(&mut self) {
        let screen = if self.idle.follow_screen() {
            self.hw.backlight.screen_fraction()
        } else {
            None
        };
        let v = self.idle.brightness(screen);
        if self.backlight != Some(v) {
            self.backlight = Some(v);
            self.hw.backlight.set(v);
        }
    }

    /// Pings the active agent unless a ping is outstanding. Without `force`, at most every
    /// [`PING_MIN_INTERVAL_MS`].
    fn ping(&mut self, now: u64, force: bool) {
        let Some(c) = self.active else { return };
        if self.liveness.outstanding.is_some() {
            return;
        }
        if !force
            && self
                .liveness
                .last_ping_ms
                .is_some_and(|t| now.saturating_sub(t) < PING_MIN_INTERVAL_MS)
        {
            return;
        }
        self.next_nonce += 1;
        let nonce = self.next_nonce;
        self.liveness.outstanding = Some((nonce, now + PONG_TIMEOUT_MS));
        self.liveness.last_ping_ms = Some(now);
        self.send(c, DaemonMsg::Ping { nonce });
    }

    // ---- connections --------------------------------------------------------------------------

    fn connected(&mut self, client: ClientId, uid: u32) {
        if self.clients.len() >= MAX_CLIENTS {
            self.out.push(Out::Close(client));
            return;
        }
        self.clients.insert(client, Client::new(uid));
    }

    fn send(&mut self, client: ClientId, msg: DaemonMsg) {
        self.out.push(Out::Send(client, msg));
    }

    fn error(&mut self, client: ClientId, code: ErrorCode, msg: String, serial: Option<u32>) {
        self.send(client, DaemonMsg::Error { code, msg, serial });
    }

    fn close(&mut self, client: ClientId) {
        if self.active == Some(client) {
            self.drop_agent();
        }
        if self.clients.remove(&client).is_some() {
            self.out.push(Out::Close(client));
        }
    }

    /// The active agent is gone or demoted: release what it held and show the safe layer.
    fn drop_agent(&mut self) {
        let Some(old) = self.active.take() else {
            return;
        };
        self.liveness = Liveness::default();
        if let Some(c) = self.clients.get_mut(&old) {
            // Demoted, still connected: frames don't carry over to its next promotion.
            c.base = None;
            c.fn_layer = None;
            let held = std::mem::take(&mut c.held_keys);
            self.keys.release(&held, &mut *self.hw.keys);
        }
        self.recompose();
    }

    fn promote(&mut self, client: ClientId) {
        self.active = Some(client);
        self.liveness = Liveness::default();
        let state = self.idle.state();
        if state != DisplayState::On {
            self.send(client, DaemonMsg::Display { state });
        }
        if self.hatch.active() {
            self.send(client, DaemonMsg::Hatch { active: true });
        }
        if self.fn_held {
            self.send(client, DaemonMsg::FnKey { held: true });
        }
    }

    fn session_changed(&mut self, now: u64, uid: Option<u32>) {
        let _ = now;
        self.active_uid = uid;
        let demote = self
            .active
            .filter(|c| self.clients.get(c).map(|c| c.uid) != uid);
        if let Some(c) = demote {
            self.send(
                c,
                DaemonMsg::Role {
                    role: Role::Standby,
                },
            );
            self.drop_agent();
        }
        if self.active.is_none() {
            // The newest eligible connection takes over.
            let next = self
                .clients
                .iter()
                .rev()
                .find(|(_, c)| c.said_hello && Some(c.uid) == uid)
                .map(|(id, _)| *id);
            if let Some(next) = next {
                self.send(next, DaemonMsg::Role { role: Role::Active });
                self.promote(next);
            }
        }
    }

    fn packet(
        &mut self,
        now: u64,
        client: ClientId,
        msg: Result<AgentMsg, CodecError>,
        fd: Attachment,
    ) {
        let Some(c) = self.clients.get(&client) else {
            return;
        };
        let msg = match msg {
            Ok(m) => m,
            Err(e @ CodecError::UnknownMessage(_)) => {
                return self.error(client, e.code(), e.to_string(), None);
            }
            Err(e) => {
                // Garbage on the wire: don't try to follow it.
                self.error(client, e.code(), e.to_string(), None);
                return self.close(client);
            }
        };
        if !c.said_hello && !matches!(msg, AgentMsg::Hello { .. }) {
            self.error(client, ErrorCode::Malformed, "hello first".into(), None);
            return self.close(client);
        }
        let is_active = self.active == Some(client);
        match msg {
            AgentMsg::Hello { proto, .. } => self.hello(client, proto),
            AgentMsg::AttachBuffer {
                id,
                width,
                height,
                stride,
                format,
            } => self.attach(client, id, width, height, stride, format, fd),
            AgentMsg::DetachBuffer { id } => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.buffers.remove(&id);
                }
            }
            AgentMsg::Present(p) if is_active => self.present(client, p),
            AgentMsg::Present(p) => self.error(
                client,
                ErrorCode::NotActive,
                "standby".into(),
                Some(p.serial),
            ),
            AgentMsg::ClearLayer { layer: Layer::Fn } if is_active => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.fn_layer = None;
                }
                self.recompose();
            }
            AgentMsg::ClearLayer { layer: Layer::Base } => self.error(
                client,
                ErrorCode::InvalidFrame,
                "only the fn layer can be cleared".into(),
                None,
            ),
            AgentMsg::DisplayPolicy(p) if is_active => {
                let before = self.idle.state();
                let after = self.idle.set_policy(&p, now);
                if after != before {
                    self.display_changed(now, after);
                } else {
                    self.apply_backlight();
                }
            }
            AgentMsg::Wake if is_active => {
                let c = self.clients.get_mut(&client).expect("checked");
                if c.last_wake_ms
                    .is_some_and(|t| now.saturating_sub(t) < WAKE_MIN_INTERVAL_MS)
                {
                    self.error(client, ErrorCode::RateLimited, "wake".into(), None);
                } else {
                    c.last_wake_ms = Some(now);
                    self.activity(now);
                }
            }
            AgentMsg::EmitKeys { keys, mode } if is_active => {
                self.emit_keys(now, client, keys, mode)
            }
            AgentMsg::Pong { nonce } => {
                if is_active && self.liveness.outstanding.is_some_and(|(n, _)| n == nonce) {
                    self.liveness.outstanding = None;
                }
            }
            AgentMsg::Bye => self.close(client),
            AgentMsg::ClearLayer { .. }
            | AgentMsg::DisplayPolicy(_)
            | AgentMsg::Wake
            | AgentMsg::EmitKeys { .. } => {
                self.error(client, ErrorCode::NotActive, "standby".into(), None)
            }
        }
    }

    fn hello(&mut self, client: ClientId, proto: sliver_ipc::proto::ProtoVersion) {
        let c = self.clients.get_mut(&client).expect("checked");
        if c.said_hello {
            return self.error(client, ErrorCode::Malformed, "duplicate hello".into(), None);
        }
        let Some(version) = PROTO.negotiate(proto) else {
            self.error(
                client,
                ErrorCode::Proto,
                format!("sliverd speaks {:?}", PROTO),
                None,
            );
            return self.close(client);
        };
        c.said_hello = true;
        let eligible = Some(c.uid) == self.active_uid;
        let role = if eligible {
            Role::Active
        } else {
            Role::Standby
        };
        if let (true, Some(old)) = (eligible, self.active) {
            self.send(
                old,
                DaemonMsg::Role {
                    role: Role::Revoked,
                },
            );
            self.close(old);
        }
        self.send(
            client,
            DaemonMsg::Welcome {
                proto: version,
                panel: self.panel.clone(),
                role,
                features: Vec::new(),
            },
        );
        if eligible {
            self.promote(client);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn attach(
        &mut self,
        client: ClientId,
        id: u32,
        width: u32,
        height: u32,
        stride: u32,
        format: sliver_ipc::proto::PixelFormat,
        fd: Attachment,
    ) {
        let c = self.clients.get_mut(&client).expect("checked");
        let res = match fd {
            Attachment::None => Err("attach_buffer without an fd".to_string()),
            Attachment::Rejected(why) => Err(why),
            Attachment::Buffer(_) if c.buffers.contains_key(&id) => {
                Err("buffer id in use".to_string())
            }
            Attachment::Buffer(buf) => validate::validate_buffer(
                &self.panel,
                c.buffers.len(),
                width,
                height,
                stride,
                format,
            )
            .map(|()| {
                c.buffers.insert(id, Attached { width, height, buf });
            })
            .map_err(|e| e.to_string()),
        };
        if let Err(msg) = res {
            self.error(client, ErrorCode::InvalidBuffer, msg, None);
        }
    }

    fn present(&mut self, client: ClientId, p: Present) {
        let serial = Some(p.serial);
        if let Err(e) = validate::validate_present(&self.panel, &p) {
            return self.error(client, e.code(), e.to_string(), serial);
        }
        let c = self.clients.get_mut(&client).expect("checked");
        let scratch = &mut self.scratch;
        let mut copy = |id: u32| c.buffers.get(&id).and_then(|a| copy_buffer(a, scratch));
        let Some(image) = copy(p.image) else {
            return self.error(
                client,
                ErrorCode::InvalidFrame,
                "unknown buffer".into(),
                serial,
            );
        };
        let pressed = match p.pressed {
            None => None,
            Some(id) => match copy(id) {
                Some(px) => Some(px),
                None => {
                    return self.error(
                        client,
                        ErrorCode::InvalidFrame,
                        "unknown pressed buffer".into(),
                        serial,
                    );
                }
            },
        };
        let frame = Frame {
            image,
            pressed,
            regions: p.regions,
        };
        // Layouts are fully editable (H4 revised): no safe-key check. The hatch covers invariant 1.
        match p.layer {
            Layer::Base => c.base = Some(frame),
            Layer::Fn => c.fn_layer = Some(frame),
        }
        self.send(client, DaemonMsg::Presented { serial: p.serial });
        self.recompose();
    }

    fn emit_keys(&mut self, now: u64, client: ClientId, keys: Vec<u16>, mode: KeyMode) {
        if let Err(e) = validate::validate_keys(&keys) {
            return self.error(client, e.code(), e.to_string(), None);
        }
        let c = self.clients.get_mut(&client).expect("checked");
        let (start, n) = c.emit_window;
        if now.saturating_sub(start) >= 1_000 {
            c.emit_window = (now, 1);
        } else if n >= EMIT_KEYS_PER_SEC {
            return self.error(client, ErrorCode::RateLimited, "emit_keys".into(), None);
        } else {
            c.emit_window.1 += 1;
        }
        let out = &mut *self.hw.keys;
        match mode {
            KeyMode::Tap => {
                self.keys.press(&keys, out);
                self.keys.release(&keys, out);
            }
            KeyMode::Down => {
                self.keys.press(&keys, out);
                c.held_keys.extend(&keys);
            }
            KeyMode::Up => {
                // Only what this agent holds: it can't release a finger's key.
                let mut mine = Vec::new();
                for k in &keys {
                    if let Some(i) = c.held_keys.iter().rposition(|h| h == k) {
                        c.held_keys.remove(i);
                        mine.push(*k);
                    }
                }
                self.keys.release(&mine, out);
            }
        }
    }

    // ---- touch --------------------------------------------------------------------------------

    fn touch(&mut self, now: u64, ev: TouchEvent) {
        let was_off = self.idle.state() == DisplayState::Off;
        self.activity(now);
        if was_off && ev.phase == TouchPhase::Down {
            // The touch that wakes the bar is swallowed. The router never sees its down, so the
            // rest of it is ignored too.
            return;
        }
        let regions = match self.shown_view() {
            View::Agent(l) => {
                let c = self.active.and_then(|c| self.clients.get(&c));
                let f = c.and_then(|c| match l {
                    Layer::Base => c.base.as_ref(),
                    Layer::Fn => c.fn_layer.as_ref(),
                });
                f.map_or(&[][..], |f| &f.regions)
            }
            View::Builtin(BuiltinKind::FKeys) => &self.fkeys.regions,
            View::Builtin(BuiltinKind::Media) => &self.media.regions,
        };
        let routed = self.router.touch(regions, ev.slot, ev.phase, ev.x, ev.y);
        self.route(now, routed, ev.time_us);
    }

    /// Carries out the router's decisions: keys to uinput (one sync per batch), agent touches to
    /// the agent. Redraws if keys changed (pressed feedback).
    fn route(&mut self, now: u64, routed: Vec<Routed>, time_us: u64) {
        let mut keys = false;
        for r in routed {
            match r {
                Routed::Key { code, down } => {
                    self.hw.keys.key(code, down);
                    keys = true;
                }
                Routed::Agent {
                    region,
                    slot,
                    phase,
                    x,
                    y,
                } => {
                    let Some(c) = self.active else { continue };
                    let msg = DaemonMsg::Touch {
                        region,
                        slot,
                        phase,
                        x,
                        y,
                        time_us,
                    };
                    self.send(c, msg);
                    if phase == TouchPhase::Down {
                        self.ping(now, false);
                    }
                }
            }
        }
        if keys {
            self.hw.keys.sync();
            self.recompose();
        }
    }

    // ---- frames -------------------------------------------------------------------------------

    /// `(base, fn)` views. The agent drives once it has presented a base layer.
    fn layers(&self) -> (View, View) {
        let agent = self.active.and_then(|c| self.clients.get(&c));
        match agent {
            Some(c) if c.base.is_some() => (
                View::Agent(Layer::Base),
                if c.fn_layer.is_some() {
                    View::Agent(Layer::Fn)
                } else {
                    View::Builtin(BuiltinKind::FKeys)
                },
            ),
            _ => (View::Builtin(self.safe_base), View::Builtin(self.safe_fn)),
        }
    }

    fn agent_frame(&self, layer: Layer) -> Option<&Frame> {
        let c = self.clients.get(&self.active?)?;
        match layer {
            Layer::Base => c.base.as_ref(),
            Layer::Fn => c.fn_layer.as_ref(),
        }
    }

    fn builtin(&self, kind: BuiltinKind) -> &BuiltinLayer {
        match kind {
            BuiltinKind::FKeys => &self.fkeys,
            BuiltinKind::Media => &self.media,
        }
    }

    fn regions(&self, view: View) -> &[Region] {
        match view {
            View::Agent(l) => self.agent_frame(l).map_or(&[], |f| &f.regions),
            View::Builtin(k) => &self.builtin(k).regions,
        }
    }

    /// Regions of `view` currently pressed by a finger (same id and rect as when touched).
    fn pressed_regions(&self, view: View) -> Vec<Region> {
        let regions = self.regions(view);
        self.router
            .pressed_keys()
            .into_iter()
            .filter_map(|(id, rect)| regions.iter().find(|r| r.id == id && r.rect == rect))
            .cloned()
            .collect()
    }

    /// Rebuilds the frame from the shown layer plus pressed feedback, then flushes the damage.
    fn recompose(&mut self) {
        let source = (self.agent_drives() && !self.hatch.active())
            .then_some(self.active)
            .flatten();
        if source != self.touch_source {
            // Captures refer to the old source's regions: release everything.
            self.touch_source = source;
            let routed = self.router.cancel_all();
            let mut released = false;
            for r in routed {
                if let Routed::Key { code, down } = r {
                    self.hw.keys.key(code, down);
                    released = true;
                }
            }
            if released {
                self.hw.keys.sync();
            }
        }
        let view = self.shown_view();
        let pressed = self.pressed_regions(view);
        match view {
            View::Builtin(kind) => {
                let layer = match kind {
                    BuiltinKind::FKeys => &self.fkeys,
                    BuiltinKind::Media => &self.media,
                };
                if pressed.is_empty() {
                    self.frame.data_mut().copy_from_slice(layer.image.data());
                } else {
                    let ids: Vec<u32> = pressed.iter().map(|r| r.id).collect();
                    layer.render_pressed(&ids, &mut self.renderer, &self.sheet, &mut self.frame);
                }
            }
            View::Agent(layer) => {
                let c = self.active.and_then(|c| self.clients.get(&c));
                let f = c.and_then(|c| match layer {
                    Layer::Base => c.base.as_ref(),
                    Layer::Fn => c.fn_layer.as_ref(),
                });
                let f = f.expect("agent view implies a frame");
                self.frame.data_mut().copy_from_slice(f.image.data());
                for r in &pressed {
                    match &f.pressed {
                        Some(p) => copy_rect(p, &mut self.frame, r.rect),
                        None => highlight_rect(&mut self.frame, r.rect),
                    }
                }
            }
        }
        self.flush();
    }

    fn flush(&mut self) {
        if self.idle.state() == DisplayState::Off {
            return; // flushed on wake
        }
        let damage = match &self.shown {
            None => vec![Span {
                x: 0,
                w: self.frame.width(),
            }],
            Some(prev) => panel::damage(prev, &self.frame, DAMAGE_MERGE_GAP),
        };
        if damage.is_empty() {
            return;
        }
        self.hw.display.flush(&self.frame, &damage);
        match &mut self.shown {
            Some(s) => s.data_mut().copy_from_slice(self.frame.data()),
            None => self.shown = Some(self.frame.clone()),
        }
    }
}

/// Copies an XRGB8888 agent buffer into a pixmap. `None` if it's somehow the wrong size.
fn copy_buffer(a: &Attached, scratch: &mut Vec<u8>) -> Option<Pixmap> {
    let mut px = Pixmap::new(a.width, a.height)?;
    a.buf.copy_tight(scratch);
    if scratch.len() != px.data().len() {
        return None;
    }
    for (o, s) in px
        .data_mut()
        .chunks_exact_mut(4)
        .zip(scratch.chunks_exact(4))
    {
        o.copy_from_slice(&[s[2], s[1], s[0], 255]);
    }
    Some(px)
}

fn rect_rows(dst: &Pixmap, r: Rect) -> (usize, usize, usize, usize) {
    let (w, h) = (dst.width(), dst.height());
    let x0 = r.x.min(w) as usize;
    let x1 = (r.right().min(u64::from(w))) as usize;
    let y0 = r.y.min(h) as usize;
    let y1 = (r.bottom().min(u64::from(h))) as usize;
    (x0, x1, y0, y1)
}

fn copy_rect(src: &Pixmap, dst: &mut Pixmap, r: Rect) {
    let (x0, x1, y0, y1) = rect_rows(dst, r);
    let row = dst.width() as usize * 4;
    let s = src.data();
    let d = dst.data_mut();
    for y in y0..y1 {
        let range = y * row + x0 * 4..y * row + x1 * 4;
        d[range.clone()].copy_from_slice(&s[range]);
    }
}

/// Generic pressed feedback when the agent sent no pressed image: lighten by 25%.
fn highlight_rect(dst: &mut Pixmap, r: Rect) {
    let (x0, x1, y0, y1) = rect_rows(dst, r);
    let row = dst.width() as usize * 4;
    let d = dst.data_mut();
    for y in y0..y1 {
        for px in d[y * row + x0 * 4..y * row + x1 * 4].chunks_exact_mut(4) {
            for c in &mut px[..3] {
                *c += (255 - *c) / 4;
            }
        }
    }
}
