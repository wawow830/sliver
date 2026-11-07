//! Live agent status for the Status page: the agent's Control1/TouchBar1 properties
//! on the session bus, read on a background thread so the window never blocks on D-Bus.

use std::sync::mpsc;
use std::time::Duration;

use sliver_ipc::dbus::{BUS_NAME, IFACE_CONTROL, IFACE_TOUCHBAR, ROOT_PATH};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Clone, PartialEq)]
pub enum Agent {
    Running {
        /// Connected to sliverd and driving the bar.
        healthy: bool,
        /// `Control1.Errors` (API 3): config, stylesheet and module problems, one per line, file
        /// and line first when known. Empty from an older agent.
        errors: Vec<String>,
        layer: String,
        display: String,
        version: String,
    },
    /// Not on the bus (not started, crashed, or no session bus).
    Absent(String),
}

fn get(conn: &Connection, iface: &str, name: &str) -> Result<OwnedValue, String> {
    let p = zbus::blocking::fdo::PropertiesProxy::builder(conn)
        .destination(BUS_NAME)
        .and_then(|b| b.path(ROOT_PATH))
        .and_then(|b| b.build())
        .map_err(|e| e.to_string())?;
    p.get(
        iface
            .try_into()
            .map_err(|e: zbus::names::Error| e.to_string())?,
        name,
    )
    .map_err(|e| e.to_string())
}

fn query() -> Agent {
    let conn = match Connection::session() {
        Ok(c) => c,
        Err(e) => return Agent::Absent(format!("no session bus: {e}")),
    };
    let s = |iface, name| {
        get(&conn, iface, name)
            .ok()
            .and_then(|v| String::try_from(v).ok())
    };
    let healthy = match get(&conn, IFACE_CONTROL, "Healthy") {
        Ok(v) => bool::try_from(v).unwrap_or(false),
        Err(e) => {
            let why = if e.contains("ServiceUnknown") || e.contains("NameHasNoOwner") {
                "not running".to_owned()
            } else {
                e
            };
            return Agent::Absent(why);
        }
    };
    Agent::Running {
        healthy,
        errors: get(&conn, IFACE_CONTROL, "Errors")
            .ok()
            .and_then(|v| Vec::<String>::try_from(v).ok())
            .unwrap_or_default(),
        layer: s(IFACE_CONTROL, "ActiveLayer").unwrap_or_default(),
        display: s(IFACE_TOUCHBAR, "DisplayState").unwrap_or_default(),
        // A number (API version) or a string.
        version: get(&conn, IFACE_TOUCHBAR, "Version")
            .ok()
            .and_then(|v| {
                String::try_from(v.try_clone().ok()?).ok().or_else(|| {
                    u32::try_from(v)
                        .ok()
                        .filter(|n| *n > 0)
                        .map(|n| format!("API {n}"))
                })
            })
            .unwrap_or_default(),
    }
}

/// Runs `work` off the main loop and gives its result to `done` on it; with no answer within
/// 3 s (a stuck bus), `done` gets `late()` instead.
fn in_background<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    late: impl FnOnce() -> T + 'static,
    done: impl FnOnce(T) + 'static,
) {
    use gtk::glib;
    glib::MainContext::default().spawn_local(async move {
        let answer = gtk::gio::spawn_blocking(work);
        match glib::future_with_timeout(Duration::from_secs(3), answer).await {
            Ok(Ok(t)) => done(t),
            _ => done(late()),
        }
    });
}

const LATE: &str = "no answer within 3 s";

/// Queries in the background and calls `done` on the GTK main loop (within ~3 s).
pub fn agent_status(done: impl FnOnce(Agent) + 'static) {
    in_background(query, || Agent::Absent(LATE.into()), done);
}

/// The session-bus connection Mirror holds for the window's lifetime: `HoldLayout` lasts as long
/// as the connection that asked, so a crashed or killed editor never pins the bar.
fn mirror_conn() -> Option<Connection> {
    static CONN: std::sync::OnceLock<Option<Connection>> = std::sync::OnceLock::new();
    CONN.get_or_init(|| Connection::session().ok()).clone()
}

fn hold(conn: &Connection, layout: &str, select_only: bool) {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let options: HashMap<&str, Value> = HashMap::from([("select-only", Value::from(select_only))]);
    let with = conn.call_method(
        Some(BUS_NAME),
        ROOT_PATH,
        Some(IFACE_CONTROL),
        "HoldLayoutWith",
        &(layout, options),
    );
    // An agent older than API 2 has only HoldLayout (touches then always fire).
    if with.is_err() {
        let _ = conn.call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_CONTROL),
            "HoldLayout",
            &(layout,),
        );
    }
}

/// One Mirror command: hold `layout` (`""`: release), and say when it's sent.
struct Hold {
    layout: String,
    select_only: bool,
    sent: Option<mpsc::Sender<()>>,
}

/// Mirror's commands go out in order, from one thread, and a newer one replaces any still
/// waiting: each says what the bar should show, so only the last one asked for matters.
fn mirror() -> &'static mpsc::Sender<Hold> {
    static TX: std::sync::OnceLock<mpsc::Sender<Hold>> = std::sync::OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Hold>();
        std::thread::spawn(move || {
            while let Ok(h) = rx.recv() {
                let (h, sent) = latest(h, &rx);
                if let Some(c) = mirror_conn() {
                    hold(&c, &h.layout, h.select_only);
                    listen_held(&c);
                }
                for s in sent {
                    let _ = s.send(());
                }
            }
        });
        tx
    })
}

/// `h`, or the newest command queued behind it, and everyone waiting for any of them.
fn latest(mut h: Hold, rx: &mpsc::Receiver<Hold>) -> (Hold, Vec<mpsc::Sender<()>>) {
    let mut sent: Vec<_> = h.sent.take().into_iter().collect();
    while let Ok(mut newer) = rx.try_recv() {
        sent.extend(newer.sent.take());
        h = newer;
    }
    (h, sent)
}

/// Mirror to Touch Bar: show `layout` on the real bar while we hold it
/// (`Control1.HoldLayoutWith`); `select_only` makes touches only select. `""` releases it back to
/// the rules. Off the main thread; an absent agent is fine. The first hold also starts listening
/// for touches on the held layout (`HeldPressed`, sent only to the holder).
pub fn set_layout(layout: &str, select_only: bool) {
    let _ = mirror().send(Hold {
        layout: layout.to_owned(),
        select_only,
        sent: None,
    });
}

/// [`set_layout`], waiting (up to 2 s) until it's sent: for the window closing, after whatever
/// is queued (process exit would also drop the hold).
pub fn set_layout_now(layout: &str) {
    let (tx, rx) = mpsc::channel();
    let hold = Hold {
        layout: layout.to_owned(),
        select_only: false,
        sent: Some(tx),
    };
    if mirror().send(hold).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(2));
    }
}

/// Once per process: a thread delivering `HeldPressed(module, zone, index)` to
/// `crate::held_pressed` on the main loop.
fn listen_held(conn: &Connection) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    let conn = conn.clone();
    STARTED.call_once(move || {
        std::thread::spawn(move || {
            let Ok(proxy) = zbus::blocking::Proxy::new(&conn, BUS_NAME, ROOT_PATH, IFACE_CONTROL)
            else {
                return;
            };
            let Ok(signals) = proxy.receive_signal("HeldPressed") else {
                return;
            };
            for msg in signals {
                if let Ok((module, zone, index)) = msg.body().deserialize::<(String, String, u32)>()
                {
                    gtk::glib::MainContext::default()
                        .invoke(move || crate::held_pressed(&module, &zone, index));
                }
            }
        });
    });
}

/// What the base layer shows and why (`Control1.ActiveLayout`, `ActiveLayoutSource`,
/// `ActiveRule`, `ActiveRuleName`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Active {
    pub layout: String,
    /// `rule`, `default` or `forced`; empty before the agent's first frame.
    pub source: String,
    /// Index into the config's `rules`; negative = none.
    pub rule: i32,
    pub rule_name: String,
}

/// The headline and the reason, for the "Active now" row.
pub fn explain(a: &Active) -> (String, String) {
    if a.layout.is_empty() {
        return (
            "Nothing shown yet".into(),
            "The agent has not drawn a frame.".into(),
        );
    }
    let why = match a.source.as_str() {
        "rule" if !a.rule_name.is_empty() => format!("because the rule “{}” matches", a.rule_name),
        "rule" if a.rule >= 0 => format!("because rule {} matches", a.rule + 1),
        "rule" => "because a rule matches".to_owned(),
        "default" => "no rule matches, so this is the layer's default layout".to_owned(),
        "forced" => {
            "held from outside: Mirror to Touch Bar, or `sliverctl layout`; rules are ignored"
                .to_owned()
        }
        _ => String::new(),
    };
    (format!("Showing “{}”", a.layout), why)
}

fn read_active() -> Result<Active, String> {
    let conn = Connection::session().map_err(|e| e.to_string())?;
    let text = |name| -> Result<String, String> {
        let v = get(&conn, IFACE_CONTROL, name)?;
        String::try_from(v).map_err(|e| e.to_string())
    };
    Ok(Active {
        layout: text("ActiveLayout")?,
        source: text("ActiveLayoutSource")?,
        rule: get(&conn, IFACE_CONTROL, "ActiveRule")
            .ok()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(-1),
        rule_name: text("ActiveRuleName").unwrap_or_default(),
    })
}

/// Reads the active layout in the background; `done` runs on the main loop (`Err` = no agent
/// or an older one without these properties).
pub fn active(done: impl FnOnce(Result<Active, String>) + 'static) {
    in_background(read_active, || Err(LATE.into()), done);
}

/// One published context, as `Control1.ListContexts` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextInfo {
    pub provider: String,
    pub title: String,
    pub scope: String,
    pub items: i64,
    pub on_screen: bool,
    /// A Flatpak/Snap client (verified by the agent): needs `allow` for status items.
    pub sandboxed: bool,
    /// Top-level item ids and their labels (same order): what `app/<provider>#<id>` places.
    pub item_ids: Vec<String>,
    pub item_labels: Vec<String>,
}

/// An integer from an `a{sv}` reply: `i` when it fits, else `x`.
fn int(d: &std::collections::HashMap<String, OwnedValue>, k: &str) -> Option<i64> {
    let v = d.get(k)?;
    i32::try_from(v.try_clone().ok()?)
        .map(i64::from)
        .ok()
        .or_else(|| i64::try_from(v.try_clone().ok()?).ok())
}

fn list_contexts() -> Result<Vec<ContextInfo>, String> {
    use std::collections::HashMap;
    let conn = Connection::session().map_err(|e| e.to_string())?;
    let reply = conn
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_CONTROL),
            "ListContexts",
            &(),
        )
        .map_err(|e| e.to_string())?;
    let list: Vec<HashMap<String, OwnedValue>> =
        reply.body().deserialize().map_err(|e| e.to_string())?;
    let text = |d: &HashMap<String, OwnedValue>, k: &str| {
        d.get(k)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default()
    };
    // An older agent doesn't send the lists: empty.
    let strings = |d: &HashMap<String, OwnedValue>, k: &str| -> Vec<String> {
        d.get(k)
            .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default()
    };
    Ok(list
        .iter()
        .map(|d| ContextInfo {
            provider: text(d, "provider"),
            title: text(d, "title"),
            scope: text(d, "scope"),
            items: int(d, "items").unwrap_or(0),
            on_screen: d
                .get("on-screen")
                .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
                .unwrap_or(false),
            sandboxed: d
                .get("sandboxed")
                .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
                .unwrap_or(false),
            item_ids: strings(d, "item-ids"),
            item_labels: strings(d, "item-labels"),
        })
        .collect())
}

/// Lists the agent's published contexts in the background; `done` runs on the main loop.
pub fn contexts(done: impl FnOnce(Result<Vec<ContextInfo>, String>) + 'static) {
    in_background(list_contexts, || Err(LATE.into()), done);
}

#[cfg(test)]
mod active_tests {
    use super::*;

    /// Queued Mirror commands: the newest wins, and whoever waits on any of them hears back.
    #[test]
    fn the_newest_mirror_command_wins() {
        let (tx, rx) = mpsc::channel();
        let hold = |l: &str, sent| Hold {
            layout: l.into(),
            select_only: false,
            sent,
        };
        let (ack, acked) = mpsc::channel();
        tx.send(hold("b", Some(ack))).unwrap();
        tx.send(hold("", None)).unwrap();
        let (h, sent) = latest(hold("a", None), &rx);
        assert_eq!(h.layout, "", "the release, sent last");
        assert_eq!(sent.len(), 1);
        sent.into_iter().for_each(|s| s.send(()).unwrap());
        assert!(acked.try_recv().is_ok());
    }

    fn a(source: &str, rule: i32, name: &str) -> Active {
        Active {
            layout: "fkeys".into(),
            source: source.into(),
            rule,
            rule_name: name.into(),
        }
    }

    #[test]
    fn explains_why_the_layout_is_showing() {
        let (t, w) = explain(&a("rule", 2, "Terminals"));
        assert_eq!(t, "Showing “fkeys”");
        assert!(w.contains("“Terminals”"), "{w}");
        assert!(explain(&a("rule", 0, "")).1.contains("rule 1"));
        assert!(explain(&a("default", -1, "")).1.contains("default"));
        assert!(explain(&a("forced", -1, "")).1.contains("Mirror"));
        assert_eq!(explain(&Active::default()).0, "Nothing shown yet");
    }
}

/// A D-Bus failure talking to the agent, in words: not running, older than this app (it lacks a
/// newer method or property), or something else (shown as is). Returns (title, subtitle).
pub fn trouble(err: &str) -> (&'static str, String) {
    let has = |s: &str| err.contains(s);
    if has("ServiceUnknown") || has("NameHasNoOwner") || has("was not provided") {
        (
            "sliver-agent isn't running",
            "Start it to see what the bar shows.".into(),
        )
    } else if has("UnknownProperty") || has("UnknownMethod") || has("UnknownInterface") {
        (
            "sliver-agent is older than this app",
            "Restart it after updating sliver (systemctl --user restart sliver-agent).".into(),
        )
    } else {
        ("The agent isn't answering", err.to_owned())
    }
}

#[cfg(test)]
mod trouble_tests {
    use super::trouble;

    #[test]
    fn words_for_common_failures() {
        assert_eq!(
            trouble("org.freedesktop.DBus.Error.ServiceUnknown: The name io.github.wawow830.Sliver was not provided by any .service files").0,
            "sliver-agent isn't running"
        );
        assert_eq!(
            trouble("org.freedesktop.DBus.Error.UnknownProperty: Unknown property 'ActiveLayout'")
                .0,
            "sliver-agent is older than this app"
        );
        assert_eq!(trouble("timeout").1, "timeout");
    }
}

#[cfg(test)]
mod int_tests {
    use super::int;
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedValue, Value};

    #[test]
    fn ints_read_as_i_or_x() {
        let mut d = HashMap::new();
        d.insert("a".to_owned(), OwnedValue::try_from(Value::I32(2)).unwrap());
        d.insert(
            "b".to_owned(),
            OwnedValue::try_from(Value::I64(1 << 40)).unwrap(),
        );
        d.insert(
            "c".to_owned(),
            OwnedValue::try_from(Value::from("x")).unwrap(),
        );
        assert_eq!(
            int(&d, "a"),
            Some(2),
            "ListContexts sends small counts as `i`"
        );
        assert_eq!(int(&d, "b"), Some(1 << 40));
        assert_eq!(int(&d, "c"), None);
        assert_eq!(int(&d, "missing"), None);
    }
}
