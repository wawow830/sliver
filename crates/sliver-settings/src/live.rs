//! Live agent status for the Status page (ux.md §3): the agent's Control1/TouchBar1 properties
//! on the session bus, read on a background thread so the window never blocks on D-Bus.

use std::sync::mpsc;
use std::time::Duration;

use sliver_ipc::dbus::{BUS_NAME, IFACE_CONTROL, IFACE_TOUCHBAR, ROOT_PATH};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

#[derive(Debug, Clone, PartialEq)]
pub enum Agent {
    Running {
        healthy: bool,
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
        layer: s(IFACE_CONTROL, "ActiveLayer").unwrap_or_default(),
        display: s(IFACE_TOUCHBAR, "DisplayState").unwrap_or_default(),
        // A number today (API version); a string would work too.
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

/// Queries in the background and calls `done` on the GTK main loop (within ~3 s).
pub fn agent_status(done: impl Fn(Agent) + 'static) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(query());
    });
    let mut waited = Duration::ZERO;
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        waited += Duration::from_millis(50);
        match rx.try_recv() {
            Ok(a) => {
                done(a);
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) if waited < Duration::from_secs(3) => {
                gtk::glib::ControlFlow::Continue
            }
            Err(_) => {
                done(Agent::Absent("no answer within 3 s".into()));
                gtk::glib::ControlFlow::Break
            }
        }
    });
}

/// The session-bus connection Mirror holds for the window's lifetime: `HoldLayout` lasts as long
/// as the connection that asked, so a crashed or killed editor never pins the bar (cedar, 4b5b28f).
fn mirror_conn() -> Option<Connection> {
    static CONN: std::sync::OnceLock<Option<Connection>> = std::sync::OnceLock::new();
    CONN.get_or_init(|| Connection::session().ok()).clone()
}

fn hold(conn: &Connection, layout: &str) {
    let _ = conn.call_method(
        Some(BUS_NAME),
        ROOT_PATH,
        Some(IFACE_CONTROL),
        "HoldLayout",
        &(layout,),
    );
}

/// Mirror to Touch Bar (ux.md §4, Q10): show `layout` on the real bar while we hold it
/// (`Control1.HoldLayout`). `""` releases it back to the rules. Off the main thread; an absent agent
/// is fine.
pub fn set_layout(layout: &str) {
    let layout = layout.to_owned();
    std::thread::spawn(move || {
        if let Some(c) = mirror_conn() {
            hold(&c, &layout);
        }
    });
}

/// Blocking release for the window closing (process exit would also drop the hold).
pub fn set_layout_now(layout: &str) {
    if let Some(c) = mirror_conn() {
        hold(&c, layout);
    }
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
pub fn active(done: impl Fn(Result<Active, String>) + 'static) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_active());
    });
    let mut waited = Duration::ZERO;
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        waited += Duration::from_millis(50);
        match rx.try_recv() {
            Ok(r) => {
                done(r);
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) if waited < Duration::from_secs(3) => {
                gtk::glib::ControlFlow::Continue
            }
            Err(_) => {
                done(Err("no answer within 3 s".into()));
                gtk::glib::ControlFlow::Break
            }
        }
    });
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
    Ok(list
        .iter()
        .map(|d| ContextInfo {
            provider: text(d, "provider"),
            title: text(d, "title"),
            scope: text(d, "scope"),
            items: d
                .get("items")
                .and_then(|v| i64::try_from(v.try_clone().ok()?).ok())
                .unwrap_or(0),
            on_screen: d
                .get("on-screen")
                .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
                .unwrap_or(false),
            sandboxed: d
                .get("sandboxed")
                .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
                .unwrap_or(false),
        })
        .collect())
}

/// Lists the agent's published contexts in the background; `done` runs on the main loop.
pub fn contexts(done: impl Fn(Result<Vec<ContextInfo>, String>) + 'static) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(list_contexts());
    });
    let mut waited = Duration::ZERO;
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        waited += Duration::from_millis(50);
        match rx.try_recv() {
            Ok(r) => {
                done(r);
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) if waited < Duration::from_secs(3) => {
                gtk::glib::ControlFlow::Continue
            }
            Err(_) => {
                done(Err("no answer within 3 s".into()));
                gtk::glib::ControlFlow::Break
            }
        }
    });
}

#[cfg(test)]
mod active_tests {
    use super::*;

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
