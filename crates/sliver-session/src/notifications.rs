//! Notifications: how many are on screen, and whether Do Not Disturb is on.
//!
//! The spec (`org.freedesktop.Notifications`) has neither a list nor DND, and we must not
//! become the notification server. So the count comes from **bus monitoring**
//! (`BecomeMonitor`, allowed for a user on their own session bus): `Notify` calls, the server's
//! replies (which carry the id) and `NotificationClosed` signals feed a [`Counter`]. Only the
//! `replaces_id` argument of `Notify` is read; summaries and bodies are dropped unread.
//!
//! "Count" means notifications the server currently has open, not unread history.
//!
//! DND has no standard, so there is a small adapter list ([`read_dnd`]): dunst
//! (`org.dunstproject.cmd0` `paused`) and KDE (`Inhibited` on the server). Others (mako, swaync,
//! GNOME, this machine's quickshell) report `None`: the agent then offers its own DND that only
//! hides Touch Bar notification chips.

use crate::{Handle, props};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use zbus::blocking::Connection;
use zbus::message::Type;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notifications {
    /// Notifications currently open in the server.
    pub count: u32,
    /// `None` = unknown (no supported daemon).
    pub dnd: Option<bool>,
}

/// Pure bookkeeping, separate from D-Bus so it is exhaustively testable.
#[derive(Debug, Default)]
pub struct Counter {
    /// `Notify` calls awaiting their reply, keyed by (caller unique name, call serial).
    pending: HashMap<(String, u32), u32>,
    open: HashSet<u32>,
}

impl Counter {
    pub fn count(&self) -> u32 {
        self.open.len() as u32
    }

    /// A client called `Notify(.., replaces_id, ..)`.
    pub fn notify_called(&mut self, caller: &str, serial: u32, replaces_id: u32) {
        // Bound memory if replies never arrive (server gone mid-call).
        if self.pending.len() > 256 {
            self.pending.clear();
        }
        self.pending
            .insert((caller.to_string(), serial), replaces_id);
    }

    /// The server replied to a call: `id` is the notification id. Ignores unrelated replies.
    pub fn notify_returned(&mut self, caller: &str, reply_serial: u32, id: u32) {
        if let Some(replaces) = self.pending.remove(&(caller.to_string(), reply_serial)) {
            if replaces != 0 && replaces != id {
                self.open.remove(&replaces);
            }
            self.open.insert(id);
        }
    }

    /// `NotificationClosed(id, reason)`.
    pub fn closed(&mut self, id: u32) {
        self.open.remove(&id);
    }

    /// The server went away or restarted: its notifications are gone.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.open.clear();
    }
}

const SERVER: &str = "org.freedesktop.Notifications";

/// Rules for the monitor connection. Method returns are limited to the server's own, so the
/// monitor never sees other services' replies.
pub const MONITOR_RULES: [&str; 6] = [
    "type='method_call',interface='org.freedesktop.Notifications',member='Notify'",
    "type='method_return',sender='org.freedesktop.Notifications'",
    "type='signal',interface='org.freedesktop.Notifications',member='NotificationClosed'",
    "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.freedesktop.Notifications'",
    // DND flips: dunst's cmd0 and KDE's server property
    "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/dunstproject/cmd0'",
    "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/freedesktop/Notifications'",
];

/// Read DND from the first supported daemon on the bus.
pub fn read_dnd(conn: &Connection) -> Option<bool> {
    let dunst = props::get_all(
        conn,
        "org.freedesktop.Notifications",
        "/org/dunstproject/cmd0",
        "org.dunstproject.cmd0",
    )
    .and_then(|p| props::boolean(&p, "paused"));
    dunst.or_else(|| {
        props::get_all(conn, SERVER, "/org/freedesktop/Notifications", SERVER)
            .and_then(|p| props::boolean(&p, "Inhibited"))
    })
}

impl Handle {
    /// Start tracking notifications.
    ///
    /// `monitor` must be a **fresh** connection to the session bus: it is turned into a
    /// monitor (receive-only) and consumed. `query` is an ordinary session connection used to
    /// read DND.
    pub fn start_notifications(&self, monitor: Connection, query: Connection) -> zbus::Result<()> {
        let rules: Vec<zbus::MatchRule<'_>> = MONITOR_RULES
            .iter()
            .filter_map(|r| zbus::MatchRule::try_from(*r).ok())
            .collect();
        zbus::blocking::fdo::MonitoringProxy::new(&monitor)?.become_monitor(&rules, 0)?;
        let signals = zbus::blocking::MessageIterator::from(monitor);
        let hub = self.hub.clone();
        let dnd = read_dnd(&query);
        hub.update(|s| s.notifications.dnd = dnd);
        std::thread::Builder::new()
            .name("sliver-session-notifications".into())
            .spawn(move || {
                let mut counter = Counter::default();
                for msg in signals.filter_map(Result::ok) {
                    let h = msg.header();
                    let sender = h
                        .sender()
                        .map(|s| s.as_str().to_owned())
                        .unwrap_or_default();
                    let member = h.member().map(|m| m.as_str().to_owned());
                    let mut dnd_changed = false;
                    match msg.message_type() {
                        Type::MethodCall if member.as_deref() == Some("Notify") => {
                            let replaces = notify_replaces_id(&msg);
                            counter.notify_called(
                                &sender,
                                h.primary().serial_num().get(),
                                replaces,
                            );
                        }
                        Type::MethodReturn => {
                            let to = h
                                .destination()
                                .map(|d| d.as_str().to_owned())
                                .unwrap_or_default();
                            if let (Some(serial), Ok(id)) =
                                (h.reply_serial(), msg.body().deserialize::<u32>())
                            {
                                counter.notify_returned(&to, serial.get(), id);
                            }
                        }
                        Type::Signal => match member.as_deref() {
                            Some("NotificationClosed") => {
                                if let Ok((id, _reason)) = msg.body().deserialize::<(u32, u32)>() {
                                    counter.closed(id);
                                }
                            }
                            Some("NameOwnerChanged") => {
                                counter.reset();
                                dnd_changed = true;
                            }
                            Some("PropertiesChanged") => {
                                let path = h.path().map(|p| p.as_str()).unwrap_or_default();
                                dnd_changed = path == "/org/dunstproject/cmd0"
                                    || path == "/org/freedesktop/Notifications";
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                    let count = counter.count();
                    let dnd = dnd_changed.then(|| read_dnd(&query));
                    let alive = hub.update(|s| {
                        s.notifications.count = count;
                        if let Some(d) = dnd {
                            s.notifications.dnd = d;
                        }
                    });
                    if !alive {
                        return;
                    }
                }
            })
            .expect("spawn thread");
        Ok(())
    }
}

/// `replaces_id` is the second argument of `Notify`; the rest is dropped unread.
fn notify_replaces_id(msg: &zbus::Message) -> u32 {
    type Args = (
        String,
        u32,
        String,
        String,
        String,
        Vec<String>,
        HashMap<String, zbus::zvariant::OwnedValue>,
        i32,
    );
    msg.body().deserialize::<Args>().map(|a| a.1).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_new_replaced_and_closed() {
        let mut c = Counter::default();
        c.notify_called(":1.5", 10, 0);
        assert_eq!(c.count(), 0, "not counted until the server assigns an id");
        c.notify_returned(":1.5", 10, 1);
        assert_eq!(c.count(), 1);
        c.notify_called(":1.6", 3, 0);
        c.notify_returned(":1.6", 3, 2);
        assert_eq!(c.count(), 2);
        // replacing id 1 in place (same id back) doesn't add one
        c.notify_called(":1.5", 11, 1);
        c.notify_returned(":1.5", 11, 1);
        assert_eq!(c.count(), 2);
        // a server that issues a fresh id on replace: old one goes away
        c.notify_called(":1.5", 12, 1);
        c.notify_returned(":1.5", 12, 7);
        assert_eq!(c.count(), 2);
        c.closed(7);
        c.closed(7);
        assert_eq!(c.count(), 1);
        // unrelated method returns are ignored
        c.notify_returned(":1.9", 99, 55);
        assert_eq!(c.count(), 1);
        c.reset();
        assert_eq!(c.count(), 0);
    }
}
