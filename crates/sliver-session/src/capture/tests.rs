use super::*;
use std::sync::mpsc::channel;

fn dict_pod(props: &[(&str, &str)]) -> Vec<u8> {
    let mut b = StructBuilder::default().int(props.len() as i32);
    for (k, v) in props {
        b = b.string(k).string(v);
    }
    b.build()
}

fn global(id: i32, ty: &str, props: &[(&str, &str)]) -> Vec<u8> {
    let body = StructBuilder::default()
        .int(id)
        .int(0x1c0)
        .string(ty)
        .int(3)
        .pod(&dict_pod(props))
        .build();
    message(REGISTRY, EV_REGISTRY_GLOBAL, 0, &body)
}

fn node_info(proxy: u32, id: i32, state: i32, props: &[(&str, &str)]) -> Vec<u8> {
    let body = StructBuilder::default()
        .int(id)
        .int(1)
        .int(1)
        .long(0x1c)
        .int(0)
        .int(0)
        .id(state as u32)
        .none()
        .pod(&dict_pod(props))
        .int(0)
        .build();
    message(proxy, EV_NODE_INFO, 0, &body)
}

fn remove(id: i32) -> Vec<u8> {
    message(
        REGISTRY,
        EV_REGISTRY_GLOBAL_REMOVE,
        0,
        &StructBuilder::default().int(id).build(),
    )
}

const MIC: &[(&str, &str)] = &[
    ("media.class", "Stream/Input/Audio"),
    ("application.name", "Firefox"),
];

#[test]
fn filter_chain_halves_are_not_microphones() {
    let p: Props = [
        ("media.class", "Stream/Input/Audio"),
        ("node.name", "audio_effect.j493-mic"),
        ("node.link-group", "filter-chain-1-18"),
    ]
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .into();
    assert_eq!(classify(&p), None);
}

/// Reads one message the client sent.
fn read_msg(s: &mut UnixStream) -> (u32, u8, Vec<Pod>) {
    let mut head = [0u8; 16];
    s.read_exact(&mut head).unwrap();
    let id = u32::from_le_bytes(head[0..4].try_into().unwrap());
    let os = u32::from_le_bytes(head[4..8].try_into().unwrap());
    let mut body = vec![0u8; (os & 0xffffff) as usize];
    s.read_exact(&mut body).unwrap();
    let (pod, _) = parse_pod(&body).unwrap();
    let mut flat = Vec::new();
    flatten(&pod, &mut flat);
    (id, (os >> 24) as u8, flat.into_iter().cloned().collect())
}

/// A client talking to a scripted server: `script` gets the server's end and can send events.
fn session(script: impl FnOnce(&mut UnixStream) + Send + 'static) -> Vec<Capture> {
    let (client, mut server) = UnixStream::pair().unwrap();
    let (tx, rx) = channel();
    let t = std::thread::spawn(move || {
        let _ = run(client, move |c| tx.send(c).is_ok());
    });
    // The handshake: Hello, who we are, then GetRegistry.
    assert_eq!(read_msg(&mut server).1, CORE_HELLO);
    let (id, op, _) = read_msg(&mut server);
    assert_eq!((id, op), (CLIENT, CLIENT_UPDATE_PROPERTIES));
    let (id, op, args) = read_msg(&mut server);
    assert_eq!((id, op), (CORE, CORE_GET_REGISTRY));
    assert_eq!(args, [Pod::Int(3), Pod::Int(2)]);
    script(&mut server);
    drop(server);
    t.join().unwrap();
    rx.try_iter().collect()
}

#[test]
fn pods_round_trip() {
    let b = StructBuilder::default()
        .int(-7)
        .string("héllo")
        .long(1 << 40)
        .none()
        .pod(&StructBuilder::default().int(1).string("x").build())
        .build();
    let (pod, n) = parse_pod(&b).unwrap();
    assert_eq!(n, b.len());
    assert_eq!(
        pod,
        Pod::Struct(vec![
            Pod::Int(-7),
            Pod::Str("héllo".into()),
            Pod::Long(1 << 40),
            Pod::None,
            Pod::Struct(vec![Pod::Int(1), Pod::Str("x".into())]),
        ])
    );
    assert!(
        parse_pod(&b[..b.len() - 3]).is_none(),
        "truncated is not a panic"
    );
}

#[test]
fn a_running_recording_stream_is_the_microphone() {
    let seen = session(|s| {
        s.write_all(&global(40, NODE_INTERFACE, MIC)).unwrap();
        // The client binds it (proxy 3) and the server answers with the node's info.
        let (id, op, args) = read_msg(s);
        assert_eq!((id, op), (REGISTRY, REGISTRY_BIND));
        assert_eq!(args[0], Pod::Int(40));
        assert_eq!(args[1], Pod::Str(NODE_INTERFACE.into()));
        assert_eq!(args[3], Pod::Int(3));
        s.write_all(&node_info(3, 40, 2, &[])).unwrap(); // idle: not capturing yet
        s.write_all(&node_info(3, 40, 3, &[])).unwrap(); // running
        s.write_all(&remove(40)).unwrap();
    });
    assert_eq!(
        seen,
        [
            Capture {
                mic: vec!["Firefox".into()],
                ..Capture::default()
            },
            Capture::default()
        ]
    );
}

#[test]
fn screens_cameras_and_things_that_are_not_captures() {
    let seen = session(|s| {
        let streams: [(i32, &[(&str, &str)]); 6] = [
            (
                50,
                &[
                    ("media.class", "Stream/Input/Video"),
                    ("application.name", "OBS"),
                ],
            ),
            (
                51,
                &[
                    ("media.class", "Stream/Input/Video"),
                    ("application.name", "Zoom"),
                    ("media.role", "Camera"),
                ],
            ),
            (
                52,
                &[
                    ("media.class", "Stream/Input/Audio"),
                    ("application.name", "Recorder"),
                    ("stream.monitor", "true"),
                ],
            ),
            (
                53,
                &[
                    ("media.class", "Stream/Input/Audio"),
                    ("application.name", "Volume Control"),
                    ("application.id", "org.PulseAudio.pavucontrol"),
                ],
            ),
            (
                54,
                &[
                    ("media.class", "Stream/Output/Audio"),
                    ("application.name", "Music"),
                ],
            ),
            (
                55,
                &[("media.class", "Audio/Source"), ("node.name", "alsa_input")],
            ),
        ];
        let mut proxy = 3;
        for (id, props) in streams {
            s.write_all(&global(id, NODE_INTERFACE, props)).unwrap();
        }
        // Only the two video streams are even bound.
        for _ in 0..2 {
            assert_eq!(read_msg(s).1, REGISTRY_BIND);
        }
        for id in [50, 51] {
            s.write_all(&node_info(proxy, id, 3, &[])).unwrap();
            proxy += 1;
        }
        // A port is not a node.
        s.write_all(&global(60, "PipeWire:Interface:Port", MIC))
            .unwrap();
    });
    assert_eq!(
        seen.last().unwrap(),
        &Capture {
            camera: vec!["Zoom".into()],
            screen: vec!["OBS".into()],
            mic: vec![],
        }
    );
}

#[test]
fn property_updates_merge_and_a_ping_is_answered() {
    let seen = session(|s| {
        s.write_all(&global(
            40,
            NODE_INTERFACE,
            &[("media.class", "Stream/Input/Audio")],
        ))
        .unwrap();
        read_msg(s); // bind
        s.write_all(&node_info(3, 40, 3, &[])).unwrap();
        // The name arrives later, with the next info; a bare update keeps it.
        s.write_all(&node_info(3, 40, 3, &[("application.name", "Discord")]))
            .unwrap();
        s.write_all(&node_info(3, 40, 3, &[])).unwrap();
        let ping = StructBuilder::default().int(0).int(77).build();
        s.write_all(&message(CORE, EV_CORE_PING, 0, &ping)).unwrap();
        let (id, op, args) = read_msg(s);
        assert_eq!((id, op), (CORE, CORE_PONG));
        assert_eq!(args, [Pod::Int(0), Pod::Int(77)]);
    });
    // First "?" (no name yet), then Discord, and nothing more for the bare update.
    let mics: Vec<_> = seen.iter().map(|c| c.mic.clone()).collect();
    assert_eq!(mics, [vec!["?".to_string()], vec!["Discord".to_string()]]);
}

#[test]
fn the_socket_path_follows_pipewire_s_rules() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    };
    assert_eq!(
        socket_path(env(&[("XDG_RUNTIME_DIR", "/run/user/1")])),
        Some("/run/user/1/pipewire-0".into())
    );
    assert_eq!(
        socket_path(env(&[
            ("XDG_RUNTIME_DIR", "/run/user/1"),
            ("PIPEWIRE_RUNTIME_DIR", "/p"),
            ("PIPEWIRE_REMOTE", "pipewire-1"),
        ])),
        Some("/p/pipewire-1".into())
    );
    assert_eq!(
        socket_path(env(&[("PIPEWIRE_REMOTE", "/tmp/sock")])),
        Some("/tmp/sock".into())
    );
    assert_eq!(socket_path(env(&[])), None);
}

/// A PipeWire stand-in on a unix socket: each connection gets a running Firefox recording.
fn fake_pipewire(path: &Path) -> std::sync::mpsc::Receiver<()> {
    let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { return };
            let tx = tx.clone();
            std::thread::spawn(move || {
                for _ in 0..3 {
                    read_msg(&mut s); // hello, who we are, get_registry
                }
                s.write_all(&global(40, NODE_INTERFACE, MIC)).unwrap();
                read_msg(&mut s); // bind
                s.write_all(&node_info(3, 40, 3, &[])).unwrap();
                let _ = tx.send(()); // connected and served
                // Until the client hangs up.
                let mut b = [0u8; 64];
                while s.read(&mut b).is_ok_and(|n| n > 0) {}
                let _ = tx.send(());
            });
        }
    });
    rx
}

/// The bar turning off cuts the PipeWire connection (nothing wakes us for registry churn), and
/// turning it on connects again and learns the current state afresh (invariant 3).
#[test]
fn a_dark_bar_hangs_up_and_wake_reconnects() {
    let Some(tb) = sliver_testbus::TestBus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let conn = || {
        zbus::blocking::connection::Builder::address(tb.address())
            .unwrap()
            .build()
            .unwrap()
    };
    let dir = std::env::temp_dir().join(format!("sliver-capture-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("dev")).unwrap();
    std::fs::create_dir_all(dir.join("proc")).unwrap();
    let sock = dir.join("pipewire-0");
    let events = fake_pipewire(&sock);

    let mut session = crate::Session::start(conn(), conn());
    let handle = session.handle();
    // Empty stand-ins for /dev and /proc: the test never reads the real machine's cameras.
    handle.start_capture_in(sock.clone(), dir.join("dev"), dir.join("proc"));
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        use crate::SessionSource;
        while let Ok(Some(s)) = session.next_change() {
            if tx.send(s.capture).is_err() {
                return;
            }
        }
    });
    let wait = |want_mic: bool| {
        let t = std::time::Instant::now();
        while t.elapsed() < std::time::Duration::from_secs(10) {
            if let Ok(c) = rx.recv_timeout(std::time::Duration::from_millis(100))
                && c.mic.is_empty() != want_mic
            {
                return true;
            }
        }
        false
    };
    assert!(wait(false), "Firefox shows as a microphone user");
    assert!(
        events
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok()
    );

    handle.set_awake(false);
    // The server sees the hang-up (second signal), and it is not reconnected while dark.
    assert!(
        events
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok(),
        "the connection was cut when the bar went dark"
    );
    assert!(
        events
            .recv_timeout(std::time::Duration::from_millis(600))
            .is_err(),
        "no reconnect while dark"
    );

    handle.set_awake(true);
    assert!(
        events
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok(),
        "reconnected on wake"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A camera held through V4L2 and one PipeWire reports are the same list, de-duplicated; with no
/// PipeWire running at all the V4L2 holders still show.
#[test]
fn v4l2_holders_join_the_camera_list() {
    let Some(tb) = sliver_testbus::TestBus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let conn = || {
        zbus::blocking::connection::Builder::address(tb.address())
            .unwrap()
            .build()
            .unwrap()
    };
    let dir = std::env::temp_dir().join(format!("sliver-capture-v4l-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dev")).unwrap();
    std::fs::create_dir_all(dir.join("proc")).unwrap();
    std::fs::write(dir.join("dev/video0"), "").unwrap();
    let mut session = crate::Session::start(conn(), conn());
    let handle = session.handle();
    // No PipeWire socket at all.
    handle.start_capture_in(dir.join("no-pipewire"), dir.join("dev"), dir.join("proc"));
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        use crate::SessionSource;
        while let Ok(Some(s)) = session.next_change() {
            if tx.send(s.capture).is_err() {
                return;
            }
        }
    });
    // Zoom opens the camera.
    let fd = dir.join("proc/30/fd");
    std::fs::create_dir_all(&fd).unwrap();
    std::fs::write(dir.join("proc/30/comm"), "zoom\n").unwrap();
    std::os::unix::fs::symlink(dir.join("dev/video0"), fd.join("5")).unwrap();
    let _open = std::fs::File::open(dir.join("dev/video0")).unwrap();
    let t = std::time::Instant::now();
    let mut seen = None;
    while t.elapsed() < std::time::Duration::from_secs(8) && seen.is_none() {
        if let Ok(c) = rx.recv_timeout(std::time::Duration::from_millis(100))
            && !c.camera.is_empty()
        {
            seen = Some(c);
        }
    }
    assert_eq!(seen.expect("the camera shows").camera, ["zoom"]);
    // The merge itself: PipeWire says Zoom too, and OBS: one entry each, sorted.
    let gate = Gate::new();
    let (hub, rx2) = crate::Hub::new(crate::SessionState::default());
    gate.set_v4l2(&hub, vec!["zoom".into()]);
    gate.set_pipewire(
        &hub,
        Capture {
            camera: vec!["obs".into(), "zoom".into()],
            mic: vec!["zoom".into()],
            ..Capture::default()
        },
    );
    let last = rx2.try_iter().last().unwrap().capture;
    assert_eq!(last.camera, ["obs", "zoom"]);
    assert_eq!(last.mic, ["zoom"]);
    let _ = std::fs::remove_dir_all(&dir);
}
