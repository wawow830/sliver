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
use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::message::Type;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notifications {
    /// Notifications currently open in the server.
    pub count: u32,
    /// `None` = unknown (no supported daemon).
    pub dnd: Option<bool>,
    /// The most recent open notification: its app and summary only, never the body.
    pub latest: Option<Latest>,
}

/// What the bar may show of a notification. The body is never read, so it can't leak onto a bar
/// that stays visible while the screen is locked or shared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Latest {
    pub id: u32,
    pub app: String,
    pub summary: String,
}

/// Summaries are cut to this many characters: a long one is no use on a 60 px bar.
const MAX_SUMMARY: usize = 120;

/// Pure bookkeeping, separate from D-Bus so it is exhaustively testable.
#[derive(Debug, Default)]
pub struct Counter {
    /// `Notify` calls awaiting their reply, keyed by (caller unique name, call serial).
    pending: HashMap<(String, u32), (u32, String, String)>,
    open: HashMap<u32, (String, String)>,
    /// Open ids, oldest first.
    order: Vec<u32>,
}

impl Counter {
    pub fn count(&self) -> u32 {
        self.open.len() as u32
    }

    /// The newest open notification.
    pub fn latest(&self) -> Option<Latest> {
        let id = *self.order.last()?;
        let (app, summary) = self.open.get(&id)?;
        Some(Latest {
            id,
            app: app.clone(),
            summary: summary.clone(),
        })
    }

    /// A client called `Notify(app, replaces_id, .., summary, ..)`.
    pub fn notify_called(
        &mut self,
        caller: &str,
        serial: u32,
        replaces_id: u32,
        app: &str,
        summary: &str,
    ) {
        // Bound memory if replies never arrive (server gone mid-call).
        if self.pending.len() > 256 {
            self.pending.clear();
        }
        let summary: String = summary.chars().take(MAX_SUMMARY).collect();
        self.pending.insert(
            (caller.to_string(), serial),
            (replaces_id, app.to_string(), summary),
        );
    }

    /// The server replied to a call: `id` is the notification id. Ignores unrelated replies.
    pub fn notify_returned(&mut self, caller: &str, reply_serial: u32, id: u32) {
        if let Some((replaces, app, summary)) =
            self.pending.remove(&(caller.to_string(), reply_serial))
        {
            if replaces != 0 && replaces != id {
                self.closed(replaces);
            }
            self.order.retain(|x| *x != id);
            self.order.push(id);
            self.open.insert(id, (app, summary));
        }
    }

    /// `NotificationClosed(id, reason)`.
    pub fn closed(&mut self, id: u32) {
        self.open.remove(&id);
        self.order.retain(|x| *x != id);
    }

    /// The server went away or restarted: its notifications are gone.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.open.clear();
        self.order.clear();
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

/// Daemons whose Do Not Disturb switch is their own command-line tool (no D-Bus property).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cli {
    Mako,
    Swaync,
}

impl Cli {
    /// From the `GetServerInformation` name.
    pub fn from_server(name: &str) -> Option<Cli> {
        match name {
            "mako" => Some(Cli::Mako),
            "SwayNotificationCenter" | "swaync" => Some(Cli::Swaync),
            _ => None,
        }
    }

    /// The program and arguments that switch DND.
    pub fn set_args(self, on: bool) -> (&'static str, &'static [&'static str]) {
        match (self, on) {
            (Cli::Mako, true) => ("makoctl", &["mode", "-a", "do-not-disturb"]),
            (Cli::Mako, false) => ("makoctl", &["mode", "-r", "do-not-disturb"]),
            (Cli::Swaync, true) => ("swaync-client", &["--dnd-on", "-sw"]),
            (Cli::Swaync, false) => ("swaync-client", &["--dnd-off", "-sw"]),
        }
    }

    /// The program and arguments that print the current state.
    pub fn get_args(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Cli::Mako => ("makoctl", &["mode"]),
            Cli::Swaync => ("swaync-client", &["--get-dnd"]),
        }
    }

    /// DND from the output of [`Cli::get_args`].
    pub fn parse(self, out: &str) -> Option<bool> {
        match self {
            Cli::Mako => Some(out.lines().any(|l| l.trim() == "do-not-disturb")),
            Cli::Swaync => match out.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
        }
    }
}

/// `GetServerInformation().name`, without starting a server.
fn server_name(conn: &Connection) -> Option<String> {
    props::call_no_autostart::<_, (String, String, String, String)>(
        conn,
        SERVER,
        "/org/freedesktop/Notifications",
        SERVER,
        "GetServerInformation",
        &(),
    )
    .map(|t| t.0)
}

fn cli_read(cli: Cli) -> Option<bool> {
    let (prog, args) = cli.get_args();
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args);
    let out = crate::audio::output_with_timeout(cmd, std::time::Duration::from_secs(2))?;
    out.status
        .success()
        .then(|| cli.parse(&String::from_utf8_lossy(&out.stdout)))?
}

/// Read DND from the first supported daemon on the bus: dunst, KDE (`Inhibited`), then mako and
/// swaync through their tools (so only when one of those is the running server).
pub fn read_dnd(conn: &Connection) -> Option<bool> {
    let dunst = props::get_all(
        conn,
        "org.freedesktop.Notifications",
        "/org/dunstproject/cmd0",
        "org.dunstproject.cmd0",
    )
    .and_then(|p| props::boolean(&p, "paused"));
    dunst
        .or_else(|| {
            props::get_all(conn, SERVER, "/org/freedesktop/Notifications", SERVER)
                .and_then(|p| props::boolean(&p, "Inhibited"))
        })
        .or_else(|| cli_read(Cli::from_server(&server_name(conn)?)?))
}

fn set_dnd_cli(conn: &Connection, on: bool) -> Option<Result<(), String>> {
    let cli = Cli::from_server(&server_name(conn)?)?;
    let (prog, args) = cli.set_args(on);
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args);
    Some(
        match crate::audio::output_with_timeout(cmd, std::time::Duration::from_secs(3)) {
            Some(o) if o.status.success() => Ok(()),
            _ => Err(format!("{prog} failed or is not installed")),
        },
    )
}

/// Turn Do Not Disturb on or off. dunst has a property; mako and swaync have a tool; KDE is
/// handled by [`Handle::set_dnd`] (a held `Inhibit`).
pub fn set_dnd(conn: &Connection, on: bool) -> Result<(), String> {
    let dunst = props::set_no_autostart(
        conn,
        "org.freedesktop.Notifications",
        "/org/dunstproject/cmd0",
        "org.dunstproject.cmd0",
        "paused",
        zbus::zvariant::Value::from(on),
    );
    match dunst {
        Ok(()) => Ok(()),
        Err(e) => set_dnd_cli(conn, on).unwrap_or_else(|| {
            Err(format!(
                "no notification daemon with a Do Not Disturb switch (dunst, mako, swaync, KDE supported): {e}"
            ))
        }),
    }
}

/// Dismiss notification `id` (`CloseNotification`; the server answers with `NotificationClosed`).
/// Never starts a notification daemon.
pub fn close(conn: &Connection, id: u32) -> Result<(), String> {
    props::call_no_autostart::<_, ()>(
        conn,
        SERVER,
        "/org/freedesktop/Notifications",
        SERVER,
        "CloseNotification",
        &(id,),
    )
    .ok_or_else(|| "no notification server to close it".to_string())
}

impl Handle {
    /// Switch Do Not Disturb and record the result in the session state at once (mako and
    /// swaync announce nothing on the bus). KDE has no setter, only an `Inhibit` that lasts as
    /// long as our connection, so that is held here.
    pub fn set_dnd(&self, conn: &Connection, on: bool) -> Result<(), String> {
        let kde = props::get_all(conn, SERVER, "/org/freedesktop/Notifications", SERVER)
            .and_then(|p| props::boolean(&p, "Inhibited"))
            .is_some();
        let r = if kde {
            let mut held = self.dnd_cookie.lock().unwrap_or_else(|e| e.into_inner());
            if on && held.is_none() {
                let hints: HashMap<String, zbus::zvariant::Value<'_>> = HashMap::new();
                *held = props::call_no_autostart::<_, u32>(
                    conn,
                    SERVER,
                    "/org/freedesktop/Notifications",
                    SERVER,
                    "Inhibit",
                    &("Sliver", "Do Not Disturb toggle", hints),
                );
                if held.is_none() {
                    return Err("the notification server refused Inhibit".into());
                }
            } else if let Some(c) = (!on).then(|| held.take()).flatten() {
                props::call_no_autostart::<_, ()>(
                    conn,
                    SERVER,
                    "/org/freedesktop/Notifications",
                    SERVER,
                    "UnInhibit",
                    &(c,),
                );
            }
            Ok(())
        } else {
            set_dnd(conn, on)
        };
        if r.is_ok() {
            self.hub.update(|s| s.notifications.dnd = Some(on));
        }
        r
    }

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
                            let (replaces, app, summary) = notify_args(&msg);
                            counter.notify_called(
                                &sender,
                                h.primary().serial_num().get(),
                                replaces,
                                &app,
                                &summary,
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
                    let latest = counter.latest();
                    let alive = hub.update(|s| {
                        s.notifications.count = count;
                        s.notifications.latest = latest;
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

/// `(replaces_id, app_name, summary)` of a `Notify` call. The body, actions and hints are
/// deserialized (the wire format needs them) and dropped at once.
fn notify_args(msg: &zbus::Message) -> (u32, String, String) {
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
    msg.body()
        .deserialize::<Args>()
        .map(|a| (a.1, a.0, a.3))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnd_cli_adapters() {
        assert_eq!(Cli::from_server("mako"), Some(Cli::Mako));
        assert_eq!(
            Cli::from_server("SwayNotificationCenter"),
            Some(Cli::Swaync)
        );
        assert_eq!(Cli::from_server("dunst"), None);
        assert_eq!(Cli::Mako.parse("default\ndo-not-disturb\n"), Some(true));
        assert_eq!(Cli::Mako.parse("default\n"), Some(false));
        assert_eq!(Cli::Swaync.parse("true\n"), Some(true));
        assert_eq!(Cli::Swaync.parse("false"), Some(false));
        assert_eq!(Cli::Swaync.parse("???"), None);
        assert_eq!(Cli::Mako.set_args(true).1, ["mode", "-a", "do-not-disturb"]);
        assert_eq!(
            Cli::Mako.set_args(false).1,
            ["mode", "-r", "do-not-disturb"]
        );
        assert_eq!(Cli::Swaync.set_args(false).0, "swaync-client");
    }

    #[test]
    fn counts_new_replaced_and_closed() {
        let mut c = Counter::default();
        c.notify_called(":1.5", 10, 0, "app", "sum");
        assert_eq!(c.count(), 0, "not counted until the server assigns an id");
        c.notify_returned(":1.5", 10, 1);
        assert_eq!(c.count(), 1);
        c.notify_called(":1.6", 3, 0, "chat", "New message");
        c.notify_returned(":1.6", 3, 2);
        assert_eq!(c.count(), 2);
        let l = c.latest().unwrap();
        assert_eq!(
            (l.id, l.app.as_str(), l.summary.as_str()),
            (2, "chat", "New message")
        );
        // replacing id 1 in place (same id back) doesn't add one
        c.notify_called(":1.5", 11, 1, "app", "sum");
        c.notify_returned(":1.5", 11, 1);
        assert_eq!(c.count(), 2);
        // a server that issues a fresh id on replace: old one goes away
        c.notify_called(":1.5", 12, 1, "app", "sum");
        c.notify_returned(":1.5", 12, 7);
        assert_eq!(c.count(), 2);
        c.closed(7);
        c.closed(7);
        assert_eq!(
            c.latest().unwrap().id,
            2,
            "back to the newest one still open"
        );
        assert_eq!(c.count(), 1);
        // unrelated method returns are ignored
        c.notify_returned(":1.9", 99, 55);
        assert_eq!(c.count(), 1);
        c.reset();
        assert_eq!(c.count(), 0);
        assert!(c.latest().is_none());
        // long summaries are cut
        c.notify_called(":1.5", 1, 0, "a", &"x".repeat(500));
        c.notify_returned(":1.5", 1, 9);
        assert_eq!(c.latest().unwrap().summary.chars().count(), MAX_SUMMARY);
    }
}
