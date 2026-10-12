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

struct Nm(Arc<Mutex<u32>>);
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

struct Adapter;
struct Device1;
struct Battery1;

#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn powered(&self) -> bool {
        true
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
    os.at("/org/freedesktop/NetworkManager", Nm(state.clone()))
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
    os.at("/org/bluez/hci0", Adapter).unwrap();
    os.at("/org/bluez/hci0/dev_AA", Device1).unwrap();
    os.at("/org/bluez/hci0/dev_AA", Battery1).unwrap();

    let client = bus.connect();
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
    assert!(s.bluetooth.available && s.bluetooth.powered);
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
