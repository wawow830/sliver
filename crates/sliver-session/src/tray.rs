//! StatusNotifierItem host: the system tray, as data.
//!
//! We register as an additional **host** with whoever owns `org.kde.StatusNotifierWatcher`
//! (KDE, quickshell, waybar, ...), follow its item list, and read each item's properties.
//! Event driven: watcher register/unregister signals and the items' own `New*` signals; no
//! polling. Activation goes straight to the item (`Activate`, `SecondaryActivate`,
//! `ContextMenu`, `Scroll`). The dbusmenu behind an item's `Menu` path is in [`crate::menu`].
//!
//! If there is no watcher on the bus the list stays empty until one appears (a host-owned
//! fallback watcher is not implemented yet).

use crate::{Handle, props};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zbus::blocking::Connection;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrayStatus {
    Passive,
    #[default]
    Active,
    NeedsAttention,
}

impl TrayStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "Passive" => Self::Passive,
            "NeedsAttention" => Self::NeedsAttention,
            _ => Self::Active,
        }
    }
}

/// A square-ish RGBA8 image (straight alpha, row-major).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayItem {
    /// The registered string, `"<bus name>/<object path>"`; the stable identity for controls.
    pub key: String,
    /// The app's own id (`Id` property); stable across restarts, unlike `key`.
    pub id: String,
    pub title: String,
    pub category: String,
    pub status: TrayStatus,
    /// Icon theme name; resolve through the XDG icon lookup (honour `icon_theme_path`).
    pub icon_name: String,
    pub icon_theme_path: String,
    pub attention_icon_name: String,
    /// Embedded icon, used when `icon_name` is empty or not found.
    pub icon: Option<Pixmap>,
    pub attention_icon: Option<Pixmap>,
    pub tooltip: String,
    /// Left click should open the menu rather than `Activate` (`ItemIsMenu`).
    pub item_is_menu: bool,
    /// dbusmenu object path, if the item has a menu.
    pub menu: Option<String>,
}

/// Split a registered item string into (bus name, object path). A bare name means the default
/// path `/StatusNotifierItem`.
pub fn parse_item_address(key: &str) -> (String, String) {
    match key.find('/') {
        Some(0) => (String::new(), key.to_string()),
        Some(i) => (key[..i].to_string(), key[i..].to_string()),
        None => (key.to_string(), "/StatusNotifierItem".to_string()),
    }
}

/// SNI pixmaps are ARGB32 in network byte order (bytes A,R,G,B). Convert to RGBA.
pub fn argb_to_rgba(data: &[u8]) -> Vec<u8> {
    data.chunks_exact(4)
        .flat_map(|p| [p[1], p[2], p[3], p[0]])
        .collect()
}

type RawPixmap = (i32, i32, Vec<u8>);

/// Choose the icon closest to `target` px: the smallest one at least that big, else the largest.
pub fn pick_pixmap(all: &[RawPixmap], target: u32) -> Option<Pixmap> {
    let valid =
        |p: &&RawPixmap| p.0 > 0 && p.1 > 0 && p.2.len() == (p.0 as usize) * (p.1 as usize) * 4;
    let side = |p: &RawPixmap| p.0.min(p.1) as u32;
    let best = all
        .iter()
        .filter(valid)
        .filter(|p| side(p) >= target)
        .min_by_key(|p| side(p))
        .or_else(|| all.iter().filter(valid).max_by_key(|p| side(p)))?;
    Some(Pixmap {
        width: best.0 as u32,
        height: best.1 as u32,
        rgba: argb_to_rgba(&best.2),
    })
}

/// Size the Touch Bar shows tray icons at (the bar is 60 px tall; keys leave ~40).
const ICON_TARGET: u32 = 40;

fn pixmaps(p: &props::Props, k: &str) -> Vec<RawPixmap> {
    p.get(k)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<RawPixmap>::try_from(v).ok())
        .unwrap_or_default()
}

/// Build a [`TrayItem`] from `GetAll(org.kde.StatusNotifierItem)`.
pub fn parse_item(key: &str, p: &props::Props) -> TrayItem {
    let s = |k: &str| props::string(p, k).unwrap_or_default();
    let tooltip = p
        .get("ToolTip")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| <(String, Vec<RawPixmap>, String, String)>::try_from(v).ok())
        .map(|t| t.2)
        .unwrap_or_default();
    let menu = p
        .get("Menu")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| zbus::zvariant::OwnedObjectPath::try_from(v).ok())
        .map(|o| o.as_str().to_string())
        .filter(|m| m != "/");
    TrayItem {
        key: key.to_string(),
        id: s("Id"),
        title: s("Title"),
        category: s("Category"),
        status: TrayStatus::parse(&s("Status")),
        icon_name: s("IconName"),
        icon_theme_path: s("IconThemePath"),
        attention_icon_name: s("AttentionIconName"),
        icon: pick_pixmap(&pixmaps(p, "IconPixmap"), ICON_TARGET),
        attention_icon: pick_pixmap(&pixmaps(p, "AttentionIconPixmap"), ICON_TARGET),
        tooltip,
        item_is_menu: props::boolean(p, "ItemIsMenu") == Some(true),
        menu,
    }
}

fn read_item(conn: &Connection, key: &str) -> Option<TrayItem> {
    let (dest, path) = parse_item_address(key);
    let p = props::get_all(conn, &dest, &path, ITEM_IFACE)?;
    Some(parse_item(key, &p))
}

/// Bookkeeping: registered items in order, and the unique bus name that owns each (signals
/// carry unique names, registrations may carry well-known ones).
#[derive(Default)]
struct Items {
    list: Vec<TrayItem>,
    owners: HashMap<String, String>,
}

impl Items {
    fn owner_of(conn: &Connection, dest: &str) -> Option<String> {
        if dest.starts_with(':') {
            return Some(dest.to_string());
        }
        let bus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
        let name = zbus::names::BusName::try_from(dest).ok()?;
        Some(bus.get_name_owner(name).ok()?.to_string())
    }

    fn add(&mut self, conn: &Connection, key: &str) {
        let Some(item) = read_item(conn, key) else {
            return;
        };
        let (dest, _) = parse_item_address(key);
        if let Some(o) = Self::owner_of(conn, &dest) {
            self.owners.insert(key.to_string(), o);
        }
        match self.list.iter_mut().find(|i| i.key == key) {
            Some(slot) => *slot = item,
            None => self.list.push(item),
        }
    }

    fn remove(&mut self, key: &str) {
        self.list.retain(|i| i.key != key);
        self.owners.remove(key);
    }

    /// Keys whose item sits at `path` on unique name `sender`.
    fn matching(&self, sender: &str, path: &str) -> Vec<String> {
        self.list
            .iter()
            .filter(|i| {
                self.owners.get(&i.key).is_some_and(|o| o == sender)
                    && parse_item_address(&i.key).1 == path
            })
            .map(|i| i.key.clone())
            .collect()
    }

    /// Replace everything from the watcher's list (initial read, watcher restart).
    fn resync(&mut self, conn: &Connection) {
        self.list.clear();
        self.owners.clear();
        let Some(p) = props::get_all(conn, WATCHER, WATCHER_PATH, WATCHER) else {
            return;
        };
        let keys = p
            .get("RegisteredStatusNotifierItems")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<String>::try_from(v).ok())
            .unwrap_or_default();
        for k in keys {
            self.add(conn, &k);
        }
    }
}

const RULES: [&str; 3] = [
    "type='signal',sender='org.kde.StatusNotifierWatcher'",
    "type='signal',interface='org.kde.StatusNotifierItem'",
    "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.kde.StatusNotifierWatcher'",
];

fn register_host(conn: &Connection) {
    // The host name only needs to be unique and well-formed.
    let name = format!("org.kde.StatusNotifierHost-{}-1", std::process::id());
    if conn.request_name(name.as_str()).is_ok() {
        let _ = conn.call_method(
            Some(WATCHER),
            WATCHER_PATH,
            Some(WATCHER),
            "RegisterStatusNotifierHost",
            &(name.as_str(),),
        );
    }
}

impl Handle {
    /// Track the tray. `conn` is a session-bus connection dedicated to this (it owns the host
    /// name). Items appear in `SessionState.tray` in registration order.
    pub fn start_tray(&self, conn: Connection) {
        let signals = crate::subscribe(&conn, &RULES);
        register_host(&conn);
        let hub = self.hub.clone();
        let mut items = Items::default();
        items.resync(&conn);
        let first = items.list.clone();
        hub.update(|s| s.tray = first);
        std::thread::Builder::new()
            .name("sliver-session-tray".into())
            .spawn(move || {
                for msg in signals {
                    let h = msg.header();
                    let member = h
                        .member()
                        .map(|m| m.as_str().to_owned())
                        .unwrap_or_default();
                    let sender = h
                        .sender()
                        .map(|s| s.as_str().to_owned())
                        .unwrap_or_default();
                    let path = h.path().map(|p| p.as_str().to_owned()).unwrap_or_default();
                    match member.as_str() {
                        "StatusNotifierItemRegistered" => {
                            if let Ok(k) = msg.body().deserialize::<String>() {
                                items.add(&conn, &k);
                            }
                        }
                        "StatusNotifierItemUnregistered" => {
                            if let Ok(k) = msg.body().deserialize::<String>() {
                                items.remove(&k);
                            }
                        }
                        "NameOwnerChanged" => {
                            items.resync(&conn);
                            register_host(&conn);
                        }
                        m if m.starts_with("New") => {
                            for k in items.matching(&sender, &path) {
                                items.add(&conn, &k);
                            }
                        }
                        _ => continue,
                    }
                    let list = items.list.clone();
                    if !hub.update(|s| s.tray = list) {
                        return;
                    }
                }
            })
            .expect("spawn thread");
    }
}

/// How the user poked an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poke {
    /// Primary action (tap).
    Activate,
    /// Middle click.
    SecondaryActivate,
    /// The app shows its own context menu near (x, y). Prefer [`crate::menu`] for the bar.
    ContextMenu,
}

/// Send `poke` to an item. (x, y) are screen coordinates the app may use to place a window.
pub fn poke(conn: &Connection, item: &TrayItem, how: Poke, x: i32, y: i32) -> zbus::Result<()> {
    let (dest, path) = parse_item_address(&item.key);
    let method = match how {
        Poke::Activate => "Activate",
        Poke::SecondaryActivate => "SecondaryActivate",
        Poke::ContextMenu => "ContextMenu",
    };
    conn.call_method(
        Some(dest.as_str()),
        path.as_str(),
        Some(ITEM_IFACE),
        method,
        &(x, y),
    )
    .map(|_| ())
}

/// Scroll over an item (`orientation` is `"vertical"` or `"horizontal"`).
pub fn scroll(
    conn: &Connection,
    item: &TrayItem,
    delta: i32,
    orientation: &str,
) -> zbus::Result<()> {
    let (dest, path) = parse_item_address(&item.key);
    conn.call_method(
        Some(dest.as_str()),
        path.as_str(),
        Some(ITEM_IFACE),
        "Scroll",
        &(delta, orientation),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_forms() {
        assert_eq!(
            parse_item_address(":1.41/StatusNotifierItem"),
            (":1.41".into(), "/StatusNotifierItem".into())
        );
        assert_eq!(
            parse_item_address("org.example.App/org/ayatana/Item"),
            ("org.example.App".into(), "/org/ayatana/Item".into())
        );
        assert_eq!(
            parse_item_address("org.example.App"),
            ("org.example.App".into(), "/StatusNotifierItem".into())
        );
    }

    #[test]
    fn argb_becomes_rgba() {
        // one pixel: A=0x80 R=1 G=2 B=3
        assert_eq!(argb_to_rgba(&[0x80, 1, 2, 3]), vec![1, 2, 3, 0x80]);
    }

    fn px(side: i32) -> RawPixmap {
        (side, side, vec![0; (side * side * 4) as usize])
    }

    #[test]
    fn picks_closest_not_smaller() {
        let all = [px(16), px(64), px(48), px(256)];
        assert_eq!(pick_pixmap(&all, 40).unwrap().width, 48);
        assert_eq!(
            pick_pixmap(&all, 300).unwrap().width,
            256,
            "falls back to the largest"
        );
        assert_eq!(pick_pixmap(&[], 40), None);
        // malformed (length mismatch) entries are skipped
        assert_eq!(pick_pixmap(&[(4, 4, vec![0; 3])], 4), None);
    }

    #[test]
    fn status_parse() {
        assert_eq!(TrayStatus::parse("Passive"), TrayStatus::Passive);
        assert_eq!(
            TrayStatus::parse("NeedsAttention"),
            TrayStatus::NeedsAttention
        );
        assert_eq!(TrayStatus::parse("whatever"), TrayStatus::Active);
    }
}
