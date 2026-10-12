//! NetworkManager and BlueZ sources against mock services on a private bus.

use sliver_session::bluetooth::BtDevice;
use sliver_session::network::{NetKind, NetState};
use sliver_session::{Session, SessionSource, SessionState};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

mod common;
use common::Bus;

fn op(s: &str) -> OwnedObjectPath {
    ObjectPath::try_from(s.to_string()).unwrap().into()
}

struct Nm(Arc<Mutex<u32>>, Arc<Mutex<bool>>);
struct Active;
struct Ip4;
struct Dev;
struct Wireless;
struct Ap(Arc<Mutex<u8>>);

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Nm {
    #[zbus(property)]
    fn state(&self) -> u32 {
        *self.0.lock().unwrap()
    }
    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        op("/org/freedesktop/NetworkManager/ActiveConnection/1")
    }
    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        *self.1.lock().unwrap()
    }
    #[zbus(property)]
    fn set_wireless_enabled(&mut self, v: bool) {
        *self.1.lock().unwrap() = v;
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "802-11-wireless".into()
    }
    #[zbus(property)]
    fn id(&self) -> String {
        "HomeNet".into()
    }
    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![op("/org/freedesktop/NetworkManager/Devices/2")]
    }
    #[zbus(property, name = "Ip4Config")]
    fn ip4(&self) -> OwnedObjectPath {
        op("/org/freedesktop/NetworkManager/IP4Config/1")
    }
    #[zbus(property)]
    fn vpn(&self) -> bool {
        false
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.IP4Config")]
impl Ip4 {
    #[zbus(property)]
    fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
        let mut m: HashMap<String, OwnedValue> = HashMap::new();
        m.insert(
            "address".into(),
            Value::from("192.168.1.5").try_into().unwrap(),
        );
        m.insert("prefix".into(), Value::from(24u32).try_into().unwrap());
        vec![m]
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Dev {
    #[zbus(property)]
    fn interface(&self) -> String {
        "wlan0".into()
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        op("/org/freedesktop/NetworkManager/AccessPoint/3")
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl Ap {
    #[zbus(property)]
    fn strength(&self) -> u8 {
        *self.0.lock().unwrap()
    }
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        b"HomeNet".to_vec()
    }
}

struct Adapter(Arc<Mutex<bool>>);
struct Device1;
struct Battery1;

#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        *self.0.lock().unwrap()
    }
    #[zbus(property)]
    fn set_powered(&mut self, v: bool) {
        *self.0.lock().unwrap() = v;
    }
}

#[zbus::interface(name = "org.bluez.Device1")]
impl Device1 {
    #[zbus(property)]
    fn connected(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn alias(&self) -> String {
        "Headphones".into()
    }
    #[zbus(property)]
    fn address(&self) -> String {
        "AA:BB:CC:DD:EE:FF".into()
    }
}

#[zbus::interface(name = "org.bluez.Battery1")]
impl Battery1 {
    #[zbus(property)]
    fn percentage(&self) -> u8 {
        80
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
fn network_and_bluetooth_follow_their_services() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let state = Arc::new(Mutex::new(70u32));
    let strength = Arc::new(Mutex::new(66u8));
    let nm = bus.service("org.freedesktop.NetworkManager");
    let os = nm.object_server();
    let wifi = Arc::new(Mutex::new(true));
    os.at(
        "/org/freedesktop/NetworkManager",
        Nm(state.clone(), wifi.clone()),
    )
    .unwrap();
    os.at("/org/freedesktop/NetworkManager/ActiveConnection/1", Active)
        .unwrap();
    os.at("/org/freedesktop/NetworkManager/IP4Config/1", Ip4)
        .unwrap();
    os.at("/org/freedesktop/NetworkManager/Devices/2", Dev)
        .unwrap();
    os.at("/org/freedesktop/NetworkManager/Devices/2", Wireless)
        .unwrap();
    os.at(
        "/org/freedesktop/NetworkManager/AccessPoint/3",
        Ap(strength.clone()),
    )
    .unwrap();

    let bz = bus.service("org.bluez");
    let os = bz.object_server();
    os.at("/", zbus::fdo::ObjectManager).unwrap();
    let powered = Arc::new(Mutex::new(true));
    os.at("/org/bluez/hci0", Adapter(powered.clone())).unwrap();
    os.at("/org/bluez/hci0/dev_AA", Device1).unwrap();
    os.at("/org/bluez/hci0/dev_AA", Battery1).unwrap();

    let client = bus.connect();
    let client2 = bus.connect();
    let session = Session::start(client.clone(), client);
    let rx = spawn_states(session);
    let s = next(&rx);
    assert_eq!(s.network.state, NetState::Connected);
    assert_eq!(s.network.kind, NetKind::Wifi);
    assert_eq!(s.network.name, "HomeNet");
    assert_eq!(s.network.ssid.as_deref(), Some("HomeNet"));
    assert_eq!(s.network.signal_pct, Some(66));
    assert_eq!(s.network.iface.as_deref(), Some("wlan0"));
    assert_eq!(s.network.ipv4.as_deref(), Some("192.168.1.5"));
    assert_eq!(s.network.wifi_enabled, Some(true));
    assert_eq!(s.bluetooth.adapter.as_deref(), Some("/org/bluez/hci0"));
    assert!(s.bluetooth.available && s.bluetooth.powered);

    // the switches write through to the services
    sliver_session::network::set_wifi(&client2, false).unwrap();
    assert!(!*wifi.lock().unwrap());
    sliver_session::bluetooth::set_powered(&client2, "/org/bluez/hci0", false).unwrap();
    assert!(!*powered.lock().unwrap());
    assert!(sliver_session::network::set_wifi(&bus.connect(), true).is_ok());
    assert!(*wifi.lock().unwrap());
    // the mocks announce their own property changes: let those states pass
    std::thread::sleep(Duration::from_millis(300));
    while rx.try_recv().is_ok() {}
    assert_eq!(
        s.bluetooth.connected,
        vec![BtDevice {
            name: "Headphones".into(),
            address: "AA:BB:CC:DD:EE:FF".into(),
            battery: Some(80)
        }]
    );

    // signal strength changes arrive as AccessPoint property changes
    *strength.lock().unwrap() = 40;
    nm.emit_signal(
        None::<&str>,
        "/org/freedesktop/NetworkManager/AccessPoint/3",
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &(
            "org.freedesktop.NetworkManager.AccessPoint",
            HashMap::<&str, Value>::new(),
            Vec::<&str>::new(),
        ),
    )
    .unwrap();
    assert_eq!(next(&rx).network.signal_pct, Some(40));
    // and the global state
    *state.lock().unwrap() = 20;
    nm.emit_signal(
        None::<&str>,
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &(
            "org.freedesktop.NetworkManager",
            HashMap::<&str, Value>::new(),
            Vec::<&str>::new(),
        ),
    )
    .unwrap();
    assert_eq!(next(&rx).network.state, NetState::Disconnected);
}

/// While the bar is off the NetworkManager subscription is dropped (invariant 3: signal strength
/// updates are ~1 wakeup/s on a real laptop); signals then change nothing, and when the bar wakes
/// the state is read again, so nothing is missed.
#[test]
fn dark_bar_drops_chatty_subscriptions_and_resyncs_on_wake() {
    let Some(bus) = Bus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let state = Arc::new(Mutex::new(70u32));
    let strength = Arc::new(Mutex::new(66u8));
    let nm = bus.service("org.freedesktop.NetworkManager");
    let os = nm.object_server();
    os.at(
        "/org/freedesktop/NetworkManager",
        Nm(state.clone(), Arc::new(Mutex::new(true))),
    )
    .unwrap();
    os.at("/org/freedesktop/NetworkManager/ActiveConnection/1", Active)
        .unwrap();
    os.at("/org/freedesktop/NetworkManager/IP4Config/1", Ip4)
        .unwrap();
    os.at("/org/freedesktop/NetworkManager/Devices/2", Dev)
        .unwrap();
    os.at("/org/freedesktop/NetworkManager/Devices/2", Wireless)
        .unwrap();
    os.at(
        "/org/freedesktop/NetworkManager/AccessPoint/3",
        Ap(strength.clone()),
    )
    .unwrap();

    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    let handle = session.handle();
    let rx = spawn_states(session);
    assert_eq!(next(&rx).network.signal_pct, Some(66));
    let ap_changed = || {
        nm.emit_signal(
            None::<&str>,
            "/org/freedesktop/NetworkManager/AccessPoint/3",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                "org.freedesktop.NetworkManager.AccessPoint",
                HashMap::<&str, Value>::new(),
                Vec::<&str>::new(),
            ),
        )
        .unwrap();
    };

    handle.set_awake(false);
    std::thread::sleep(Duration::from_millis(500)); // the rule removal runs on its own thread
    *strength.lock().unwrap() = 40;
    ap_changed();
    assert!(
        rx.recv_timeout(Duration::from_millis(700)).is_err(),
        "a dark bar must not hear NetworkManager"
    );
    // wake: the new strength is read at once, without any signal
    handle.set_awake(true);
    assert_eq!(next(&rx).network.signal_pct, Some(40));
    // and the subscription is back
    *strength.lock().unwrap() = 55;
    ap_changed();
    assert_eq!(next(&rx).network.signal_pct, Some(55));
}

#[test]
fn missing_services_leave_unknown() {
    let Some(bus) = Bus::start() else {
        return;
    };
    let client = bus.connect();
    let session = Session::start(client.clone(), client);
    let rx = spawn_states(session);
    let s = next(&rx);
    assert_eq!(s.network.state, NetState::Unknown);
    assert_eq!(s.network.kind, NetKind::None);
    assert!(!s.bluetooth.available);
}

struct Dunst(Arc<Mutex<bool>>);

#[zbus::interface(name = "org.dunstproject.cmd0")]
impl Dunst {
    #[zbus(property, name = "paused")]
    fn paused(&self) -> bool {
        *self.0.lock().unwrap()
    }
    #[zbus(property, name = "paused")]
    fn set_paused(&mut self, v: bool) {
        *self.0.lock().unwrap() = v;
    }
}

#[test]
fn dnd_toggles_on_dunst_and_says_so_elsewhere() {
    let Some(bus) = Bus::start() else {
        return;
    };
    let client = bus.connect();
    let e = sliver_session::notifications::set_dnd(&client, true).unwrap_err();
    assert!(e.contains("Do Not Disturb"), "{e}");
    let paused = Arc::new(Mutex::new(false));
    let d = bus.service("org.freedesktop.Notifications");
    d.object_server()
        .at("/org/dunstproject/cmd0", Dunst(paused.clone()))
        .unwrap();
    sliver_session::notifications::set_dnd(&client, true).unwrap();
    assert!(*paused.lock().unwrap());
    sliver_session::notifications::set_dnd(&client, false).unwrap();
    assert!(!*paused.lock().unwrap());
}
