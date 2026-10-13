//! The event loop end to end: a real `SOCK_SEQPACKET` socket, a real agent [`Conn`], sealed
//! memfd frames, and fake hardware (input arrives through a pollable fd).

use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sliver_ipc::AgentMsg;
use sliver_ipc::proto::{
    DaemonMsg, DisplayPolicy, DisplayState, Layer, PROTO, PanelInfo, PixelFormat, Present, Rect,
    Region, RegionKind, Role, TouchPhase,
};
use sliver_ipc::shm::ShmBuffer;
use sliver_ipc::transport::{Conn, Listener};
use sliver_render::panel::{Orientation, blit_xrgb8888};
use sliver_render::{Fonts, Item, Renderer, Scene, Stylesheet};
use sliverd::backend::{HwEvent, TouchEvent};
use sliverd::fake::{FakeHardware, FakeInput, FakeInputHandle, KeyEvent};
use sliverd::server::Server;
use sliverd::session::FixedSession;
use sliverd::{Config, Daemon};

struct Rig {
    server: Server,
    hw: FakeHardware,
    input: FakeInputHandle,
    socket: PathBuf,
}

/// Under target/ when the path fits in `sun_path` (108 bytes), else the system temp dir (deep
/// build roots like rpmbuild's exceed it).
fn socket_path(name: &str) -> PathBuf {
    let file = format!("sliverd-{name}-{}.sock", std::process::id());
    let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(&file);
    if p.as_os_str().len() < 100 {
        p
    } else {
        std::env::temp_dir().join(file)
    }
}

impl Rig {
    fn new(name: &str) -> Rig {
        let socket = socket_path(name);
        let listener = Listener::bind(&socket).unwrap();
        let hw = FakeHardware::new(2008, 60);
        let panel = PanelInfo {
            width: 2008,
            height: 60,
            format: PixelFormat::Xrgb8888,
            max_brightness: 255,
            model: "Mac14,7".into(),
        };
        let clock = Instant::now();
        let daemon = Daemon::new(hw.backends(), panel, &Config::default(), None, 0);
        let (input, handle) = FakeInput::new().unwrap();
        let uid = rustix::process::getuid().as_raw();
        let session = FixedSession::new(Some(uid)).unwrap();
        let server = Server::new(
            daemon,
            listener,
            vec![Box::new(input)],
            Box::new(session),
            clock,
        )
        .unwrap();
        Rig {
            server,
            hw,
            input: handle,
            socket,
        }
    }

    fn connect(&mut self) -> Conn {
        let c = Conn::connect(&self.socket).unwrap();
        c.set_nonblocking(true).unwrap();
        self.pump();
        c
    }

    /// Lets the server handle whatever is pending (without blocking on an idle socket).
    fn pump(&mut self) {
        for _ in 0..3 {
            self.server
                .run_once(Some(Duration::from_millis(5)))
                .unwrap();
        }
    }

    /// Pumps until `agent` has a message, then returns it.
    fn recv(&mut self, agent: &Conn) -> DaemonMsg {
        for _ in 0..100 {
            match agent.recv::<DaemonMsg>() {
                Ok(Some(p)) => return p.msg.unwrap(),
                Ok(None) => panic!("sliverd hung up"),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => self
                    .server
                    .run_once(Some(Duration::from_millis(5)))
                    .unwrap(),
                Err(e) => panic!("{e}"),
            }
        }
        panic!("no message from sliverd");
    }

    fn touch(&mut self, slot: u8, phase: TouchPhase, x: f32) {
        self.input.push(HwEvent::Touch(TouchEvent {
            slot,
            phase,
            x,
            y: 30.0,
            time_us: 0,
        }));
        self.pump();
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn hello(rig: &mut Rig) -> Conn {
    let a = rig.connect();
    a.send(&AgentMsg::Hello {
        proto: PROTO,
        agent: "test".into(),
    })
    .unwrap();
    assert!(matches!(
        rig.recv(&a),
        DaemonMsg::Welcome {
            role: Role::Active,
            ..
        }
    ));
    a
}

/// "Copy" (Ctrl+C) key region + an agent region, drawn into a sealed memfd and presented.
fn present_frame(rig: &mut Rig, a: &Conn) -> (ShmBuffer, Vec<Region>) {
    let mut r = Renderer::new(Fonts::embedded());
    let scene = Scene::new(vec![
        Item::text("Copy").fill(1),
        Item::text("widget").fill(3),
    ]);
    let sheet = Stylesheet::classic();
    let px = r.render_new(&scene, &sheet, 2008, 60);
    let rects = r.layout(&scene, &sheet, 2008, 60).items;
    let mut buf = ShmBuffer::new(2008, 60, PixelFormat::Xrgb8888).unwrap();
    let stride = buf.stride as usize;
    blit_xrgb8888(&px, Orientation::Landscape, None, buf.pixels_mut(), stride);
    a.send_with_fd(&buf.attach_msg(1), Some(buf.as_fd()))
        .unwrap();
    let rect = |i: usize| Rect::new(rects[i].x as u32, 0, rects[i].w as u32, 60);
    let regions = vec![
        Region {
            id: 1,
            rect: rect(0),
            kind: RegionKind::Key { keys: vec![29, 46] },
        },
        Region {
            id: 2,
            rect: rect(1),
            kind: RegionKind::Agent,
        },
    ];
    a.send(&AgentMsg::Present(Present {
        serial: 1,
        layer: Layer::Base,
        image: 1,
        pressed: None,
        regions: regions.clone(),
        damage: None,
    }))
    .unwrap();
    assert_eq!(rig.recv(a), DaemonMsg::Presented { serial: 1 });
    assert!(rig.server.daemon().agent_drives());
    assert_eq!(rig.hw.display.frame().data(), px.data());
    (buf, regions)
}

#[test]
fn agent_over_a_real_socket() {
    let mut rig = Rig::new("e2e");
    let a = hello(&mut rig);
    let (_buf, regions) = present_frame(&mut rig, &a);

    // Key region through the pollable input source.
    let copy = regions[0].rect.x as f32 + 10.0;
    rig.touch(0, TouchPhase::Down, copy);
    rig.touch(0, TouchPhase::Up, copy);
    use KeyEvent::{Down, Up};
    assert_eq!(rig.hw.keys.take(), vec![Down(29), Down(46), Up(46), Up(29)]);

    // Agent region: forwarded, and the first one pings.
    rig.touch(1, TouchPhase::Down, regions[1].rect.x as f32 + 5.0);
    assert!(matches!(
        rig.recv(&a),
        DaemonMsg::Touch {
            region: 2,
            phase: TouchPhase::Down,
            ..
        }
    ));
    let DaemonMsg::Ping { nonce } = rig.recv(&a) else {
        panic!("expected ping");
    };
    a.send(&AgentMsg::Pong { nonce }).unwrap();
    rig.touch(1, TouchPhase::Up, 0.0);
    assert!(matches!(
        rig.recv(&a),
        DaemonMsg::Touch {
            phase: TouchPhase::Up,
            ..
        }
    ));

    // The agent crashes: hangup → safe layer.
    drop(a);
    rig.pump();
    assert!(!rig.server.daemon().agent_drives());
    assert!(rig.server.daemon().active_client().is_none());
}

#[test]
fn deadlines_drive_the_loop_and_idle_blocks() {
    let mut rig = Rig::new("deadline");
    let a = hello(&mut rig);
    a.send(&AgentMsg::DisplayPolicy(DisplayPolicy {
        dim_after_ms: Some(300),
        off_after_ms: Some(0),
        ..Default::default()
    }))
    .unwrap();
    rig.pump();
    // One run_once with a generous cap returns at the dim deadline, not the cap.
    let t = Instant::now();
    while rig.server.daemon().display_state() == DisplayState::On {
        rig.server.run_once(Some(Duration::from_secs(5))).unwrap();
        assert!(t.elapsed() < Duration::from_secs(2), "deadline missed");
    }
    assert_eq!(
        rig.recv(&a),
        DaemonMsg::Display {
            state: DisplayState::Dimmed
        }
    );
    assert_eq!(rig.hw.backlight.current(), Some(1));
    // Dimmed with off disabled: nothing left to time, so the loop would block on input.
    assert_eq!(rig.server.daemon().next_deadline(), None);
    let t = Instant::now();
    rig.server
        .run_once(Some(Duration::from_millis(100)))
        .unwrap();
    assert!(t.elapsed() >= Duration::from_millis(90), "no busy loop");
}

#[test]
fn bad_attachment_is_refused() {
    let mut rig = Rig::new("badfd");
    let a = hello(&mut rig);
    // An unsealed memfd.
    let fd = rustix::fs::memfd_create("x", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    rustix::fs::ftruncate(&fd, 2008 * 60 * 4).unwrap();
    a.send_with_fd(
        &AgentMsg::AttachBuffer {
            id: 1,
            width: 2008,
            height: 60,
            stride: 2008 * 4,
            format: PixelFormat::Xrgb8888,
        },
        Some(fd.as_fd()),
    )
    .unwrap();
    assert!(matches!(
        rig.recv(&a),
        DaemonMsg::Error {
            code: sliver_ipc::proto::ErrorCode::InvalidBuffer,
            ..
        }
    ));
}

/// `--touch-socket`: text lines on a datagram socket go through the real touch path. Without an
/// agent the built-in layer shows, so a tap on F1 presses F1; a bad line doesn't stop the rest.
#[test]
fn touch_socket_injects_through_the_real_touch_path() {
    use sliverd::inject::TouchSocket;
    use std::os::unix::net::UnixDatagram;

    let listener_path = socket_path("inject-ipc");
    let inject_path = socket_path("inject");
    let listener = Listener::bind(&listener_path).unwrap();
    let hw = FakeHardware::new(2008, 60);
    let panel = PanelInfo {
        width: 2008,
        height: 60,
        format: PixelFormat::Xrgb8888,
        max_brightness: 255,
        model: "Mac14,7".into(),
    };
    let daemon = Daemon::new(hw.backends(), panel, &Config::default(), None, 0);
    let src = TouchSocket::bind(&inject_path, 60).unwrap();
    let session = FixedSession::new(Some(rustix::process::getuid().as_raw())).unwrap();
    let mut server = Server::new(
        daemon,
        listener,
        vec![Box::new(src)],
        Box::new(session),
        Instant::now(),
    )
    .unwrap();

    const KEY_F1: u16 = 59;
    let f1 = server
        .daemon()
        .visible_regions()
        .iter()
        .find(|r| r.kind == RegionKind::Key { keys: vec![KEY_F1] })
        .expect("the built-in layer has F1")
        .rect;
    let x = f1.x as f32 + f1.w as f32 / 2.0;

    let client = UnixDatagram::unbound().unwrap();
    client
        .send_to(format!("bogus line\ntap {x}\n").as_bytes(), &inject_path)
        .unwrap();
    for _ in 0..3 {
        server.run_once(Some(Duration::from_millis(5))).unwrap();
    }
    use KeyEvent::{Down, Up};
    assert_eq!(hw.keys.take(), vec![Down(KEY_F1), Up(KEY_F1)]);

    // Held across datagrams: the key stays down until `up`.
    client
        .send_to(format!("down {x} 2").as_bytes(), &inject_path)
        .unwrap();
    server.run_once(Some(Duration::from_millis(5))).unwrap();
    assert_eq!(hw.keys.take(), vec![Down(KEY_F1)]);
    client.send_to(b"up 2", &inject_path).unwrap();
    server.run_once(Some(Duration::from_millis(5))).unwrap();
    assert_eq!(hw.keys.take(), vec![Up(KEY_F1)]);

    drop(server);
    assert!(
        !inject_path.exists(),
        "the socket file is removed with the source"
    );
    let _ = std::fs::remove_file(&listener_path);
}
