//! NetworkManager: the primary connection (type, name, SSID, signal, interface, IPv4).
//!
//! Re-read on any NetworkManager signal (state changes, access point strength). Without
//! NetworkManager everything stays `None`/`Unknown`.

use crate::props;
use serde::{Deserialize, Serialize};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetState {
    #[default]
    Unknown,
    Disconnected,
    Connecting,
    /// Connected without confirmed internet access.
    Local,
    Connected,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetKind {
    #[default]
    None,
    Wifi,
    Ethernet,
    Vpn,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Network {
    pub state: NetState,
    pub kind: NetKind,
    /// The active connection's name (the SSID for Wi-Fi, "Wired connection 1", a VPN's name).
    pub name: String,
    pub ssid: Option<String>,
    /// Wi-Fi signal, 0..=100.
    pub signal_pct: Option<u8>,
    pub iface: Option<String>,
    pub ipv4: Option<String>,
}

pub fn parse_state(n: u32) -> NetState {
    match n {
        20 | 10 => NetState::Disconnected,
        30 | 40 => NetState::Connecting,
        50 | 60 => NetState::Local,
        70 => NetState::Connected,
        _ => NetState::Unknown,
    }
}

pub fn parse_kind(t: &str) -> NetKind {
    match t {
        "802-11-wireless" => NetKind::Wifi,
        "802-3-ethernet" => NetKind::Ethernet,
        "vpn" | "wireguard" => NetKind::Vpn,
        "" => NetKind::None,
        _ => NetKind::Other,
    }
}

fn path_of(p: &props::Props, k: &str) -> Option<String> {
    let v: OwnedObjectPath = OwnedObjectPath::try_from(p.get(k)?.try_clone().ok()?).ok()?;
    let s = v.as_str();
    (s != "/").then(|| s.to_string())
}

fn paths_of(p: &props::Props, k: &str) -> Vec<String> {
    p.get(k)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<OwnedObjectPath>::try_from(v).ok())
        .map(|v| v.iter().map(|o| o.as_str().to_string()).collect())
        .unwrap_or_default()
}

/// Re-read the whole picture.
pub fn read_into(conn: &Connection, net: &mut Network) {
    *net = Network::default();
    let Some(m) = props::get_all(conn, NM, NM_PATH, NM) else {
        return;
    };
    net.state = props::uint(&m, "State")
        .map(parse_state)
        .unwrap_or_default();
    let Some(active) = path_of(&m, "PrimaryConnection") else {
        return;
    };
    let Some(a) = props::get_all(
        conn,
        NM,
        &active,
        "org.freedesktop.NetworkManager.Connection.Active",
    ) else {
        return;
    };
    net.kind = parse_kind(&props::string(&a, "Type").unwrap_or_default());
    if props::boolean(&a, "Vpn") == Some(true) {
        net.kind = NetKind::Vpn;
    }
    net.name = props::string(&a, "Id").unwrap_or_default();
    if let Some(ip) = path_of(&a, "Ip4Config") {
        net.ipv4 = props::get_all(conn, NM, &ip, "org.freedesktop.NetworkManager.IP4Config")
            .and_then(|c| c.get("AddressData").and_then(|v| v.try_clone().ok()))
            .and_then(|v| Vec::<std::collections::HashMap<String, OwnedValue>>::try_from(v).ok())
            .and_then(|list| list.into_iter().next())
            .and_then(|d| d.get("address").and_then(|v| v.try_clone().ok()))
            .and_then(|v| String::try_from(v).ok());
    }
    let Some(dev) = paths_of(&a, "Devices").into_iter().next() else {
        return;
    };
    if let Some(d) = props::get_all(conn, NM, &dev, "org.freedesktop.NetworkManager.Device") {
        net.iface = props::string(&d, "Interface");
    }
    if net.kind == NetKind::Wifi {
        let ap = props::get_all(
            conn,
            NM,
            &dev,
            "org.freedesktop.NetworkManager.Device.Wireless",
        )
        .and_then(|w| path_of(&w, "ActiveAccessPoint"));
        if let Some(ap) = ap.and_then(|p| {
            props::get_all(conn, NM, &p, "org.freedesktop.NetworkManager.AccessPoint")
        }) {
            net.signal_pct = ap
                .get("Strength")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| u8::try_from(v).ok())
                .map(|s| s.min(100));
            net.ssid = ap
                .get("Ssid")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<u8>::try_from(v).ok())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .filter(|s| !s.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nm_enums() {
        assert_eq!(parse_state(70), NetState::Connected);
        assert_eq!(parse_state(20), NetState::Disconnected);
        assert_eq!(parse_state(40), NetState::Connecting);
        assert_eq!(parse_state(60), NetState::Local);
        assert_eq!(parse_state(0), NetState::Unknown);
        assert_eq!(parse_kind("802-11-wireless"), NetKind::Wifi);
        assert_eq!(parse_kind("802-3-ethernet"), NetKind::Ethernet);
        assert_eq!(parse_kind("wireguard"), NetKind::Vpn);
        assert_eq!(parse_kind("bridge"), NetKind::Other);
        assert_eq!(parse_kind(""), NetKind::None);
    }
}
