//! The daemon core: a sans-IO state machine.
//!
//! The event loop feeds it [`Input`]s with a monotonic timestamp in milliseconds, arms one timer
//! for [`Daemon::next_deadline`], and carries out the returned [`Out`]s on the agent sockets.
//! Hardware effects (frames, backlight, keys) go straight to the [`Backends`]. Nothing here
//! blocks, sleeps or polls, so tests drive it with simulated time and fake hardware.
//!
//! The rules it keeps:
//! - **Who drives.** Only connections of the seat's active uid become active; a newer hello from
//!   that uid replaces the old connection, and others wait in standby (their presents are
//!   answered `not_active`). The agent drives once it has presented a `base` frame. Frames are
//!   never refused for lacking Esc or F-keys: the safe hatch covers that.
//! - **Touch.** Any change of frame source (agent ⇄ built-in, one agent to another, the hatch)
//!   and the display turning off cancel every touch and release every held key. After the agent's
//!   frames vanish, touch-downs are ignored for [`AGENT_LOSS_SETTLE_MS`], so a tap aimed at an
//!   agent button can't press the safe-layer key that appears under it. A pressed region is lit
//!   only if both its id and its rect still match what the finger went down on.
//! - **Faults.** An unknown message is an error and the connection carries on. A malformed packet,
//!   a missing hello, a protocol major mismatch, `bye`, a pong timeout or a hangup closes it and
//!   brings back the safe layer. An invalid frame is an error and the previous frame stays.
//! - **Liveness.** A ping goes out when the display wakes and on an agent-region touch-down after
//!   5 s without one. The pong deadline is the only liveness timer, armed only while a ping waits.
//! - **`emit_keys`.** Validated (SysRq refused) and capped at [`EMIT_KEYS_PER_SEC`]. Keys an agent
//!   holds are released when it goes, and are counted apart from touch keys, so a finger and the
//!   agent both holding Ctrl are two holds.

use std::collections::{BTreeMap, HashMap};

use crate::hatch::FnHatch;
use crate::router::{Routed, TouchRouter};
use sliver_ipc::proto::{
    CodecError, DaemonMsg, DisplayState, ErrorCode, HwReport, KeyMode, Layer, PING_MIN_INTERVAL_MS,
    PONG_TIMEOUT_MS, PROTO, PanelInfo, Present, Rect, Region, Role, TouchPhase,
};
use sliver_ipc::{AgentMsg, validate};
use sliver_render::panel::{self, Span};
use sliver_render::{Fonts, Pixmap, Renderer, Stylesheet};

use crate::backend::{Backends, PixelSource, TouchEvent};
use crate::builtin::{BuiltinKind, BuiltinLayer};
use crate::idle::Idle;
use crate::ipc::Attachment;
use crate::keystate::KeyState;
use crate::profile::DeviceProfile;
use crate::shift::PixelShift;

mod clients;
mod display;
mod frames;
mod input;

/// Identifies one agent connection. Assigned by the transport, increasing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u64);

/// Bounded everything: connections beyond this are closed at once.
pub const MAX_CLIENTS: usize = 8;
/// `emit_keys` messages per second per client.
pub const EMIT_KEYS_PER_SEC: u32 = 100;
/// `wake` at most this often.
pub const WAKE_MIN_INTERVAL_MS: u64 = 1_000;
/// After the agent's frames vanish (crash, hang, demotion) touch-downs are ignored this long: a
/// finger landing just after was aiming at an agent button, not at whatever safe-layer key (an
/// F-key) now sits under it.
pub const AGENT_LOSS_SETTLE_MS: u64 = 200;
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
    /// The main display's brightness changed: re-apply the bar's if it follows the screen.
    ScreenBacklight,
    /// The uid owning the active seat0 session changed (logind).
    SessionChanged {
        active_uid: Option<u32>,
    },
    /// [`Daemon::next_deadline`] has passed.
    Tick,
    /// The machine was suspended (the server notices on its next wakeup). s2idle may have reset
    /// the panel or its backlight behind our back: repaint everything and write the backlight
    /// again, even if nothing changed on our side.
    Resumed,
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

impl View {
    /// Stable name for the wire (`hw_report.view`), independent of the Rust names.
    pub fn wire_name(self) -> &'static str {
        match self {
            View::Agent(Layer::Base) => "agent:base",
            View::Agent(Layer::Fn) => "agent:fn",
            View::Builtin(BuiltinKind::FKeys) => "builtin:fkeys",
            View::Builtin(BuiltinKind::Media) => "builtin:media",
        }
    }
}

/// An agent frame as sliverd copied it.
pub struct Frame {
    pub serial: u32,
    pub image: Pixmap,
    pub pressed: Option<Pixmap>,
    pub regions: Vec<Region>,
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
    /// Latest time seen by [`Daemon::handle`].
    now: u64,
    /// Touch-downs before this are swallowed ([`AGENT_LOSS_SETTLE_MS`]).
    settle_until: u64,
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
    /// Pixel shift: where the composed frame is moved to on the panel.
    shift: PixelShift,
    /// Scratch for the shifted frame (reused, only while shifting).
    shifted: Option<Pixmap>,
    out: Vec<Out>,
    /// What was found and opened at startup, for `report`. Live fields are filled in per ask.
    facts: HwReport,
    started: u64,
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
            now,
            settle_until: 0,
            keys: KeyState::default(),
            scratch: Vec::new(),
            fn_held: false,
            hatch: FnHatch::new(),
            idle,
            backlight: None,
            frame,
            shown: None,
            // Different starting point per run, like tiny-dfr's random start.
            shift: PixelShift::new(now ^ u64::from(std::process::id()).rotate_left(17)),
            shifted: None,
            out: Vec::new(),
            facts: HwReport::default(),
            started: now,
        };
        d.recompose();
        d.apply_backlight();
        d
    }

    pub fn panel(&self) -> &PanelInfo {
        &self.panel
    }

    /// The startup facts for `report` answers (backend, gate, devices). Model and size come from
    /// the panel.
    pub fn set_facts(&mut self, facts: HwReport) {
        self.facts = facts;
    }

    /// The `hw_report` answer: the startup facts plus the state right now.
    pub fn hw_report(&self) -> HwReport {
        HwReport {
            version: env!("CARGO_PKG_VERSION").into(),
            model: self.panel.model.clone(),
            width: self.panel.width,
            height: self.panel.height,
            max_brightness: self.panel.max_brightness,
            uptime_ms: self.now.saturating_sub(self.started),
            display: Some(self.idle.state()),
            brightness: self.backlight,
            view: self.shown_view().wire_name().into(),
            hatch: self.hatch.active(),
            pixel_shift: self.shift.enabled(),
            active_uid: self.active_uid,
            agent_uid: self
                .active
                .and_then(|a| self.clients.get(&a))
                .map(|c| c.uid),
            clients: self.clients.len() as u32,
            ..self.facts.clone()
        }
    }

    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }

    pub fn display_state(&self) -> DisplayState {
        self.idle.state()
    }

    /// Pixel shift's current `(dx, dy)`: how far the picture is moved on the panel.
    pub fn pixel_shift(&self) -> (i32, i32) {
        self.shift.offset()
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
        let shift = self.shift.deadline(self.idle.state() != DisplayState::Off);
        [self.idle.deadline(), pong, shift]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn handle(&mut self, now: u64, input: Input) -> Vec<Out> {
        self.now = self.now.max(now);
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
            Input::ScreenBacklight => {
                if self.idle.follow_screen() {
                    self.apply_backlight();
                }
            }
            Input::SessionChanged { active_uid } => self.session_changed(now, active_uid),
            Input::Tick => {}
            Input::Resumed => {
                self.shown = None;
                self.backlight = None;
                self.recompose();
                self.apply_backlight();
            }
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
        if self.shift.tick(now, self.idle.state() != DisplayState::Off) {
            self.flush();
        }
    }
}
