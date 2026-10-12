//! End-to-end tests of the daemon core on fake hardware: simulated touches through the key-region
//! semantics (docs/api.md §6), asserting emitted keys, agent messages and PNG frames.
//!
//! Snapshots live in `tests/snapshots/`. Look at them when they change.
//! Update after an intended change: `SLIVER_BLESS=1 cargo test -p sliverd --test daemon`.

use std::path::PathBuf;

use KeyEvent::{Down, Up};
use sliver_ipc::AgentMsg;
use sliver_ipc::keys::*;
use sliver_ipc::proto::{
    CodecError, DaemonMsg, DisplayPolicy, DisplayState, ErrorCode, KeyMode, Layer, PROTO,
    PanelInfo, PixelFormat, Present, ProtoVersion, Rect, Region, RegionKind, Role, TouchPhase,
};
use sliver_render::{Fonts, Item, Pixmap, Renderer, Scene, Stylesheet};
use sliverd::backend::TouchEvent;
use sliverd::builtin::{BuiltinKind, media_keys};
use sliverd::fake::{FakeHardware, FakeShm, KeyEvent};
use sliverd::ipc::Attachment;
use sliverd::{ClientId, Config, Daemon, Input, Out, View};

const UID: u32 = 1000;
const A: ClientId = ClientId(1);
const KEY_LEFTCTRL: u16 = 29;
const KEY_C: u16 = 46;

struct Rig {
    hw: FakeHardware,
    d: Daemon,
    now: u64,
}

impl Rig {
    fn new() -> Rig {
        Rig::with(2008, "Mac14,7", 255, Config::default())
    }

    fn with(width: u32, model: &str, max_brightness: u32, config: Config) -> Rig {
        let hw = FakeHardware::new(width, 60);
        let panel = PanelInfo {
            width,
            height: 60,
            format: PixelFormat::Xrgb8888,
            max_brightness,
            model: model.into(),
        };
        let d = Daemon::new(hw.backends(), panel, &config, Some(UID), 0);
        Rig { hw, d, now: 0 }
    }

    fn input(&mut self, i: Input) -> Vec<Out> {
        self.d.handle(self.now, i)
    }

    fn advance(&mut self, ms: u64) -> Vec<Out> {
        self.now += ms;
        self.input(Input::Tick)
    }

    fn touch(&mut self, slot: u8, phase: TouchPhase, x: f32) -> Vec<Out> {
        self.input(Input::Touch(TouchEvent {
            slot,
            phase,
            x,
            y: 30.0,
            time_us: self.now * 1000,
        }))
    }
    fn down(&mut self, slot: u8, x: f32) -> Vec<Out> {
        self.touch(slot, TouchPhase::Down, x)
    }
    fn mv(&mut self, slot: u8, x: f32) -> Vec<Out> {
        self.touch(slot, TouchPhase::Move, x)
    }
    fn up(&mut self, slot: u8, x: f32) -> Vec<Out> {
        self.touch(slot, TouchPhase::Up, x)
    }

    fn keys(&self) -> Vec<KeyEvent> {
        self.hw.keys.take()
    }

    fn send(&mut self, client: ClientId, msg: AgentMsg) -> Vec<Out> {
        self.input(Input::Packet {
            client,
            msg: Ok(msg),
            fd: Attachment::None,
        })
    }

    /// Connects `client` and says hello. Returns the daemon's replies.
    fn connect(&mut self, client: ClientId, uid: u32) -> Vec<Out> {
        assert!(self.input(Input::Connected { client, uid }).is_empty());
        self.send(
            client,
            AgentMsg::Hello {
                proto: PROTO,
                agent: "test".into(),
            },
        )
    }

    fn attach(&mut self, client: ClientId, id: u32, frame: &Pixmap) -> (FakeShm, Vec<Out>) {
        let shm = FakeShm::from_pixmap(frame);
        let out = self.input(Input::Packet {
            client,
            msg: Ok(AgentMsg::AttachBuffer {
                id,
                width: frame.width(),
                height: frame.height(),
                stride: frame.width() * 4,
                format: PixelFormat::Xrgb8888,
            }),
            fd: Attachment::Buffer(Box::new(shm.clone())),
        });
        (shm, out)
    }

    fn present(
        &mut self,
        client: ClientId,
        serial: u32,
        layer: Layer,
        image: u32,
        pressed: Option<u32>,
        regions: Vec<Region>,
    ) -> Vec<Out> {
        self.send(
            client,
            AgentMsg::Present(Present {
                serial,
                layer,
                image,
                pressed,
                regions,
                damage: None,
            }),
        )
    }
}

fn sent(out: &[Out], client: ClientId) -> Vec<DaemonMsg> {
    out.iter()
        .filter_map(|o| match o {
            Out::Send(c, m) if *c == client => Some(m.clone()),
            _ => None,
        })
        .collect()
}

fn closed(out: &[Out], client: ClientId) -> bool {
    out.contains(&Out::Close(client))
}

fn error_codes(out: &[Out], client: ClientId) -> Vec<ErrorCode> {
    sent(out, client)
        .into_iter()
        .filter_map(|m| match m {
            DaemonMsg::Error { code, .. } => Some(code),
            _ => None,
        })
        .collect()
}

/// Center x of slot `i` of `n` equal slots on a 2008 px bar (tiny-dfr spacing).
fn slot_x(i: usize, n: usize) -> f32 {
    let slot = (2008.0 - 16.0 * (n as f32 - 1.0)) / n as f32;
    i as f32 * (slot + 16.0) + slot / 2.0
}

/// Compares a frame with `tests/snapshots/NAME.png` (small tolerance for rasterizer drift).
fn check_png(name: &str, actual: &Pixmap) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("tests/snapshots").join(format!("{name}.png"));
    if std::env::var_os("SLIVER_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        actual.save_png(&path).unwrap();
        return;
    }
    let expected = Pixmap::load_png(&path).unwrap_or_else(|e| {
        panic!("{name}: no snapshot at {path:?} ({e}); run with SLIVER_BLESS=1")
    });
    let mismatched = if (expected.width(), expected.height()) != (actual.width(), actual.height()) {
        usize::MAX
    } else {
        expected
            .data()
            .chunks(4)
            .zip(actual.data().chunks(4))
            .filter(|(e, a)| e.iter().zip(a.iter()).any(|(e, a)| e.abs_diff(*a) > 2))
            .count()
    };
    if mismatched > 20 {
        let dir = root.join("../../target/snapshot-failures");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("sliverd-{name}.png"));
        actual.save_png(&out).unwrap();
        panic!("{name}: {mismatched} pixels differ from {path:?}; actual: {out:?}");
    }
}

/// An agent frame like the real agent would draw: "Copy" (Ctrl+C), a widget area the agent
/// handles itself, and a volume key. Returns (normal, pressed, regions).
fn agent_frame() -> (Pixmap, Pixmap, Vec<Region>) {
    let mut r = Renderer::new(Fonts::embedded());
    let sheet = Stylesheet::classic();
    let items = vec![
        Item::text("Copy").fill(1),
        Item::text("now playing").fill(4),
        Item::text("vol+").fill(1),
    ];
    let scene = Scene::new(items.clone());
    let normal = r.render_new(&scene, &sheet, 2008, 60);
    let pressed_scene = Scene::new(items.into_iter().map(|i| i.pressed(true)).collect());
    let pressed = r.render_new(&pressed_scene, &sheet, 2008, 60);
    let rects = r.layout(&scene, &sheet, 2008, 60).items;
    let rect = |i: usize| Rect::new(rects[i].x as u32, 0, rects[i].w as u32, 60);
    let regions = vec![
        Region {
            id: 10,
            rect: rect(0),
            kind: RegionKind::Key {
                keys: vec![KEY_LEFTCTRL, KEY_C],
            },
        },
        Region {
            id: 11,
            rect: rect(1),
            kind: RegionKind::Agent,
        },
        Region {
            id: 12,
            rect: rect(2),
            kind: RegionKind::Key {
                keys: vec![media_keys::VOLUME_UP],
            },
        },
    ];
    (normal, pressed, regions)
}

/// Connects agent `A` and puts [`agent_frame`] on screen (base layer, with a pressed image).
fn rig_with_agent() -> (Rig, Vec<Region>) {
    let mut rig = Rig::new();
    let out = rig.connect(A, UID);
    assert!(matches!(
        sent(&out, A)[..],
        [DaemonMsg::Welcome {
            role: Role::Active,
            ..
        }]
    ));
    let (normal, pressed, regions) = agent_frame();
    assert!(rig.attach(A, 1, &normal).1.is_empty());
    assert!(rig.attach(A, 2, &pressed).1.is_empty());
    let out = rig.present(A, 1, Layer::Base, 1, Some(2), regions.clone());
    assert_eq!(sent(&out, A), vec![DaemonMsg::Presented { serial: 1 }]);
    assert!(rig.d.agent_drives());
    (rig, regions)
}

// ---- safe layer + key-region semantics ---------------------------------------------------------

#[test]
fn starts_on_the_safe_layer() {
    let rig = Rig::new();
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::FKeys));
    assert_eq!(rig.hw.backlight.writes(), vec![128]);
    assert_eq!(rig.hw.display.flush_count(), 1);
    check_png("safe-fkeys", &rig.hw.display.frame());
    assert_eq!(rig.d.next_deadline(), Some(30_000), "only the dim timer");
}

#[test]
fn tap_presses_and_releases_with_feedback() {
    let mut rig = Rig::new();
    rig.down(0, slot_x(0, 12)); // F1
    assert_eq!(rig.keys(), vec![Down(KEY_F1)]);
    check_png("safe-f1-pressed", &rig.hw.display.frame());
    let flushes = rig.hw.display.flush_count();
    rig.mv(0, slot_x(0, 12) + 5.0);
    assert!(rig.keys().is_empty());
    assert_eq!(rig.hw.display.flush_count(), flushes, "no change, no flush");
    rig.up(0, slot_x(0, 12));
    assert_eq!(rig.keys(), vec![Up(KEY_F1)]);
    check_png("safe-fkeys", &rig.hw.display.frame());
    // Damage covered only the F1 button, not the whole bar.
    let s = rig.hw.display.state();
    let last = s.flushes.last().unwrap();
    assert_eq!(last.len(), 1);
    assert!(last[0].w < 200, "{last:?}");
}

#[test]
fn slide_off_releases_slide_back_presses_never_a_second_region() {
    let mut rig = Rig::new();
    let (f1, f2) = (slot_x(0, 12), slot_x(1, 12));
    rig.down(0, f1);
    rig.mv(0, f2); // onto F2: releases F1, does NOT press F2
    assert_eq!(rig.keys(), vec![Down(KEY_F1), Up(KEY_F1)]);
    check_png("safe-fkeys", &rig.hw.display.frame());
    rig.mv(0, f1); // back: presses F1 again
    assert_eq!(rig.keys(), vec![Down(KEY_F1)]);
    rig.mv(0, -50.0); // off the bar entirely
    rig.up(0, -50.0);
    assert_eq!(rig.keys(), vec![Up(KEY_F1)]);
}

#[test]
fn multitouch_chord_and_shared_keys() {
    let mut rig = Rig::new();
    rig.down(0, slot_x(0, 12));
    rig.down(1, slot_x(11, 12));
    assert_eq!(rig.keys(), vec![Down(KEY_F1), Down(KEY_F12)]);
    // A third finger on F1 doesn't re-press, and F1 stays down until the last finger lifts.
    rig.down(2, slot_x(0, 12) + 10.0);
    rig.up(0, 0.0);
    assert!(rig.keys().is_empty());
    rig.up(2, 0.0);
    rig.up(1, 0.0);
    assert_eq!(rig.keys(), vec![Up(KEY_F1), Up(KEY_F12)]);
}

#[test]
fn gaps_between_builtin_keys_have_no_dead_zone() {
    let mut rig = Rig::new();
    // Just right of F1's button, inside the 16 px gap: still F1.
    let gap = slot_x(0, 12) + (2008.0 - 16.0 * 11.0) / 12.0 / 2.0 + 4.0;
    rig.down(0, gap);
    rig.up(0, gap);
    assert_eq!(rig.keys(), vec![Down(KEY_F1), Up(KEY_F1)]);
}

#[test]
fn fn_swaps_to_media_and_held_touch_keeps_its_key() {
    let mut rig = Rig::new();
    rig.down(0, slot_x(2, 12)); // F3
    rig.input(Input::FnKey { held: true });
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::Media));
    check_png("safe-media", &rig.hw.display.frame());
    rig.up(0, slot_x(2, 12));
    assert_eq!(rig.keys(), vec![Down(KEY_F3), Up(KEY_F3)]);
    // Media layer on a bar with physical Esc: 12 icons, the first is brightness down.
    rig.down(0, slot_x(0, 12));
    rig.up(0, slot_x(0, 12));
    assert_eq!(
        rig.keys(),
        vec![
            Down(media_keys::BRIGHTNESS_DOWN),
            Up(media_keys::BRIGHTNESS_DOWN)
        ]
    );
    rig.input(Input::FnKey { held: false });
    check_png("safe-fkeys", &rig.hw.display.frame());
}

#[test]
fn media_layer_default_config() {
    let config = Config {
        media_layer_default: true,
        ..Default::default()
    };
    let mut rig = Rig::with(2008, "Mac14,7", 255, config);
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::Media));
    rig.input(Input::FnKey { held: true });
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::FKeys));
}

#[test]
fn t2_bar_without_physical_esc() {
    let mut rig = Rig::with(2170, "MacBookPro15,1", 2, Config::default());
    assert_eq!(rig.hw.backlight.writes(), vec![2]);
    rig.input(Input::FnKey { held: true });
    check_png("safe-media-t2", &rig.hw.display.frame());
    // Esc leads the media row too.
    rig.down(0, 20.0);
    rig.up(0, 20.0);
    assert_eq!(rig.keys(), vec![Down(KEY_ESC), Up(KEY_ESC)]);
    rig.advance(30_000);
    assert_eq!(rig.hw.backlight.current(), Some(1), "T2 dim step");
}

/// T96: the T2's backlight has three steps (0 off, 1 dim, 2 full). Adaptive brightness, a custom
/// curve and the timers must land on them and never turn the bar off while it's on.
#[test]
fn t2_three_step_backlight_with_adaptive_and_curve() {
    let mut rig = Rig::with(2170, "MacBookPro15,1", 2, Config::default());
    rig.connect(A, UID);
    rig.hw.backlight.set_screen(Some(1.0));
    let policy = |curve: Vec<[u16; 2]>| {
        AgentMsg::DisplayPolicy(DisplayPolicy {
            brightness: Some(2),
            follow_screen: Some(true),
            curve: Some(curve),
            ..Default::default()
        })
    };
    for curve in [
        vec![],
        vec![[0, 0], [1000, 1000]],
        vec![[0, 500], [500, 1000]],
    ] {
        rig.send(A, policy(curve.clone()));
        for screen in [0.0, 0.01, 0.3, 0.7, 1.0] {
            rig.hw.backlight.set_screen(Some(screen));
            rig.input(Input::ScreenBacklight);
            let v = rig.hw.backlight.current().unwrap();
            assert!((1..=2).contains(&v), "curve {curve:?} screen {screen}: {v}");
        }
        assert_eq!(
            rig.hw.backlight.current(),
            Some(2),
            "full screen = full bar ({curve:?})"
        );
    }
    rig.advance(30_000);
    assert_eq!(rig.hw.backlight.current(), Some(1), "dim step");
    rig.advance(30_000);
    assert_eq!(rig.hw.backlight.current(), Some(0), "off");
}

// ---- display power ------------------------------------------------------------------------------

#[test]
fn dims_turns_off_and_wake_touch_is_swallowed() {
    let mut rig = Rig::new();
    rig.advance(30_000);
    assert_eq!(rig.d.display_state(), DisplayState::Dimmed);
    assert_eq!(rig.hw.backlight.current(), Some(1));
    // Touch while dimmed works and brightens.
    rig.down(0, slot_x(0, 12));
    rig.up(0, slot_x(0, 12));
    assert_eq!(rig.keys(), vec![Down(KEY_F1), Up(KEY_F1)]);
    assert_eq!(rig.hw.backlight.current(), Some(128));
    rig.advance(60_000);
    assert_eq!(rig.d.display_state(), DisplayState::Off);
    assert_eq!(rig.hw.backlight.current(), Some(0));
    assert_eq!(rig.d.next_deadline(), None, "no timers while off");
    // Fn while off: the layer change is drawn on wake, not now.
    let flushes = rig.hw.display.flush_count();
    rig.input(Input::Lid { closed: true });
    assert_eq!(rig.hw.display.flush_count(), flushes);
    rig.input(Input::Lid { closed: false });
    assert_eq!(rig.d.display_state(), DisplayState::On);
    rig.advance(60_000);
    // Wake touch: no key, display on.
    rig.down(0, slot_x(0, 12));
    rig.mv(0, slot_x(0, 12) + 1.0);
    rig.up(0, slot_x(0, 12));
    assert!(rig.keys().is_empty());
    assert_eq!(rig.d.display_state(), DisplayState::On);
}

#[test]
fn backlight_written_only_on_change() {
    let mut rig = Rig::new();
    for _ in 0..5 {
        rig.input(Input::Activity);
        rig.advance(1_000);
    }
    assert_eq!(rig.hw.backlight.writes(), vec![128]);
}

// ---- agent --------------------------------------------------------------------------------------

#[test]
fn agent_frame_and_combo_region() {
    let (mut rig, regions) = rig_with_agent();
    check_png("agent-base", &rig.hw.display.frame());
    let copy = regions[0].rect;
    let x = copy.x as f32 + 20.0;
    rig.down(0, x);
    assert_eq!(rig.keys(), vec![Down(KEY_LEFTCTRL), Down(KEY_C)]);
    // Pressed feedback comes from the agent's pressed image, only for that region.
    check_png("agent-copy-pressed", &rig.hw.display.frame());
    rig.up(0, x);
    // Combos release in reverse: modifiers last.
    assert_eq!(rig.keys(), vec![Up(KEY_C), Up(KEY_LEFTCTRL)]);
    check_png("agent-base", &rig.hw.display.frame());
}

#[test]
fn combo_is_atomic_per_sync() {
    let (mut rig, regions) = rig_with_agent();
    rig.hw.keys.take();
    rig.down(0, regions[0].rect.x as f32 + 1.0);
    assert_eq!(
        rig.hw.keys.take_raw(),
        vec![Down(KEY_LEFTCTRL), Down(KEY_C), KeyEvent::Sync]
    );
}

#[test]
fn agent_region_touches_are_forwarded_and_captured() {
    let (mut rig, regions) = rig_with_agent();
    let widget = regions[1].rect;
    let x = widget.x as f32 + 10.0;
    let out = rig.down(3, x);
    let msgs = sent(&out, A);
    assert!(matches!(
        msgs[0],
        DaemonMsg::Touch {
            region: 11,
            slot: 3,
            phase: TouchPhase::Down,
            ..
        }
    ));
    // First agent-region touch pings (liveness only when it matters).
    assert!(matches!(msgs[1], DaemonMsg::Ping { .. }));
    // Captured: moving onto a key region still goes to the agent and emits nothing.
    let out = rig.mv(3, regions[2].rect.x as f32 + 5.0);
    assert!(matches!(
        sent(&out, A)[..],
        [DaemonMsg::Touch {
            region: 11,
            phase: TouchPhase::Move,
            ..
        }]
    ));
    let out = rig.up(3, 0.0);
    assert!(matches!(
        sent(&out, A)[..],
        [DaemonMsg::Touch {
            region: 11,
            phase: TouchPhase::Up,
            ..
        }]
    ));
    assert!(rig.keys().is_empty());
}

#[test]
fn hung_agent_falls_back_to_safe_layer() {
    let (mut rig, regions) = rig_with_agent();
    let x = regions[1].rect.x as f32 + 10.0;
    let out = rig.down(0, x);
    let nonce = sent(&out, A)
        .into_iter()
        .find_map(|m| match m {
            DaemonMsg::Ping { nonce } => Some(nonce),
            _ => None,
        })
        .expect("ping");
    rig.up(0, x);
    assert_eq!(rig.d.next_deadline(), Some(rig.now + 2_000));
    rig.advance(1_000);
    rig.send(A, AgentMsg::Pong { nonce });
    assert_eq!(
        rig.d.next_deadline(),
        Some(30_000),
        "pong cleared the timer"
    );
    // Within 5 s of the last ping: no new ping.
    let out = rig.down(0, x);
    assert!(
        !sent(&out, A)
            .iter()
            .any(|m| matches!(m, DaemonMsg::Ping { .. }))
    );
    rig.up(0, x);
    rig.advance(5_000);
    let out = rig.down(0, x);
    assert!(
        sent(&out, A)
            .iter()
            .any(|m| matches!(m, DaemonMsg::Ping { .. }))
    );
    rig.up(0, x);
    // ...and no pong this time.
    let out = rig.advance(2_000);
    assert!(closed(&out, A));
    assert!(!rig.d.agent_drives());
    check_png("safe-fkeys", &rig.hw.display.frame());
}

#[test]
fn agent_death_releases_held_keys_and_shows_safe_layer() {
    let (mut rig, regions) = rig_with_agent();
    let vol = regions[2].rect.x as f32 + 10.0;
    rig.down(0, vol);
    rig.send(
        A,
        AgentMsg::EmitKeys {
            keys: vec![KEY_LEFTCTRL],
            mode: KeyMode::Down,
        },
    );
    assert_eq!(
        rig.keys(),
        vec![Down(media_keys::VOLUME_UP), Down(KEY_LEFTCTRL)]
    );
    rig.input(Input::Disconnected { client: A });
    assert_eq!(
        rig.keys(),
        vec![Up(KEY_LEFTCTRL), Up(media_keys::VOLUME_UP)]
    );
    check_png("safe-fkeys", &rig.hw.display.frame());
    // The finger that was on the dead agent's region does nothing more, even on a safe key.
    rig.mv(0, slot_x(0, 12));
    rig.up(0, slot_x(0, 12));
    assert!(rig.keys().is_empty());
    // A tap landing right after the agent vanished was aimed at an agent button: swallowed.
    rig.now += 22;
    rig.down(1, slot_x(9, 12));
    rig.up(1, slot_x(9, 12));
    assert!(rig.keys().is_empty(), "no surprise F10");
    // Once the safe layer has been visible for a moment, it works normally.
    rig.now += 200;
    rig.down(1, slot_x(9, 12));
    rig.up(1, slot_x(9, 12));
    assert_eq!(rig.keys(), vec![Down(KEY_F10), Up(KEY_F10)]);
}

#[test]
fn bye_and_garbage_fall_back() {
    let (mut rig, _) = rig_with_agent();
    let out = rig.send(A, AgentMsg::Bye);
    assert!(closed(&out, A));
    assert!(!rig.d.agent_drives());

    let (mut rig, _) = rig_with_agent();
    let out = rig.input(Input::Packet {
        client: A,
        msg: Err(CodecError::UnknownMessage("teleport".into())),
        fd: Attachment::None,
    });
    assert_eq!(error_codes(&out, A), vec![ErrorCode::UnknownMessage]);
    assert!(rig.d.agent_drives(), "unknown messages are not fatal");
    let out = rig.input(Input::Packet {
        client: A,
        msg: Err(CodecError::Malformed("eh".into())),
        fd: Attachment::None,
    });
    assert_eq!(error_codes(&out, A), vec![ErrorCode::Malformed]);
    assert!(closed(&out, A));
    assert!(!rig.d.agent_drives());
}

#[test]
fn fn_layer_is_fully_editable() {
    let (mut rig, regions) = rig_with_agent();
    // No agent fn layer: Fn shows the built-in F-keys.
    rig.input(Input::FnKey { held: true });
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::FKeys));
    rig.down(0, slot_x(4, 12));
    rig.up(0, slot_x(4, 12));
    assert_eq!(rig.keys(), vec![Down(KEY_F5), Up(KEY_F5)]);
    // An fn layer without any F-keys is accepted as is (H4 revised).
    let out = rig.present(A, 2, Layer::Fn, 1, None, regions.clone());
    assert_eq!(sent(&out, A), vec![DaemonMsg::Presented { serial: 2 }]);
    assert_eq!(rig.d.shown_view(), View::Agent(Layer::Fn));
    // clear_layer falls back to the built-in one.
    rig.send(A, AgentMsg::ClearLayer { layer: Layer::Fn });
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::FKeys));
}

/// Agent `A` with an fn layer that has no F-keys, so only the hatch can reach them.
fn rig_without_fkeys() -> (Rig, Vec<Region>) {
    let (mut rig, regions) = rig_with_agent();
    rig.present(A, 2, Layer::Fn, 1, None, regions.clone());
    (rig, regions)
}

#[test]
fn safe_hatch_double_press_fn_and_hold() {
    let (mut rig, regions) = rig_without_fkeys();
    // A finger on the agent's widget area.
    let widget = regions[1].rect.x as f32 + 10.0;
    rig.down(5, widget);
    // Single Fn press: the agent's fn layer.
    rig.input(Input::FnKey { held: true });
    assert_eq!(rig.d.shown_view(), View::Agent(Layer::Fn));
    rig.now += 100;
    rig.input(Input::FnKey { held: false });
    rig.now += 150;
    // Second press within 400 ms: the hatch opens. Captured touches are cancelled first, then
    // the agent hears hatch{active}, then fn{held}.
    let out = rig.input(Input::FnKey { held: true });
    assert!(rig.d.hatch_active());
    let msgs = sent(&out, A);
    assert!(matches!(
        msgs[0],
        DaemonMsg::Touch {
            region: 11,
            phase: TouchPhase::Cancel,
            ..
        }
    ));
    assert_eq!(
        msgs[1..],
        [
            DaemonMsg::Hatch { active: true },
            DaemonMsg::FnKey { held: true }
        ]
    );
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::FKeys));
    check_png("safe-fkeys", &rig.hw.display.frame());
    rig.up(5, widget); // the cancelled finger lifts: nothing
    // F-keys work and never reach the agent.
    let out = rig.down(0, slot_x(0, 12));
    assert!(sent(&out, A).is_empty());
    rig.up(0, slot_x(0, 12));
    assert_eq!(rig.keys(), vec![Down(KEY_F1), Up(KEY_F1)]);
    // Agent frames keep arriving but don't show while the hatch is open.
    let out = rig.present(A, 3, Layer::Base, 1, None, regions.clone());
    assert_eq!(sent(&out, A), vec![DaemonMsg::Presented { serial: 3 }]);
    assert_eq!(rig.d.shown_view(), View::Builtin(BuiltinKind::FKeys));
    // Releasing Fn closes it: a key held under the hatch is released.
    rig.down(0, slot_x(0, 12));
    rig.now += 1_000; // (the widget touch's ping is still within its 2 s)
    let out = rig.input(Input::FnKey { held: false });
    assert_eq!(rig.keys(), vec![Down(KEY_F1), Up(KEY_F1)]);
    assert_eq!(
        sent(&out, A),
        vec![
            DaemonMsg::Hatch { active: false },
            DaemonMsg::FnKey { held: false }
        ]
    );
    assert!(rig.d.agent_drives());
    assert_eq!(rig.d.shown_view(), View::Agent(Layer::Base));
}

#[test]
fn safe_hatch_needs_a_quick_double_press() {
    let (mut rig, _) = rig_without_fkeys();
    rig.input(Input::FnKey { held: true });
    rig.now += 100;
    rig.input(Input::FnKey { held: false });
    rig.now += 400; // 500 ms between the downs
    rig.input(Input::FnKey { held: true });
    assert!(!rig.d.hatch_active());
    assert_eq!(rig.d.shown_view(), View::Agent(Layer::Fn));
}

#[test]
fn invalid_frames_keep_the_previous_frame() {
    let (mut rig, regions) = rig_with_agent();
    let before = rig.hw.display.flush_count();
    let mut bad = regions.clone();
    bad[2].kind = RegionKind::Key {
        keys: vec![KEY_SYSRQ],
    };
    let out = rig.present(A, 5, Layer::Base, 1, None, bad);
    assert_eq!(error_codes(&out, A), vec![ErrorCode::ForbiddenKey]);
    let mut overlap = regions.clone();
    overlap[1].rect.x -= 20;
    let out = rig.present(A, 6, Layer::Base, 1, None, overlap);
    assert_eq!(error_codes(&out, A), vec![ErrorCode::InvalidFrame]);
    let out = rig.present(A, 7, Layer::Base, 99, None, regions.clone());
    assert_eq!(error_codes(&out, A), vec![ErrorCode::InvalidFrame]);
    assert!(rig.d.agent_drives());
    assert_eq!(rig.hw.display.flush_count(), before);
    // emit_keys refuses SysRq too.
    let out = rig.send(
        A,
        AgentMsg::EmitKeys {
            keys: vec![56, KEY_SYSRQ],
            mode: KeyMode::Tap,
        },
    );
    assert_eq!(error_codes(&out, A), vec![ErrorCode::ForbiddenKey]);
    assert!(rig.keys().is_empty());
}

#[test]
fn buffers_are_validated_and_copied() {
    let mut rig = Rig::new();
    rig.connect(A, UID);
    let (normal, _, regions) = agent_frame();
    // No fd, wrong size, too short.
    let out = rig.send(
        A,
        AgentMsg::AttachBuffer {
            id: 1,
            width: 2008,
            height: 60,
            stride: 8032,
            format: PixelFormat::Xrgb8888,
        },
    );
    assert_eq!(error_codes(&out, A), vec![ErrorCode::InvalidBuffer]);
    let small = Pixmap::new(100, 60).unwrap();
    assert_eq!(
        error_codes(&rig.attach(A, 1, &small).1, A),
        vec![ErrorCode::InvalidBuffer]
    );
    let attach = AgentMsg::AttachBuffer {
        id: 1,
        width: 2008,
        height: 60,
        stride: 8032,
        format: PixelFormat::Xrgb8888,
    };
    let out = rig.input(Input::Packet {
        client: A,
        msg: Ok(attach.clone()),
        fd: Attachment::Rejected("not sealed".into()),
    });
    assert_eq!(error_codes(&out, A), vec![ErrorCode::InvalidBuffer]);
    // A buffer whose contents don't match its header is refused at present.
    let out = rig.input(Input::Packet {
        client: A,
        msg: Ok(attach),
        fd: Attachment::Buffer(Box::new(FakeShm::new(100))),
    });
    assert!(out.is_empty());
    let out = rig.present(A, 1, Layer::Base, 1, None, regions.clone());
    assert_eq!(error_codes(&out, A), vec![ErrorCode::InvalidFrame]);
    rig.send(A, AgentMsg::DetachBuffer { id: 1 });
    // Copy on present: later writes to the buffer don't show until the next present.
    let (shm, _) = rig.attach(A, 1, &normal);
    rig.present(A, 1, Layer::Base, 1, None, regions.clone());
    let shown = rig.hw.display.frame();
    shm.write(&Pixmap::new(2008, 60).unwrap());
    rig.input(Input::Activity);
    assert_eq!(rig.hw.display.frame().data(), shown.data());
    // Generic highlight without a pressed image.
    rig.down(0, regions[0].rect.x as f32 + 1.0);
    let f = rig.hw.display.frame();
    let px = |p: &Pixmap, x: u32| p.pixel(x, 30).unwrap().red();
    assert!(px(&f, 5) > px(&shown, 5), "region lightened");
    assert_eq!(px(&f, 1000), px(&shown, 1000), "rest untouched");
}

#[test]
fn proto_mismatch_and_hello_first() {
    let mut rig = Rig::new();
    rig.input(Input::Connected {
        client: A,
        uid: UID,
    });
    let out = rig.send(
        A,
        AgentMsg::Hello {
            proto: ProtoVersion(PROTO.0 + 1, 0),
            agent: "future".into(),
        },
    );
    assert_eq!(error_codes(&out, A), vec![ErrorCode::Proto]);
    assert!(closed(&out, A));

    let b = ClientId(2);
    rig.input(Input::Connected {
        client: b,
        uid: UID,
    });
    let out = rig.send(b, AgentMsg::Wake);
    assert!(closed(&out, b));
    assert_eq!(rig.d.active_client(), None);
}

#[test]
fn sessions_standby_takeover_and_revoke() {
    let (mut rig, _) = rig_with_agent();
    // Another user's agent waits in standby; its frames are dropped.
    let b = ClientId(2);
    let out = rig.connect(b, 2000);
    assert!(matches!(
        sent(&out, b)[..],
        [DaemonMsg::Welcome {
            role: Role::Standby,
            ..
        }]
    ));
    let out = rig.present(b, 1, Layer::Base, 1, None, vec![]);
    assert_eq!(error_codes(&out, b), vec![ErrorCode::NotActive]);
    // Session switch to uid 2000: A goes standby, B becomes active, safe layer until B presents.
    let out = rig.input(Input::SessionChanged {
        active_uid: Some(2000),
    });
    assert_eq!(
        sent(&out, A),
        vec![DaemonMsg::Role {
            role: Role::Standby
        }]
    );
    assert_eq!(sent(&out, b), vec![DaemonMsg::Role { role: Role::Active }]);
    assert_eq!(rig.d.active_client(), Some(b));
    assert!(!rig.d.agent_drives());
    // A second agent of the active user takes over; the old one is revoked and closed.
    let c = ClientId(3);
    let out = rig.connect(c, 2000);
    assert_eq!(
        sent(&out, b),
        vec![DaemonMsg::Role {
            role: Role::Revoked
        }]
    );
    assert!(closed(&out, b));
    assert_eq!(rig.d.active_client(), Some(c));
}

#[test]
fn display_policy_and_wake() {
    let (mut rig, _) = rig_with_agent();
    rig.send(
        A,
        AgentMsg::DisplayPolicy(DisplayPolicy {
            brightness: Some(200),
            off_after_ms: Some(10_000),
            dim_after_ms: Some(0),
            ..Default::default()
        }),
    );
    assert_eq!(rig.hw.backlight.current(), Some(200));
    assert_eq!(rig.d.next_deadline(), Some(10_000));
    let out = rig.advance(10_000);
    assert_eq!(
        sent(&out, A),
        vec![DaemonMsg::Display {
            state: DisplayState::Off
        }]
    );
    // Wake from the agent turns it on (and pings, since the display woke).
    let out = rig.send(A, AgentMsg::Wake);
    let msgs = sent(&out, A);
    assert_eq!(
        msgs[0],
        DaemonMsg::Display {
            state: DisplayState::On
        }
    );
    assert!(matches!(msgs[1], DaemonMsg::Ping { .. }));
    assert_eq!(rig.hw.backlight.current(), Some(200));
    let out = rig.send(A, AgentMsg::Wake);
    assert_eq!(error_codes(&out, A), vec![ErrorCode::RateLimited]);
}

/// The agent re-sends its policy when the power source changes (T68, `[display.on-battery]`): the
/// new levels apply at once in whatever state the bar is in, and shorter timers take effect
/// against the time already idle.
#[test]
fn policy_change_applies_in_current_state() {
    let (mut rig, _) = rig_with_agent();
    let ac = DisplayPolicy {
        brightness: Some(200),
        dim_brightness: Some(40),
        dim_after_ms: Some(30_000),
        off_after_ms: Some(60_000),
        ..Default::default()
    };
    rig.send(A, AgentMsg::DisplayPolicy(ac.clone()));
    assert_eq!(rig.hw.backlight.current(), Some(200));
    rig.advance(30_000);
    assert_eq!(rig.hw.backlight.current(), Some(40), "dimmed");
    // Unplugged while dimmed: the battery dim level applies right away, no touch needed.
    let battery = DisplayPolicy {
        brightness: Some(120),
        dim_brightness: Some(10),
        dim_after_ms: Some(10_000),
        off_after_ms: Some(40_000),
        ..Default::default()
    };
    let out = rig.send(A, AgentMsg::DisplayPolicy(battery));
    assert!(sent(&out, A).is_empty(), "still dim: no display message");
    assert_eq!(rig.hw.backlight.current(), Some(10));
    // Already idle 30 s of the new 40 s: off 10 s later, not 40.
    assert_eq!(rig.d.next_deadline(), Some(rig.now + 10_000));
    // A touch wakes it to the battery's active level.
    rig.down(0, 100.0);
    rig.up(0, 100.0);
    assert_eq!(rig.hw.backlight.current(), Some(120));
    // Plugged in again while on: the AC level at once.
    rig.send(A, AgentMsg::DisplayPolicy(ac));
    assert_eq!(rig.hw.backlight.current(), Some(200));
}

/// The main screen's brightness changing (a udev event, no polling) re-applies an adaptive bar's
/// brightness at once, and does nothing for a fixed one.
#[test]
fn screen_backlight_change_follows_without_a_touch() {
    let (mut rig, _) = rig_with_agent();
    rig.hw.backlight.set_screen(Some(1.0));
    rig.send(
        A,
        AgentMsg::DisplayPolicy(DisplayPolicy {
            brightness: Some(128),
            follow_screen: Some(true),
            ..Default::default()
        }),
    );
    assert_eq!(rig.hw.backlight.current(), Some(129), "sqrt(1) * 128 + 1");
    rig.hw.backlight.set_screen(Some(0.25));
    rig.input(Input::ScreenBacklight);
    assert_eq!(rig.hw.backlight.current(), Some(65), "sqrt(0.25) * 128 + 1");
    let n = rig.hw.backlight.writes().len();
    rig.input(Input::ScreenBacklight);
    assert_eq!(rig.hw.backlight.writes().len(), n, "unchanged: no write");

    rig.send(
        A,
        AgentMsg::DisplayPolicy(DisplayPolicy {
            follow_screen: Some(false),
            ..Default::default()
        }),
    );
    assert_eq!(rig.hw.backlight.current(), Some(128));
    rig.hw.backlight.set_screen(Some(1.0));
    rig.input(Input::ScreenBacklight);
    assert_eq!(
        rig.hw.backlight.current(),
        Some(128),
        "fixed: ignores the screen"
    );
}

#[test]
fn emit_keys_tap_and_up_only_releases_own_keys() {
    let (mut rig, regions) = rig_with_agent();
    rig.send(
        A,
        AgentMsg::EmitKeys {
            keys: vec![KEY_LEFTCTRL, KEY_C],
            mode: KeyMode::Tap,
        },
    );
    assert_eq!(
        rig.keys(),
        vec![Down(KEY_LEFTCTRL), Down(KEY_C), Up(KEY_C), Up(KEY_LEFTCTRL)]
    );
    // A finger holds Ctrl+C; the agent can't release it.
    rig.down(0, regions[0].rect.x as f32 + 1.0);
    rig.keys();
    rig.send(
        A,
        AgentMsg::EmitKeys {
            keys: vec![KEY_LEFTCTRL],
            mode: KeyMode::Up,
        },
    );
    assert!(rig.keys().is_empty());
}

fn shift_on() -> AgentMsg {
    AgentMsg::DisplayPolicy(DisplayPolicy {
        pixel_shift: Some(true),
        ..Default::default()
    })
}

/// `p` moved by `(dx, dy)` with black where nothing lands: what the panel should show.
fn moved(p: &Pixmap, dx: i32, dy: i32) -> Vec<[u8; 4]> {
    let (w, h) = (p.width() as i32, p.height() as i32);
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (x - dx, y - dy);
            out.push(if (0..w).contains(&sx) && (0..h).contains(&sy) {
                let px = p.pixel(sx as u32, sy as u32).unwrap();
                [px.red(), px.green(), px.blue(), 255]
            } else {
                [0, 0, 0, 255]
            });
        }
    }
    out
}

fn pixels(p: &Pixmap) -> Vec<[u8; 4]> {
    p.data()
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], 255])
        .collect()
}

/// T79: the panel shows the frame moved by the current shift, steps 1 px at a time on a slow
/// timer, and touches are hit-tested where the finger is on the (moved) picture.
#[test]
fn pixel_shift_moves_the_picture_and_the_touches() {
    let (mut rig, regions) = rig_with_agent();
    let still = rig.hw.display.frame();
    assert_eq!(rig.d.pixel_shift(), (0, 0));
    // Never idle: stepping takes up to a minute, and a dark bar would swallow the test tap.
    rig.send(
        A,
        AgentMsg::DisplayPolicy(DisplayPolicy {
            dim_after_ms: Some(0),
            off_after_ms: Some(0),
            ..Default::default()
        }),
    );
    rig.send(A, shift_on());
    let mut last = rig.d.pixel_shift();
    assert!(
        pixels(&rig.hw.display.frame()) == moved(&still, last.0, last.1),
        "panel = frame moved by {last:?}"
    );
    // Steps every 15 s (or 60 s at an end), 1 px in x, until it's at least 2 px off centre.
    while last.0.abs() < 2 {
        let at = rig.d.next_deadline().expect("shift step scheduled");
        rig.advance(at - rig.now);
        let now = rig.d.pixel_shift();
        assert_eq!((now.0 - last.0).abs(), 1, "{last:?} -> {now:?}");
        assert!(pixels(&rig.hw.display.frame()) == moved(&still, now.0, now.1));
        last = now;
    }
    // 2 px inside the Copy region's end *on the frame*, at the end the picture moved towards:
    // without the correction, a 2+ px shift would put the finger outside the region.
    let copy = regions[0].rect;
    let frame_x = if last.0 > 0 {
        (copy.x + copy.w) as f32 - 2.0
    } else {
        copy.x as f32 + 1.0
    };
    let on_glass = frame_x + last.0 as f32;
    rig.down(0, on_glass);
    assert_eq!(
        rig.keys(),
        vec![Down(KEY_LEFTCTRL), Down(KEY_C)],
        "shift {last:?}"
    );
    rig.up(0, on_glass);

    // Off again: centred at once and no more steps.
    rig.send(
        A,
        AgentMsg::DisplayPolicy(DisplayPolicy {
            pixel_shift: Some(false),
            ..Default::default()
        }),
    );
    assert_eq!(rig.d.pixel_shift(), (0, 0));
    assert!(pixels(&rig.hw.display.frame()) == pixels(&still));
}

/// T79: no steps (no wakeups) while the bar is dark; the built-in layers keep clear of the ends.
#[test]
fn pixel_shift_sleeps_while_dark_and_insets_the_safe_layer() {
    let (mut rig, _) = rig_with_agent();
    rig.send(A, shift_on());
    rig.input(Input::Disconnected { client: A }); // the policy stays; safe layer shown
    let (dx, _) = rig.d.pixel_shift();
    let f = rig.hw.display.frame();
    let w = f.width() as i32;
    for x in (0..w).filter(|&x| x < 11 + dx || x >= w - 11 + dx) {
        for y in 0..f.height() {
            let px = f.pixel(x as u32, y).unwrap();
            assert_eq!(
                (px.red(), px.green(), px.blue()),
                (0, 0, 0),
                "ink at ({x}, {y}), dx {dx}"
            );
        }
    }
    rig.advance(60_000); // idle: off
    assert_eq!(rig.d.display_state(), DisplayState::Off);
    assert_eq!(rig.d.next_deadline(), None, "a dark bar sleeps");
}
