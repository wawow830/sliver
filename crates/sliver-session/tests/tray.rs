//! StatusNotifierItem host against a mock watcher and mock items.

use sliver_session::tray::{self, Poke, TrayStatus};
use sliver_session::{Session, SessionSource, SessionState};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};

mod common;
use common::Bus;

#[derive(Default)]
struct Watcher {
    items: Vec<String>,
    hosts: Vec<String>,
}
struct MockWatcher(Arc<Mutex<Watcher>>);

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl MockWatcher {
    fn register_status_notifier_host(&self, name: &str) {
        self.0.lock().unwrap().hosts.push(name.into());
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.0.lock().unwrap().items.clone()
    }
}

#[derive(Default)]
struct Item {
    title: String,
    status: String,
    icon: String,
    activated: Vec<(i32, i32)>,
}
struct MockItem(Arc<Mutex<Item>>);

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl MockItem {
    #[zbus(property)]
    fn id(&self) -> String {
        "mock-app".into()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        self.0.lock().unwrap().title.clone()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        self.0.lock().unwrap().status.clone()
    }
    #[zbus(property)]
    fn category(&self) -> String {
        "ApplicationStatus".into()
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        self.0.lock().unwrap().icon.clone()
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<(i32, i32, Vec<u8>)> {
        // 1x1 ARGB: A=255 R=10 G=20 B=30
        vec![(1, 1, vec![255, 10, 20, 30])]
    }
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::try_from("/MenuBar")
            .unwrap()
            .into()
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    fn activate(&self, x: i32, y: i32) {
        self.0.lock().unwrap().activated.push((x, y));
    }
}

type Node = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

struct MockMenu(Arc<Mutex<Vec<i32>>>);

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl MockMenu {
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }
    fn get_layout(&self, _parent: i32, _depth: i32, _props: Vec<String>) -> (u32, Node) {
        let child = |id: i32, label: &str| {
            let mut p: HashMap<String, OwnedValue> = HashMap::new();
            p.insert("label".into(), Value::from(label).try_into().unwrap());
            OwnedValue::try_from(Value::Structure(
                StructureBuilder::new()
                    .add_field(id)
                    .add_field(p)
                    .add_field(Vec::<OwnedValue>::new())
                    .build()
                    .unwrap(),
            ))
            .unwrap()
        };
        (
            7,
            (0, HashMap::new(), vec![child(1, "_Open"), child(2, "Quit")]),
        )
    }
    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _ts: u32) {
        assert_eq!(event_id, "clicked");
        self.0.lock().unwrap().push(id);
    }
}

fn next(rx: &Receiver<SessionState>) -> SessionState {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("a state change within 10s")
}

#[test]
fn tray_follows_the_watcher_and_items() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let w = Arc::new(Mutex::new(Watcher::default()));
    let watcher = bus.service("org.kde.StatusNotifierWatcher");
    watcher
        .object_server()
        .at("/StatusNotifierWatcher", MockWatcher(w.clone()))
        .unwrap();

    // an item that is already registered when we start
    let i1 = Arc::new(Mutex::new(Item {
        title: "One".into(),
        status: "Active".into(),
        icon: "icon-one".into(),
        ..Default::default()
    }));
    let item1 = bus.service("org.example.One");
    item1
        .object_server()
        .at("/StatusNotifierItem", MockItem(i1.clone()))
        .unwrap();
    w.lock()
        .unwrap()
        .items
        .push("org.example.One/StatusNotifierItem".into());

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
    handle.start_tray(bus.connect());

    let t = next(&rx).tray;
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].id.as_str(), t[0].title.as_str()), ("mock-app", "One"));
    assert_eq!(t[0].icon_name, "icon-one");
    assert_eq!(t[0].status, TrayStatus::Active);
    assert_eq!(t[0].menu.as_deref(), Some("/MenuBar"));
    let px = t[0].icon.as_ref().unwrap();
    assert_eq!(
        (px.width, px.height, px.rgba.clone()),
        (1, 1, vec![10, 20, 30, 255])
    );
    assert_eq!(w.lock().unwrap().hosts.len(), 1, "we registered as a host");

    // a second item registers
    let i2 = Arc::new(Mutex::new(Item {
        title: "Two".into(),
        status: "Passive".into(),
        ..Default::default()
    }));
    let item2 = bus.service("org.example.Two");
    item2
        .object_server()
        .at("/StatusNotifierItem", MockItem(i2.clone()))
        .unwrap();
    watcher
        .emit_signal(
            None::<&str>,
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "StatusNotifierItemRegistered",
            &("org.example.Two/StatusNotifierItem",),
        )
        .unwrap();
    let t = next(&rx).tray;
    assert_eq!(
        t.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
        ["One", "Two"]
    );
    assert_eq!(t[1].status, TrayStatus::Passive);

    // item one changes its icon and says so
    i1.lock().unwrap().icon = "icon-new".into();
    item1
        .emit_signal(
            None::<&str>,
            "/StatusNotifierItem",
            "org.kde.StatusNotifierItem",
            "NewIcon",
            &(),
        )
        .unwrap();
    let t = next(&rx).tray;
    assert_eq!(t[0].icon_name, "icon-new");

    // controls reach the item
    tray::poke(&client, &t[0], Poke::Activate, 3, 4).unwrap();
    assert_eq!(i1.lock().unwrap().activated, vec![(3, 4)]);

    // the item's dbusmenu
    let clicks = Arc::new(Mutex::new(Vec::new()));
    item1
        .object_server()
        .at("/MenuBar", MockMenu(clicks.clone()))
        .unwrap();
    let m = sliver_session::menu::layout(&client, &t[0]).unwrap();
    assert_eq!(
        m.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
        ["Open", "Quit"]
    );
    sliver_session::menu::click(&client, &t[0], m[1].id).unwrap();
    assert_eq!(*clicks.lock().unwrap(), vec![2]);

    // unregister
    watcher
        .emit_signal(
            None::<&str>,
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "StatusNotifierItemUnregistered",
            &("org.example.One/StatusNotifierItem",),
        )
        .unwrap();
    let t = next(&rx).tray;
    assert_eq!(t.len(), 1);
    assert_eq!(t[0].title, "Two");
}

#[test]
fn no_watcher_means_empty_tray() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    session.handle().start_tray(bus.connect());
}
