//! The desktop's own lock state (`ActiveChanged`) against a mock GNOME-style screen saver.

use sliver_session::{Session, SessionSource, SessionState};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod common;
use common::Bus;

struct Saver(Arc<Mutex<bool>>);

#[zbus::interface(name = "org.gnome.ScreenSaver")]
impl Saver {
    fn get_active(&self) -> bool {
        *self.0.lock().unwrap()
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
fn screensaver_state_is_read_and_follows_active_changed() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let active = Arc::new(Mutex::new(true));
    let svc = bus.service("org.gnome.ScreenSaver");
    svc.object_server()
        .at("/org/gnome/ScreenSaver", Saver(active.clone()))
        .unwrap();

    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    let handle = session.handle();
    let rx = spawn_states(session);
    let first = next(&rx);
    assert_eq!(first.screensaver, None, "unknown until we look");
    handle.start_screensaver(bus.connect());
    assert_eq!(next(&rx).screensaver, Some(true), "read on start");

    for want in [false, true] {
        *active.lock().unwrap() = want;
        svc.emit_signal(
            None::<&str>,
            "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver",
            "ActiveChanged",
            &(want,),
        )
        .unwrap();
        assert_eq!(next(&rx).screensaver, Some(want));
    }
}

#[test]
fn no_screensaver_service_leaves_it_unknown_and_starts_nothing() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    let handle = session.handle();
    let rx = spawn_states(session);
    let _ = next(&rx);
    handle.start_screensaver(bus.connect());
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "nothing changed"
    );
}
