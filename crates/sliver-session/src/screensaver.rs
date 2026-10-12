//! Screen-saver / lock state from the desktop's own `ActiveChanged` signal.
//!
//! GNOME (`org.gnome.ScreenSaver`), KDE and others (`org.freedesktop.ScreenSaver`) announce when
//! the lock screen comes up and goes away. Unlike logind's `LockedHint` (which depends on the
//! locker calling `unlock-session`, see integration.md §3) this is the shell's own truth, so
//! when it exists it wins. Nothing here polls: one signal subscription and a thread that sleeps
//! in it.
//!
//! State reads never start a service (`NoAutoStart`).

use crate::{Handle, props};
use zbus::blocking::Connection;

/// `(destination, path, interface)` of every service that has `GetActive` / `ActiveChanged`.
pub const SERVICES: [(&str, &str, &str); 4] = [
    (
        "org.gnome.ScreenSaver",
        "/org/gnome/ScreenSaver",
        "org.gnome.ScreenSaver",
    ),
    (
        "org.freedesktop.ScreenSaver",
        "/org/freedesktop/ScreenSaver",
        "org.freedesktop.ScreenSaver",
    ),
    (
        "org.freedesktop.ScreenSaver",
        "/ScreenSaver",
        "org.freedesktop.ScreenSaver",
    ),
    (
        "org.kde.screensaver",
        "/ScreenSaver",
        "org.freedesktop.ScreenSaver",
    ),
];

pub const RULES: [&str; 2] = [
    "type='signal',interface='org.gnome.ScreenSaver',member='ActiveChanged'",
    "type='signal',interface='org.freedesktop.ScreenSaver',member='ActiveChanged'",
];

/// `GetActive()` on the first service that answers. `None` = no such service (or it is only an
/// idle-inhibit provider, like hypridle's, which has no `GetActive`).
pub fn read_active(conn: &Connection) -> Option<bool> {
    SERVICES.iter().find_map(|(dest, path, iface)| {
        props::call_no_autostart::<_, bool>(conn, dest, path, iface, "GetActive", &())
    })
}

impl Handle {
    /// Track the screen-saver's active state in `SessionState::screensaver`. `conn` is a
    /// session-bus connection dedicated to this.
    pub fn start_screensaver(&self, conn: Connection) {
        let signals = crate::subscribe(&conn, &RULES);
        let hub = self.hub.clone();
        let first = read_active(&conn);
        hub.update(|s| s.screensaver = first);
        std::thread::Builder::new()
            .name("sliver-session-screensaver".into())
            .spawn(move || {
                for msg in signals {
                    let h = msg.header();
                    if h.member().map(|m| m.as_str()) != Some("ActiveChanged") {
                        continue;
                    }
                    let Ok(active) = msg.body().deserialize::<bool>() else {
                        continue;
                    };
                    if !hub.update(|s| s.screensaver = Some(active)) {
                        return;
                    }
                }
            })
            .expect("spawn thread");
    }
}
