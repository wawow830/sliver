//! cedar's `sliver_ipc::fake::FakeDaemon` test suite, run unchanged (modulo the mechanical
//! substitutions noted inline) against `sliverd::testing::TestDaemon`: the drop-in replacement
//! built on the real daemon core behaves the same.

use sliver_ipc::AgentMsg;
use sliver_ipc::keys::*;
use sliver_ipc::proto::*;
use sliver_ipc::shm::ShmBuffer;
use sliver_ipc::transport::Conn;
use sliverd::testing::TestDaemon;
use std::os::fd::AsFd;

fn recv(c: &Conn) -> DaemonMsg {
    c.recv::<DaemonMsg>().unwrap().unwrap().msg.unwrap()
}

/// Hello + one attached buffer filled with `fill`.
fn connect() -> (TestDaemon, Conn, ShmBuffer) {
    let (mut d, a) = TestDaemon::new(TestDaemon::mac14_7()).unwrap();
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
            assert_eq!(proto, sliver_ipc::PROTO, "the lower minor: ours");
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
    // (Substitution: B, G, R only. sliverd copies into a pixmap, so the X padding byte isn't kept.)
    let bgr = |px: &[u8]| px.chunks(4).all(|p| p[..3] == [0x11; 3]);
    assert!(bgr(&f.pixels));
    // The agent may reuse the buffer after `presented` without affecting the shown frame.
    buf.pixels_mut().fill(0);
    assert!(bgr(&d.base.as_ref().unwrap().pixels));
    assert!(!d.safe_layer_active());
}

#[test]
fn layouts_without_fkeys_are_fine_and_the_hatch_brings_them_back() {
    let (mut d, a, _buf) = connect();
    // Fully editable (H4): neither layer has F-keys. sliverd accepts that.
    let base = vec![Region {
        id: 1,
        rect: Rect::new(0, 0, 2008, 60),
        kind: RegionKind::Agent,
    }];
    a.send(&present(Layer::Base, 1, base)).unwrap();
    a.send(&present(Layer::Fn, 2, vec![key(0, 0, 100, 113)]))
        .unwrap();
    d.pump().unwrap();
    assert_eq!(recv(&a), DaemonMsg::Presented { serial: 1 });
    assert_eq!(recv(&a), DaemonMsg::Presented { serial: 2 });
    assert!(d.errors.is_empty());
    // A finger resting on the agent area when the hatch opens gets cancelled.
    d.touch(3, TouchPhase::Down, 500.0, 30.0).unwrap();
    // Fn, Fn-and-hold: the built-in layer, with F5 where the safe layer puts it.
    d.fn_key(true, 10_000).unwrap();
    d.fn_key(false, 10_100).unwrap();
    d.fn_key(true, 10_250).unwrap();
    assert!(d.hatch_active());
    let f5 = d
        .visible_regions()
        .into_iter()
        .find(|r| r.kind == RegionKind::Key { keys: vec![KEY_F5] })
        .unwrap();
    d.tap(f5.rect.x as f32 + 1.0, 30.0).unwrap();
    assert_eq!(d.keys, vec![(KEY_F5, true), (KEY_F5, false)]);
    d.fn_key(false, 12_000).unwrap();
    assert!(!d.hatch_active());
    let msgs: Vec<DaemonMsg> = (0..8).map(|_| recv(&a)).collect();
    assert!(matches!(
        msgs[0],
        DaemonMsg::Touch {
            phase: TouchPhase::Down,
            ..
        }
    ));
    assert_eq!(msgs[1], DaemonMsg::FnKey { held: true });
    assert_eq!(msgs[2], DaemonMsg::FnKey { held: false });
    assert!(matches!(
        msgs[3],
        DaemonMsg::Touch {
            phase: TouchPhase::Cancel,
            region: 1,
            slot: 3,
            ..
        }
    ));
    assert_eq!(msgs[4], DaemonMsg::Hatch { active: true });
    assert_eq!(msgs[5], DaemonMsg::FnKey { held: true });
    assert_eq!(msgs[6], DaemonMsg::Hatch { active: false });
    assert_eq!(msgs[7], DaemonMsg::FnKey { held: false });
    // A plain Fn hold shows the agent's fn layer.
    d.set_fn(true).unwrap();
    assert_eq!(d.visible_regions(), vec![key(0, 0, 100, 113)]);
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
    let (mut d, a) = TestDaemon::new(TestDaemon::mac14_7()).unwrap();
    a.send(&AgentMsg::Hello {
        proto: ProtoVersion(2, 0),
        agent: "future".into(),
    })
    .unwrap();
    d.pump().unwrap();
    assert_eq!(
        d.errors.iter().map(|e| e.0).collect::<Vec<_>>(),
        vec![ErrorCode::Proto]
    );
    assert!(d.closed && d.hello.is_none());

    let (mut d, a) = TestDaemon::new(TestDaemon::mac14_7()).unwrap();
    a.send(&AgentMsg::Wake).unwrap();
    d.pump().unwrap();
    assert_eq!(d.errors[0].0, ErrorCode::Malformed);
    assert!(d.closed);
}

#[test]
fn unknown_types_continue_garbage_closes() {
    let (mut d, a, _buf) = connect();
    rustix::net::send(
        &a,
        br#"{"t":"from_the_future"}"#,
        rustix::net::SendFlags::empty(),
    )
    .unwrap();
    d.pump().unwrap();
    assert!(!d.closed);
    rustix::net::send(&a, b"garbage", rustix::net::SendFlags::empty()).unwrap();
    d.pump().unwrap();
    let codes: Vec<ErrorCode> = d.errors.iter().map(|e| e.0).collect();
    assert_eq!(codes, vec![ErrorCode::UnknownMessage, ErrorCode::Malformed]);
    assert!(d.closed && d.safe_layer_active());
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

// ---- beyond FakeDaemon: the real daemon's extras -------------------------------------------

#[test]
fn liveness_when_auto_pong_is_off() {
    let (mut d, a, _buf) = connect();
    d.auto_pong = false;
    let mut regions = fkeys();
    regions.push(Region {
        id: 99,
        rect: Rect::new(1920, 0, 88, 60),
        kind: RegionKind::Agent,
    });
    a.send(&present(Layer::Base, 1, regions)).unwrap();
    d.pump().unwrap();
    recv(&a);
    d.touch(1, TouchPhase::Down, 1950.0, 30.0).unwrap();
    assert!(matches!(recv(&a), DaemonMsg::Touch { .. }));
    assert!(matches!(recv(&a), DaemonMsg::Ping { .. }));
    d.advance(2_000); // no pong
    assert!(d.closed && d.safe_layer_active());
}

#[test]
fn frame_shows_what_the_bar_shows() {
    let (mut d, a, _buf) = connect();
    a.send(&present(Layer::Base, 1, fkeys())).unwrap();
    d.pump().unwrap();
    let f = d.frame();
    assert_eq!((f.width(), f.height()), (2008, 60));
    let px = f.pixel(500, 30).unwrap();
    assert_eq!((px.red(), px.green(), px.blue()), (0x11, 0x11, 0x11));
    // Pressing F1 lightens it (no pressed image: generic highlight).
    d.touch(0, TouchPhase::Down, 10.0, 30.0).unwrap();
    assert!(d.frame().pixel(10, 30).unwrap().red() > 0x11);
}
