//! BlueZ: is Bluetooth on, and which devices are connected (with battery where reported).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

type Props = HashMap<String, OwnedValue>;
type Managed = HashMap<OwnedObjectPath, HashMap<String, Props>>;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BtDevice {
    pub name: String,
    pub address: String,
    /// `org.bluez.Battery1.Percentage`, if the device reports one.
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bluetooth {
    /// A Bluetooth adapter exists.
    pub available: bool,
    pub powered: bool,
    pub connected: Vec<BtDevice>,
}

fn get<T: TryFrom<OwnedValue>>(p: &Props, k: &str) -> Option<T> {
    T::try_from(p.get(k)?.try_clone().ok()?).ok()
}

/// Fold `GetManagedObjects` into [`Bluetooth`].
pub fn from_managed(objs: &Managed) -> Bluetooth {
    let mut bt = Bluetooth::default();
    let mut paths: Vec<&OwnedObjectPath> = objs.keys().collect();
    paths.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    for path in paths {
        let ifaces = &objs[path];
        if let Some(a) = ifaces.get("org.bluez.Adapter1") {
            bt.available = true;
            bt.powered |= get::<bool>(a, "Powered").unwrap_or(false);
        }
        if let Some(d) = ifaces.get("org.bluez.Device1") {
            if get::<bool>(d, "Connected") == Some(true) {
                bt.connected.push(BtDevice {
                    name: get::<String>(d, "Alias")
                        .or_else(|| get(d, "Name"))
                        .unwrap_or_default(),
                    address: get::<String>(d, "Address").unwrap_or_default(),
                    battery: ifaces
                        .get("org.bluez.Battery1")
                        .and_then(|b| get::<u8>(b, "Percentage")),
                });
            }
        }
    }
    bt
}

pub fn read_into(conn: &Connection, bt: &mut Bluetooth) {
    *bt = Bluetooth::default();
    let reply = conn.call_method(
        Some("org.bluez"),
        "/",
        Some("org.freedesktop.DBus.ObjectManager"),
        "GetManagedObjects",
        &(),
    );
    if let Ok(r) = reply {
        if let Ok(objs) = r.body().deserialize::<Managed>() {
            *bt = from_managed(&objs);
        }
    }
}
