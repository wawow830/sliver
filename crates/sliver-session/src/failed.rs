//! Failed systemd units (the `systemd-failed-units` module, waybar's): `NFailedUnits` of the
//! system manager (system bus) and the user manager (session bus), plus the failed units' names
//! for the module's sub-bar.
//!
//! Event-driven, no polling. `NFailedUnits` is the only Manager property that emits change
//! signals, so a match on the Manager's `PropertiesChanged` wakes us only when the count
//! changes. systemd sends those signals on the API bus only while some client has called
//! `Subscribe()` (`bus_foreach_bus`), so we subscribe. systemd tracks the subscription and drops
//! it when our connection closes. Started only when a layout uses the module.

use crate::{Handle, props};
use serde::{Deserialize, Serialize};
use zbus::blocking::Connection;

const DEST: &str = "org.freedesktop.systemd1";
const PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";

pub const RULE: &str = "type='signal',sender='org.freedesktop.systemd1',\
    path='/org/freedesktop/systemd1',interface='org.freedesktop.DBus.Properties',\
    member='PropertiesChanged',arg0='org.freedesktop.systemd1.Manager'";

/// Failed units of one manager. `None` in [`FailedUnits`] = that manager isn't reachable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Manager {
    pub count: u32,
    /// The failed units' names (`foo.service`), sorted.
    pub units: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FailedUnits {
    /// Watching at all (the module is on some layout).
    pub watched: bool,
    pub system: Option<Manager>,
    pub user: Option<Manager>,
}

/// `NFailedUnits` and the failed units' names; `None` if there is no systemd on `conn`.
pub fn read(conn: &Connection) -> Option<Manager> {
    let n: zbus::zvariant::OwnedValue = props::call_no_autostart(
        conn,
        DEST,
        PATH,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(MANAGER, "NFailedUnits"),
    )?;
    let count = u32::try_from(n).ok()?;
    let mut units = if count == 0 {
        Vec::new()
    } else {
        type Unit = (
            String,
            String,
            String,
            String,
            String,
            String,
            zbus::zvariant::OwnedObjectPath,
            u32,
            String,
            zbus::zvariant::OwnedObjectPath,
        );
        props::call_no_autostart::<_, Vec<Unit>>(
            conn,
            DEST,
            PATH,
            MANAGER,
            "ListUnitsFiltered",
            &(vec!["failed"],),
        )
        .unwrap_or_default()
        .into_iter()
        .map(|u| u.0)
        .collect()
    };
    units.sort();
    Some(Manager { count, units })
}

impl Handle {
    /// Watch failed units on the system manager (`system`) and the user manager (`user`, the
    /// session bus), each a connection dedicated to this. Once per process; later calls do
    /// nothing.
    pub fn start_failed_units(&self, system: Connection, user: Connection) {
        use std::sync::atomic::Ordering;
        if self.failed_started.swap(true, Ordering::SeqCst) {
            return;
        }
        self.hub.update(|s| s.failed_units.watched = true);
        for (conn, is_user) in [(system, false), (user, true)] {
            let signals = crate::subscribe(&conn, &[RULE]);
            // Without a subscriber systemd doesn't send the signal at all (see above).
            let _: Option<()> =
                props::call_no_autostart(&conn, DEST, PATH, MANAGER, "Subscribe", &());
            let hub = self.hub.clone();
            let set = move |hub: &crate::Hub, m: Option<Manager>| {
                hub.update(|s| {
                    if is_user {
                        s.failed_units.user = m;
                    } else {
                        s.failed_units.system = m;
                    }
                })
            };
            set(&hub, read(&conn));
            std::thread::Builder::new()
                .name(format!(
                    "sliver-session-failed-{}",
                    if is_user { "user" } else { "system" }
                ))
                .spawn(move || {
                    for _ in signals {
                        if !set(&hub, read(&conn)) {
                            return;
                        }
                    }
                })
                .expect("spawn thread");
        }
    }
}
