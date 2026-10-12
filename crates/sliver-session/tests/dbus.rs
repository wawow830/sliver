//! End-to-end: the real sources against mock logind/UPower/MPRIS services on a private
//! `dbus-daemon`. Skipped (with a note) when `dbus-daemon` is not installed.

use sliver_session::{BatteryState, PlaybackStatus, Session, SessionSource, SessionState};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::blocking::Connection;

mod common;
use common::Bus;
use zbus::zvariant::{OwnedValue, Value};

fn props_changed(conn: &Connection, path: &str, iface: &str) {
    let changed: HashMap<&str, Value> = HashMap::new();
    let invalidated: Vec<&str> = Vec::new();
    conn.emit_signal(
        None::<&str>,
        path,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &(iface, changed, invalidated),
    )
    .unwrap();
}

#[derive(Default, Clone)]
struct Login {
    locked: bool,
    idle: bool,
    block: &'static str,
    inhibits: u32,
}
struct LoginSession(Arc<Mutex<Login>>);
struct LoginManager(Arc<Mutex<Login>>);

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl LoginSession {
    #[zbus(property)]
    fn locked_hint(&self) -> bool {
        self.0.lock().unwrap().locked
    }
    #[zbus(property)]
    fn idle_hint(&self) -> bool {
        self.0.lock().unwrap().idle
    }
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl LoginManager {
    #[zbus(property)]
    fn block_inhibited(&self) -> String {
        self.0.lock().unwrap().block.into()
    }
    fn inhibit(&self, what: &str, _who: &str, _why: &str, mode: &str) -> zbus::zvariant::OwnedFd {
        assert_eq!((what, mode), ("idle", "block"));
        self.0.lock().unwrap().inhibits += 1;
        std::os::fd::OwnedFd::from(std::fs::File::open("/dev/null").unwrap()).into()
    }
    fn get_session(&self, _id: &str) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::try_from("/org/freedesktop/login1/session/_31")
            .unwrap()
            .into()
    }
}

#[derive(Default)]
struct Upower {
    on_battery: bool,
    pct: f64,
    state: u32,
}
struct UpowerRoot(Arc<Mutex<Upower>>);
struct UpowerDevice(Arc<Mutex<Upower>>);

#[zbus::interface(name = "org.freedesktop.UPower")]
impl UpowerRoot {
    #[zbus(property)]
    fn on_battery(&self) -> bool {
        self.0.lock().unwrap().on_battery
    }
    #[zbus(property)]
    fn lid_is_closed(&self) -> bool {
        false
    }
}

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl UpowerDevice {
    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.0.lock().unwrap().pct
    }
    #[zbus(property)]
    fn state(&self) -> u32 {
        self.0.lock().unwrap().state
    }
    #[zbus(property)]
    fn is_present(&self) -> bool {
        true
    }
}

#[derive(Clone)]
struct Track {
    status: &'static str,
    title: &'static str,
}
struct MprisRoot;
struct MprisPlayer(Arc<Mutex<Track>>);

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl MprisRoot {
    #[zbus(property)]
    fn identity(&self) -> String {
        "Mock Player".into()
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayer {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.0.lock().unwrap().status.into()
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let t = self.0.lock().unwrap().title;
        let mut m: HashMap<String, OwnedValue> = HashMap::new();
        m.insert("xesam:title".into(), Value::from(t).try_into().unwrap());
        m.insert(
            "xesam:artist".into(),
            Value::from(vec!["A", "B"]).try_into().unwrap(),
        );
        // some players send the length as u64, others as i64
        m.insert(
            "mpris:length".into(),
            Value::from(215_000_000u64).try_into().unwrap(),
        );
        m
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        42
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    fn play_pause(&self) {
        let mut t = self.0.lock().unwrap();
        t.status = if t.status == "Playing" {
            "Paused"
        } else {
            "Playing"
        };
    }
}

fn spawn_states(session: Session) -> Receiver<SessionState> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut s = session;
        while let Ok(Some(st)) = s.next_change() {
            if tx.send(st).is_err() {
                return;
            }
        }
    });
    rx
}

fn next(rx: &Receiver<SessionState>) -> SessionState {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("a state change within 10s")
}

#[test]
fn live_sources_against_mock_services() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };

    // logind
    let login = Arc::new(Mutex::new(Login::default()));
    let logind = bus.service("org.freedesktop.login1");
    logind
        .object_server()
        .at("/org/freedesktop/login1", LoginManager(login.clone()))
        .unwrap();
    logind
        .object_server()
        .at(
            "/org/freedesktop/login1/session/_31",
            LoginSession(login.clone()),
        )
        .unwrap();
    // upower
    let up = Arc::new(Mutex::new(Upower {
        pct: 80.4,
        state: 2,
        on_battery: true,
    }));
    let upower = bus.service("org.freedesktop.UPower");
    upower
        .object_server()
        .at("/org/freedesktop/UPower", UpowerRoot(up.clone()))
        .unwrap();
    upower
        .object_server()
        .at(
            "/org/freedesktop/UPower/devices/DisplayDevice",
            UpowerDevice(up.clone()),
        )
        .unwrap();
    // one paused and one playing MPRIS player, plus playerctld which must be ignored
    let t1 = Arc::new(Mutex::new(Track {
        status: "Paused",
        title: "one",
    }));
    let p1 = bus.service("org.mpris.MediaPlayer2.mock1");
    p1.object_server()
        .at("/org/mpris/MediaPlayer2", MprisRoot)
        .unwrap();
    p1.object_server()
        .at("/org/mpris/MediaPlayer2", MprisPlayer(t1.clone()))
        .unwrap();
    let ctld = bus.service("org.mpris.MediaPlayer2.playerctld");
    ctld.object_server()
        .at(
            "/org/mpris/MediaPlayer2",
            MprisPlayer(Arc::new(Mutex::new(Track {
                status: "Playing",
                title: "ghost",
            }))),
        )
        .unwrap();

    let client = bus.connect();
    let session = Session::start(client.clone(), client.clone());
    let handle = session.handle();
    let rx = spawn_states(session);

    // initial snapshot
    let s = next(&rx);
    assert!(!s.locked && !s.idle && !s.sleeping);
    assert_eq!(s.power.on_battery, Some(true));
    assert_eq!(s.power.battery_pct, Some(80));
    assert_eq!(s.power.battery_state, BatteryState::Discharging);
    let m = s.media.expect("paused mock1 is the only real player");
    assert_eq!(m.player, "org.mpris.MediaPlayer2.mock1");
    assert_eq!(
        (m.identity.as_str(), m.title.as_str(), m.artist.as_str()),
        ("Mock Player", "one", "A, B")
    );
    assert_eq!(m.status, PlaybackStatus::Paused);
    assert_eq!(m.length_us, Some(215_000_000));
    assert_eq!(m.position_us, Some(42));
    assert!(m.can_next && m.can_play_pause && !m.can_seek);

    // idle inhibitors held by others show up; caffeine holds ours
    login.lock().unwrap().block = "idle:sleep";
    props_changed(
        &logind,
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    );
    assert!(next(&rx).idle_inhibited);
    handle.set_caffeine(true).unwrap();
    assert!(next(&rx).caffeine);
    assert_eq!(login.lock().unwrap().inhibits, 1);
    handle.set_caffeine(true).unwrap(); // idempotent: no second inhibitor
    assert_eq!(login.lock().unwrap().inhibits, 1);
    handle.set_caffeine(false).unwrap();
    assert!(!next(&rx).caffeine);

    // brightness is pushed through the handle (sysfs has no signal)
    let root = std::env::temp_dir().join(format!("sliver-sysfs-dbus-{}", std::process::id()));
    let bl = root.join("backlight/apple-panel-bl");
    std::fs::create_dir_all(&bl).unwrap();
    std::fs::write(bl.join("max_brightness"), "200\n").unwrap();
    std::fs::write(bl.join("brightness"), "100\n").unwrap();
    let sysfs = sliver_session::brightness::Sysfs::new(&root);
    assert!(handle.refresh_brightness(&sysfs));
    assert_eq!(next(&rx).brightness.screen_pct, Some(50));
    std::fs::remove_dir_all(&root).unwrap();

    // lock via hint + PropertiesChanged
    login.lock().unwrap().locked = true;
    props_changed(
        &logind,
        "/org/freedesktop/login1/session/_31",
        "org.freedesktop.login1.Session",
    );
    assert!(next(&rx).locked);
    // unlock via the Unlock edge only
    logind
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/login1/session/_31",
            "org.freedesktop.login1.Session",
            "Unlock",
            &(),
        )
        .unwrap();
    assert!(!next(&rx).locked);
    // another session's Lock must not lock us
    logind
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/login1/session/_99",
            "org.freedesktop.login1.Session",
            "Lock",
            &(),
        )
        .unwrap();
    // sleep
    logind
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForSleep",
            &(true,),
        )
        .unwrap();
    let s = next(&rx);
    assert!(s.sleeping && !s.locked);

    // power
    *up.lock().unwrap() = Upower {
        pct: 79.6,
        state: 1,
        on_battery: false,
    };
    props_changed(
        &upower,
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
    );
    let s = next(&rx);
    assert_eq!(s.power.battery_pct, Some(80));
    assert_eq!(s.power.battery_state, BatteryState::Charging);

    // a second player starts playing: it takes over; the playerctld ghost never shows
    let t2 = Arc::new(Mutex::new(Track {
        status: "Playing",
        title: "two",
    }));
    let p2 = bus.service("org.mpris.MediaPlayer2.mock2");
    p2.object_server()
        .at("/org/mpris/MediaPlayer2", MprisRoot)
        .unwrap();
    p2.object_server()
        .at("/org/mpris/MediaPlayer2", MprisPlayer(t2.clone()))
        .unwrap();
    let m = loop {
        let m = next(&rx).media.unwrap();
        if m.player.ends_with("mock2") {
            break m;
        }
    };
    assert_eq!(
        (m.status, m.title.as_str()),
        (PlaybackStatus::Playing, "two")
    );

    // control: PlayPause toggles it, and the change shows up
    sliver_session::mpris::control(
        &client,
        &m.player,
        sliver_session::mpris::Command::PlayPause,
    )
    .unwrap();
    props_changed(
        &p2,
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    );
    let m = loop {
        let m = next(&rx).media.unwrap();
        if m.status == PlaybackStatus::Paused {
            break m;
        }
    };
    // both paused now: the more recently changed one (mock2) stays selected
    assert!(m.player.ends_with("mock2"));
}
