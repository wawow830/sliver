//! sliverd ⇄ sliver-agent messages (`docs/api.md` §6).
//!
//! One message = one `SOCK_SEQPACKET` packet = one UTF-8 JSON object tagged by `"t"`.
//! Unknown fields are ignored (forward compatibility within a major version). Only
//! [`AgentMsg::AttachBuffer`] carries a file descriptor (`SCM_RIGHTS`).

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;

/// Protocol version spoken by this build.
pub const PROTO: ProtoVersion = ProtoVersion(1, 0);
/// Where sliverd listens.
pub const SOCKET_PATH: &str = "/run/sliver/sliverd.sock";
/// Maximum packet size, both directions.
pub const MAX_PACKET: usize = 64 * 1024;
/// Maximum buffers an agent may have attached at once.
pub const MAX_BUFFERS: usize = 8;
/// An agent that hasn't answered a ping within this time is considered hung.
pub const PONG_TIMEOUT_MS: u64 = 2_000;
/// sliverd pings on agent-region touches at most this often (plus once per display wake).
pub const PING_MIN_INTERVAL_MS: u64 = 5_000;

/// `[major, minor]`. Majors must match. Otherwise both sides use the lower minor.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProtoVersion(pub u16, pub u16);

impl ProtoVersion {
    /// The version both sides speak, or `None` if the majors differ.
    pub fn negotiate(self, other: ProtoVersion) -> Option<ProtoVersion> {
        (self.0 == other.0).then(|| ProtoVersion(self.0, self.1.min(other.1)))
    }
}

/// Rectangle in logical bar pixels (landscape, origin top-left as the user sees it).
/// On the wire: `[x, y, w, h]`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(from = "[u32; 4]", into = "[u32; 4]")]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> u64 {
        u64::from(self.x) + u64::from(self.w)
    }
    pub fn bottom(&self) -> u64 {
        u64::from(self.y) + u64::from(self.h)
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        u64::from(self.x) < o.right()
            && u64::from(o.x) < self.right()
            && u64::from(self.y) < o.bottom()
            && u64::from(o.y) < self.bottom()
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (x, y) = (f64::from(x), f64::from(y));
        x >= f64::from(self.x)
            && x < self.right() as f64
            && y >= f64::from(self.y)
            && y < self.bottom() as f64
    }
}

impl From<[u32; 4]> for Rect {
    fn from([x, y, w, h]: [u32; 4]) -> Self {
        Rect { x, y, w, h }
    }
}

impl From<Rect> for [u32; 4] {
    fn from(r: Rect) -> Self {
        [r.x, r.y, r.w, r.h]
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    /// DRM `XRGB8888`: little-endian 32-bit, bytes B, G, R, X.
    Xrgb8888,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Xrgb8888 => 4,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Shown normally.
    Base,
    /// Shown while Fn is held. sliverd swaps locally.
    Fn,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub id: u32,
    pub rect: Rect,
    #[serde(flatten)]
    pub kind: RegionKind,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RegionKind {
    /// sliverd hit-tests it and emits `keys` (pressed in order, released in reverse).
    Key { keys: Vec<u16> },
    /// Touches that start here go to the agent as [`DaemonMsg::Touch`].
    Agent,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Present {
    pub serial: u32,
    pub layer: Layer,
    /// Buffer id of the frame.
    pub image: u32,
    /// Buffer id of the same frame with every region drawn pressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressed: Option<u32>,
    pub regions: Vec<Region>,
    /// Changed horizontal spans `[x, w]`. `None` = everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage: Option<Vec<[u32; 2]>>,
}

/// Backlight/idle policy. Missing fields keep sliverd's current values.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dim_brightness: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_after_ms: Option<u64>,
    /// Scale with the main display's backlight (tiny-dfr `AdaptiveBrightness`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_screen: Option<bool>,
    /// Adaptive brightness curve: points `[screen, bar]` in per-mille (0–1000) of the main
    /// display's brightness and of `brightness`, piecewise-linear and flat beyond the ends. Empty =
    /// tiny-dfr's `sqrt`. sliverd normalises it (clamps, sorts, drops duplicate screens, keeps 16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Vec<[u16; 2]>>,
    /// Slowly move the whole picture by up to ±11 px x / ±2 px y against OLED burn-in (tiny-dfr
    /// `EnablePixelShift`). sliverd moves frames and touches; the agent keeps its content 11 px
    /// in from both ends while it's on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_shift: Option<bool>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyMode {
    Tap,
    Down,
    Up,
}

/// Agent → sliverd.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum AgentMsg {
    Hello {
        proto: ProtoVersion,
        agent: String,
    },
    /// Carries exactly one fd: a memfd sealed with `F_SEAL_SHRINK | F_SEAL_GROW`.
    AttachBuffer {
        id: u32,
        width: u32,
        height: u32,
        stride: u32,
        format: PixelFormat,
    },
    DetachBuffer {
        id: u32,
    },
    Present(Present),
    /// `fn` only: fall back to the built-in fn layer.
    ClearLayer {
        layer: Layer,
    },
    DisplayPolicy(DisplayPolicy),
    Wake,
    EmitKeys {
        keys: Vec<u16>,
        mode: KeyMode,
    },
    Pong {
        nonce: u64,
    },
    Bye,
}

impl AgentMsg {
    /// Tags of all variants this build understands.
    pub const TAGS: &'static [&'static str] = &[
        "hello",
        "attach_buffer",
        "detach_buffer",
        "present",
        "clear_layer",
        "display_policy",
        "wake",
        "emit_keys",
        "pong",
        "bye",
    ];

    /// Whether this message must arrive with exactly one fd attached.
    pub fn carries_fd(&self) -> bool {
        matches!(self, AgentMsg::AttachBuffer { .. })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PanelInfo {
    /// Logical (landscape) width, e.g. 2008.
    pub width: u32,
    /// Logical height, e.g. 60.
    pub height: u32,
    pub format: PixelFormat,
    pub max_brightness: u32,
    /// DMI product name, e.g. `Mac14,7`.
    pub model: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// This agent drives the bar.
    Active,
    /// Connected, but its uid doesn't own the active seat0 session. Presents are dropped.
    Standby,
    /// Replaced by a newer agent connection. sliverd closes the socket after sending this.
    Revoked,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DisplayState {
    On,
    Dimmed,
    Off,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Major version mismatch. sliverd closes the connection.
    Proto,
    UnknownMessage,
    Malformed,
    InvalidBuffer,
    InvalidFrame,
    ForbiddenKey,
    RateLimited,
    NotActive,
    /// A code from a newer peer.
    #[serde(other)]
    Other,
}

/// sliverd → agent.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DaemonMsg {
    Welcome {
        proto: ProtoVersion,
        panel: PanelInfo,
        role: Role,
        #[serde(default)]
        features: Vec<String>,
    },
    Presented {
        serial: u32,
    },
    /// Only for touches that started in an `agent` region. Captured until up/cancel.
    Touch {
        region: u32,
        slot: u8,
        phase: TouchPhase,
        x: f32,
        y: f32,
        time_us: u64,
    },
    #[serde(rename = "fn")]
    FnKey {
        held: bool,
    },
    /// The safe hatch (Fn double-press and hold) is showing the built-in layer instead of the
    /// agent's frames. Touches don't reach the agent while active.
    Hatch {
        active: bool,
    },
    Display {
        state: DisplayState,
    },
    Role {
        role: Role,
    },
    Ping {
        nonce: u64,
    },
    Error {
        code: ErrorCode,
        msg: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        serial: Option<u32>,
    },
}

impl DaemonMsg {
    pub const TAGS: &'static [&'static str] = &[
        "welcome",
        "presented",
        "touch",
        "fn",
        "hatch",
        "display",
        "role",
        "ping",
        "error",
    ];
}

#[derive(Debug, PartialEq, Eq)]
pub enum CodecError {
    TooLarge(usize),
    /// Valid JSON object with a `"t"` this build doesn't know. Reply `unknown_message`, carry on.
    UnknownMessage(String),
    Malformed(String),
}

impl CodecError {
    pub fn code(&self) -> ErrorCode {
        match self {
            CodecError::TooLarge(_) | CodecError::Malformed(_) => ErrorCode::Malformed,
            CodecError::UnknownMessage(_) => ErrorCode::UnknownMessage,
        }
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::TooLarge(n) => write!(f, "packet of {n} bytes exceeds {MAX_PACKET}"),
            CodecError::UnknownMessage(t) => write!(f, "unknown message type {t:?}"),
            CodecError::Malformed(e) => write!(f, "malformed message: {e}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Message types that can be encoded/decoded as packets.
pub trait Message: Serialize + DeserializeOwned {
    const TAGS: &'static [&'static str];
}

impl Message for AgentMsg {
    const TAGS: &'static [&'static str] = AgentMsg::TAGS;
}

impl Message for DaemonMsg {
    const TAGS: &'static [&'static str] = DaemonMsg::TAGS;
}

/// Serialize one message into one packet.
pub fn encode<M: Message>(msg: &M) -> Result<Vec<u8>, CodecError> {
    let buf = serde_json::to_vec(msg).map_err(|e| CodecError::Malformed(e.to_string()))?;
    if buf.len() > MAX_PACKET {
        return Err(CodecError::TooLarge(buf.len()));
    }
    Ok(buf)
}

/// Parse one packet. Distinguishes unknown-but-well-formed messages from garbage.
pub fn decode<M: Message>(packet: &[u8]) -> Result<M, CodecError> {
    if packet.len() > MAX_PACKET {
        return Err(CodecError::TooLarge(packet.len()));
    }
    let value: serde_json::Value =
        serde_json::from_slice(packet).map_err(|e| CodecError::Malformed(e.to_string()))?;
    let tag = value
        .get("t")
        .and_then(|t| t.as_str())
        .ok_or_else(|| CodecError::Malformed("missing string field \"t\"".into()))?;
    if !M::TAGS.contains(&tag) {
        return Err(CodecError::UnknownMessage(tag.to_owned()));
    }
    serde_json::from_value(value).map_err(|e| CodecError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire<M: Message>(msg: &M) -> serde_json::Value {
        serde_json::from_slice(&encode(msg).unwrap()).unwrap()
    }

    #[test]
    fn hello_matches_spec() {
        let m = AgentMsg::Hello {
            proto: PROTO,
            agent: "sliver-agent 0.1.0".into(),
        };
        assert_eq!(
            wire(&m),
            json!({"t":"hello","proto":[1,0],"agent":"sliver-agent 0.1.0"})
        );
    }

    #[test]
    fn present_matches_spec() {
        let m = AgentMsg::Present(Present {
            serial: 7,
            layer: Layer::Base,
            image: 1,
            pressed: Some(2),
            regions: vec![
                Region {
                    id: 1,
                    rect: Rect::new(0, 0, 100, 60),
                    kind: RegionKind::Key { keys: vec![1] },
                },
                Region {
                    id: 2,
                    rect: Rect::new(100, 0, 300, 60),
                    kind: RegionKind::Agent,
                },
            ],
            damage: None,
        });
        assert_eq!(
            wire(&m),
            json!({"t":"present","serial":7,"layer":"base","image":1,"pressed":2,"regions":[
                {"id":1,"rect":[0,0,100,60],"kind":"key","keys":[1]},
                {"id":2,"rect":[100,0,300,60],"kind":"agent"}]})
        );
    }

    #[test]
    fn daemon_messages_match_spec() {
        assert_eq!(
            wire(&DaemonMsg::FnKey { held: true }),
            json!({"t":"fn","held":true})
        );
        assert_eq!(
            wire(&DaemonMsg::Ping { nonce: 3 }),
            json!({"t":"ping","nonce":3})
        );
        let w = DaemonMsg::Welcome {
            proto: PROTO,
            panel: PanelInfo {
                width: 2008,
                height: 60,
                format: PixelFormat::Xrgb8888,
                max_brightness: 255,
                model: "Mac14,7".into(),
            },
            role: Role::Active,
            features: vec![],
        };
        assert_eq!(
            wire(&w),
            json!({"t":"welcome","proto":[1,0],"panel":{"width":2008,"height":60,"format":"xrgb8888",
                "max_brightness":255,"model":"Mac14,7"},"role":"active","features":[]})
        );
    }

    fn sample_present() -> AgentMsg {
        AgentMsg::Present(Present {
            serial: 7,
            layer: Layer::Base,
            image: 1,
            pressed: Some(2),
            regions: vec![
                Region {
                    id: 1,
                    rect: Rect::new(0, 0, 100, 60),
                    kind: RegionKind::Key { keys: vec![1] },
                },
                Region {
                    id: 2,
                    rect: Rect::new(100, 0, 300, 60),
                    kind: RegionKind::Agent,
                },
            ],
            damage: Some(vec![[0, 400]]),
        })
    }

    fn tag<M: Message>(m: &M) -> String {
        wire(m)["t"].as_str().unwrap().to_owned()
    }

    /// Every variant roundtrips, and `TAGS` lists exactly the variants' tags.
    #[test]
    fn every_variant_roundtrips() {
        let agent = [
            sample_present(),
            AgentMsg::Hello {
                proto: PROTO,
                agent: "x".into(),
            },
            AgentMsg::AttachBuffer {
                id: 1,
                width: 2008,
                height: 60,
                stride: 8032,
                format: PixelFormat::Xrgb8888,
            },
            AgentMsg::DetachBuffer { id: 1 },
            AgentMsg::ClearLayer { layer: Layer::Fn },
            AgentMsg::DisplayPolicy(DisplayPolicy {
                brightness: Some(100),
                ..Default::default()
            }),
            AgentMsg::Wake,
            AgentMsg::EmitKeys {
                keys: vec![29, 46],
                mode: KeyMode::Tap,
            },
            AgentMsg::Pong { nonce: 9 },
            AgentMsg::Bye,
        ];
        let mut tags: Vec<String> = agent.iter().map(tag).collect();
        tags.sort();
        let mut want: Vec<String> = AgentMsg::TAGS.iter().map(|t| t.to_string()).collect();
        want.sort();
        assert_eq!(tags, want);
        for m in agent {
            assert_eq!(decode::<AgentMsg>(&encode(&m).unwrap()).unwrap(), m);
        }
        let daemon = [
            DaemonMsg::Welcome {
                proto: PROTO,
                panel: PanelInfo {
                    width: 2008,
                    height: 60,
                    format: PixelFormat::Xrgb8888,
                    max_brightness: 255,
                    model: "m".into(),
                },
                role: Role::Active,
                features: vec!["x".into()],
            },
            DaemonMsg::FnKey { held: false },
            DaemonMsg::Hatch { active: true },
            DaemonMsg::Ping { nonce: 2 },
            DaemonMsg::Presented { serial: 1 },
            DaemonMsg::Touch {
                region: 2,
                slot: 0,
                phase: TouchPhase::Move,
                x: 10.5,
                y: 30.0,
                time_us: 5,
            },
            DaemonMsg::Display {
                state: DisplayState::Dimmed,
            },
            DaemonMsg::Role {
                role: Role::Standby,
            },
            DaemonMsg::Error {
                code: ErrorCode::InvalidFrame,
                msg: "no esc".into(),
                serial: Some(4),
            },
        ];
        let mut tags: Vec<String> = daemon.iter().map(tag).collect();
        tags.sort();
        let mut want: Vec<String> = DaemonMsg::TAGS.iter().map(|t| t.to_string()).collect();
        want.sort();
        assert_eq!(tags, want);
        for m in daemon {
            assert_eq!(decode::<DaemonMsg>(&encode(&m).unwrap()).unwrap(), m);
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let m: AgentMsg = decode(br#"{"t":"pong","nonce":1,"from_the_future":true}"#).unwrap();
        assert_eq!(m, AgentMsg::Pong { nonce: 1 });
    }

    #[test]
    fn unknown_type_is_distinguished_from_garbage() {
        assert_eq!(
            decode::<AgentMsg>(br#"{"t":"teleport"}"#),
            Err(CodecError::UnknownMessage("teleport".into()))
        );
        assert!(matches!(
            decode::<AgentMsg>(b"not json"),
            Err(CodecError::Malformed(_))
        ));
        assert!(matches!(
            decode::<AgentMsg>(br#"{"nonce":1}"#),
            Err(CodecError::Malformed(_))
        ));
        assert!(matches!(
            decode::<AgentMsg>(br#"{"t":"pong"}"#),
            Err(CodecError::Malformed(_))
        ));
        // A daemon tag sent by an agent is unknown to the agent→daemon direction.
        assert_eq!(
            decode::<AgentMsg>(br#"{"t":"ping","nonce":1}"#)
                .unwrap_err()
                .code(),
            ErrorCode::UnknownMessage
        );
    }

    #[test]
    fn unknown_error_code_from_newer_daemon() {
        let m: DaemonMsg = decode(br#"{"t":"error","code":"quantum_flux","msg":"?"}"#).unwrap();
        assert_eq!(
            m,
            DaemonMsg::Error {
                code: ErrorCode::Other,
                msg: "?".into(),
                serial: None
            }
        );
    }

    #[test]
    fn oversized_packets_rejected() {
        let big = vec![b' '; MAX_PACKET + 1];
        assert_eq!(
            decode::<AgentMsg>(&big),
            Err(CodecError::TooLarge(MAX_PACKET + 1))
        );
        let m = AgentMsg::EmitKeys {
            keys: vec![30; MAX_PACKET],
            mode: KeyMode::Tap,
        };
        assert!(matches!(encode(&m), Err(CodecError::TooLarge(_))));
    }

    #[test]
    fn version_negotiation() {
        assert_eq!(
            ProtoVersion(1, 3).negotiate(ProtoVersion(1, 1)),
            Some(ProtoVersion(1, 1))
        );
        assert_eq!(ProtoVersion(1, 0).negotiate(ProtoVersion(2, 0)), None);
    }

    #[test]
    fn rect_geometry() {
        let a = Rect::new(0, 0, 100, 60);
        assert!(a.intersects(&Rect::new(99, 0, 10, 60)));
        assert!(!a.intersects(&Rect::new(100, 0, 10, 60)));
        assert!(a.contains(99.9, 59.9));
        assert!(!a.contains(100.0, 0.0));
    }
}
