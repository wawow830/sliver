//! Notification counting via bus monitoring, against a mock notification server.

use sliver_session::{Session, SessionSource, SessionState};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::zvariant::OwnedValue;

mod common;
use common::Bus;

#[derive(Default)]
struct Server {
    next_id: u32,
    inhibited: bool,
}
struct Mock(Arc<Mutex<Server>>);

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Mock {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app: &str,
        replaces_id: u32,
        _icon: &str,
        _summary: &str,
        _body: &str,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _timeout: i32,
    ) -> u32 {
        let mut s = self.0.lock().unwrap();
        if replaces_id != 0 {
            return replaces_id;
        }
        s.next_id += 1;
        s.next_id
    }
    #[zbus(property)]
    fn inhibited(&self) -> bool {
        self.0.lock().unwrap().inhibited
    }
}

fn notify(conn: &zbus::blocking::Connection, replaces: u32) -> u32 {
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "test",
            replaces,
            "",
            "private summary",
            "private body",
            Vec::<String>::new(),
            HashMap::<String, zbus::zvariant::Value>::new(),
            -1i32,
        ),
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap()
}

fn next(rx: &Receiver<SessionState>) -> SessionState {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("a state change within 10s")
}

#[test]
fn counts_notifications_and_reads_dnd() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let state = Arc::new(Mutex::new(Server::default()));
    let server = bus.service("org.freedesktop.Notifications");
    server
        .object_server()
        .at("/org/freedesktop/Notifications", Mock(state.clone()))
        .unwrap();

    let client = bus.connect();
    let session = Session::start(client.clone(), client.clone());
    let handle = session.handle();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut s = session;
        while let Ok(Some(st)) = s.next_change() {
            if tx.send(st).is_err() {
                return;
            }
        }
    });
    let _initial = next(&rx);
    handle
        .start_notifications(bus.connect(), bus.connect())
        .unwrap();
    assert_eq!(next(&rx).notifications.dnd, Some(false));

    let a = notify(&client, 0);
    assert_eq!(next(&rx).notifications.count, 1);
    let b = notify(&client, 0);
    assert_eq!(next(&rx).notifications.count, 2);
    assert_eq!(notify(&client, a), a, "replace keeps the count");
    server
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "NotificationClosed",
            &(b, 1u32),
        )
        .unwrap();
    assert_eq!(next(&rx).notifications.count, 1);

    // KDE-style DND: the server's Inhibited property
    state.lock().unwrap().inhibited = true;
    server
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                "org.freedesktop.Notifications",
                HashMap::<&str, zbus::zvariant::Value>::new(),
                Vec::<&str>::new(),
            ),
        )
        .unwrap();
    assert_eq!(next(&rx).notifications.dnd, Some(true));
}
