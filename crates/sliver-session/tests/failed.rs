//! Failed systemd units against a mock systemd Manager on a private "system" and "user" bus.

use sliver_session::failed::{FailedUnits, Manager};
use sliver_session::{Session, SessionSource, SessionState};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::zvariant::OwnedObjectPath;

mod common;
use common::Bus;

const PATH: &str = "/org/freedesktop/systemd1";

#[derive(Default)]
struct Mgr {
    failed: Arc<Mutex<Vec<String>>>,
    subscribed: Arc<AtomicU32>,
}

type Unit = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl Mgr {
    #[zbus(property)]
    fn n_failed_units(&self) -> u32 {
        self.failed.lock().unwrap().len() as u32
    }
    fn subscribe(&self) {
        self.subscribed.fetch_add(1, Ordering::SeqCst);
    }
    fn list_units_filtered(&self, states: Vec<String>) -> Vec<Unit> {
        assert_eq!(states, ["failed"]);
        let p = OwnedObjectPath::try_from("/").unwrap();
        self.failed
            .lock()
            .unwrap()
            .iter()
            .map(|u| {
                (
                    u.clone(),
                    String::new(),
                    "loaded".into(),
                    "failed".into(),
                    "failed".into(),
                    String::new(),
                    p.clone(),
                    0,
                    String::new(),
                    p.clone(),
                )
            })
            .collect()
    }
}

struct Fake {
    conn: zbus::blocking::Connection,
    failed: Arc<Mutex<Vec<String>>>,
    subscribed: Arc<AtomicU32>,
}

impl Fake {
    fn on(bus: &Bus, failed: &[&str]) -> Fake {
        let conn = bus.service("org.freedesktop.systemd1");
        let m = Mgr {
            failed: Arc::new(Mutex::new(failed.iter().map(|s| s.to_string()).collect())),
            ..Default::default()
        };
        let (failed, subscribed) = (m.failed.clone(), m.subscribed.clone());
        conn.object_server().at(PATH, m).unwrap();
        Fake {
            conn,
            failed,
            subscribed,
        }
    }

    fn set(&self, failed: &[&str]) {
        *self.failed.lock().unwrap() = failed.iter().map(|s| s.to_string()).collect();
        let iface = self.conn.object_server().interface::<_, Mgr>(PATH).unwrap();
        zbus::block_on(async {
            iface
                .get()
                .n_failed_units_changed(iface.signal_emitter())
                .await
        })
        .unwrap();
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

/// The next state whose failed units satisfy `ok` (several updates may come in between).
fn until(rx: &Receiver<SessionState>, ok: impl Fn(&FailedUnits) -> bool) -> FailedUnits {
    let end = std::time::Instant::now() + Duration::from_secs(10);
    let mut last = None;
    while std::time::Instant::now() < end {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(s) if ok(&s.failed_units) => return s.failed_units,
            Ok(s) => last = Some(s.failed_units),
            Err(_) => {}
        }
    }
    panic!("no such state within 10s; last {last:?}");
}

fn m(units: &[&str]) -> Option<Manager> {
    Some(Manager {
        count: units.len() as u32,
        units: units.iter().map(|s| s.to_string()).collect(),
    })
}

#[test]
fn counts_and_names_follow_the_managers() {
    let (Some(system), Some(user)) = (Bus::start(), Bus::start()) else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let sys = Fake::on(&system, &["nfs.mount", "backup.service"]);
    let usr = Fake::on(&user, &[]);

    let client = system.connect();
    let session = Session::start(client, user.connect());
    let handle = session.handle();
    let rx = spawn_states(session);
    let first = until(&rx, |_| true);
    assert!(
        !first.watched,
        "nothing is watched until a layout uses the module"
    );
    handle.start_failed_units(system.connect(), user.connect());
    handle.start_failed_units(system.connect(), user.connect()); // once only
    let f = until(&rx, |f| f.system.is_some() && f.user.is_some());
    assert!(f.watched);
    assert_eq!(f.system, m(&["backup.service", "nfs.mount"]), "sorted");
    assert_eq!(f.user, m(&[]));
    assert_eq!(
        sys.subscribed.load(Ordering::SeqCst),
        1,
        "systemd only signals subscribers"
    );
    assert_eq!(usr.subscribed.load(Ordering::SeqCst), 1);

    usr.set(&["pipewire.service"]);
    let f = until(&rx, |f| f.user == m(&["pipewire.service"]));
    assert_eq!(f.system, m(&["backup.service", "nfs.mount"]));
    sys.set(&[]);
    let f = until(&rx, |f| f.system == m(&[]));
    assert_eq!(f.user, m(&["pipewire.service"]));
}

#[test]
fn no_systemd_leaves_it_unknown() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let session = Session::start(bus.connect(), bus.connect());
    let handle = session.handle();
    let rx = spawn_states(session);
    let _ = until(&rx, |_| true);
    handle.start_failed_units(bus.connect(), bus.connect());
    let f = until(&rx, |f| f.watched);
    assert_eq!((f.system, f.user), (None, None));
}
