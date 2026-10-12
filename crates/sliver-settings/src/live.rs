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

/// Mirror to Touch Bar (ux.md §4, Q10): ask the agent to show `layout` on the real bar
/// (`Control1.SetLayout`); `""` returns it to the rules. Fire and forget, off the main thread;
/// an absent agent is fine.
pub fn set_layout(layout: &str) {
    let layout = layout.to_owned();
    std::thread::spawn(move || set_layout_now(&layout));
}

/// Blocking version, for the window closing (a background thread would die with the process).
pub fn set_layout_now(layout: &str) {
    if let Ok(conn) = Connection::session() {
        let _ = conn.call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_CONTROL),
            "SetLayout",
            &(layout,),
        );
    }
}
