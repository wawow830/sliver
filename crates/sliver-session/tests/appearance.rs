//! The accent colour and colour scheme from a mock settings portal.

use sliver_session::{Session, SessionSource, SessionState};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};

mod common;
use common::Bus;

struct Portal(Arc<Mutex<([f64; 3], u32)>>);

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl Portal {
    fn read_one(&self, namespace: &str, key: &str) -> OwnedValue {
        assert_eq!(namespace, "org.freedesktop.appearance");
        let (rgb, scheme) = *self.0.lock().unwrap();
        match key {
            "accent-color" => OwnedValue::try_from(Value::Structure(
                StructureBuilder::new()
                    .add_field(rgb[0])
                    .add_field(rgb[1])
                    .add_field(rgb[2])
                    .build()
                    .unwrap(),
            ))
            .unwrap(),
            _ => OwnedValue::try_from(Value::from(scheme)).unwrap(),
        }
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
fn accent_and_scheme_follow_the_portal_and_kde_fills_in() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let state = Arc::new(Mutex::new(([1.0, 0.0, 0.0], 1u32)));
    let portal = bus.service("org.freedesktop.portal.Desktop");
    portal
        .object_server()
        .at("/org/freedesktop/portal/desktop", Portal(state.clone()))
        .unwrap();

    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    let handle = session.handle();
    let rx = spawn_states(session);
    let _ = next(&rx);
    handle.start_appearance(bus.connect(), None);
    let a = next(&rx).appearance;
    assert_eq!((a.accent, a.dark), (Some([255, 0, 0]), Some(true)));

    *state.lock().unwrap() = ([0.0, 1.0, 0.0], 2);
    portal
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "SettingChanged",
            &(
                "org.freedesktop.appearance",
                "accent-color",
                Value::from(0u32),
            ),
        )
        .unwrap();
    let a = next(&rx).appearance;
    assert_eq!((a.accent, a.dark), (Some([0, 255, 0]), Some(false)));
}

#[test]
fn no_portal_falls_back_to_kdeglobals_without_starting_anything() {
    let Some(bus) = Bus::start() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("sliver-appearance-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("kdeglobals"),
        "[General]\nAccentColor=61,174,233\n",
    )
    .unwrap();
    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    let handle = session.handle();
    let rx = spawn_states(session);
    let _ = next(&rx);
    handle.start_appearance(bus.connect(), Some(dir.join("kdeglobals")));
    assert_eq!(next(&rx).appearance.accent, Some([61, 174, 233]));
    let _ = std::fs::remove_dir_all(&dir);
}
