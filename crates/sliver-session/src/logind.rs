//! logind: `LockedHint`, `IdleHint`, `Lock`/`Unlock`, `PrepareForSleep`.
//!
//! `LockedHint` is only a hint set by the screen locker. On the dev machine it read `true`
//! while the session was unlocked, so consumers should treat `locked` as advisory until a
//! per-compositor cross-check exists (docs/integration.md §3).

use crate::{Hub, SessionState, props};
use zbus::Message;
use zbus::blocking::Connection;

const DEST: &str = "org.freedesktop.login1";
const MANAGER: &str = "/org/freedesktop/login1";
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";

/// Which session object we follow.
pub struct Watch {
    /// Real object path of our session; `None` = could not resolve, accept any session's signals.
    session_path: Option<String>,
}

fn resolve_session(conn: &Connection) -> Option<String> {
    // "auto" = the caller's session, or the user's graphical one (works from user services).
    let r = conn.call_method(
        Some(DEST),
        MANAGER,
        Some("org.freedesktop.login1.Manager"),
        "GetSession",
        &("auto",),
    );
    let r = r.or_else(|_| {
        conn.call_method(
            Some(DEST),
            MANAGER,
            Some("org.freedesktop.login1.Manager"),
            "GetSessionByPID",
            &(std::process::id(),),
        )
    });
    let path: zbus::zvariant::OwnedObjectPath = r.ok()?.body().deserialize().ok()?;
    Some(path.as_str().to_owned())
}

/// Initial read. Always returns a `Watch`; fields stay default if logind is absent.
pub fn read_into(conn: &Connection, state: &mut SessionState) -> Watch {
    apply_manager_props(conn, state);
    let session_path = resolve_session(conn);
    let path = session_path
        .as_deref()
        .unwrap_or("/org/freedesktop/login1/session/auto");
    apply_session_props(conn, path, state);
    Watch { session_path }
}

fn apply_manager_props(conn: &Connection, state: &mut SessionState) {
    if let Some(p) = props::get_all(conn, DEST, MANAGER, "org.freedesktop.login1.Manager") {
        if let Some(b) = props::string(&p, "BlockInhibited") {
            state.idle_inhibited = blocks_idle(&b);
        }
    }
}

/// `BlockInhibited` is a colon list such as `idle:sleep`.
fn blocks_idle(s: &str) -> bool {
    s.split(':').any(|w| w == "idle")
}

/// Take a logind `idle` block inhibitor. Closing (dropping) the returned fd releases it.
pub fn inhibit_idle(
    conn: &Connection,
    who: &str,
    why: &str,
) -> zbus::Result<zbus::zvariant::OwnedFd> {
    conn.call_method(
        Some(DEST),
        MANAGER,
        Some("org.freedesktop.login1.Manager"),
        "Inhibit",
        &("idle", who, why, "block"),
    )?
    .body()
    .deserialize()
}

fn apply_session_props(conn: &Connection, path: &str, state: &mut SessionState) {
    if let Some(p) = props::get_all(conn, DEST, path, SESSION_IFACE) {
        if let Some(b) = props::boolean(&p, "LockedHint") {
            state.locked = b;
        }
        if let Some(b) = props::boolean(&p, "IdleHint") {
            state.idle = b;
        }
    }
}

impl Watch {
    fn ours(&self, path: &str) -> bool {
        match &self.session_path {
            Some(p) => p == path,
            None => path.starts_with("/org/freedesktop/login1/session/"),
        }
    }

    /// Handle one signal from login1. `false` = receiver gone.
    pub(crate) fn on_message(&self, conn: &Connection, msg: &Message, hub: &Hub) -> bool {
        let header = msg.header();
        let path = header.path().map(|p| p.as_str()).unwrap_or_default();
        let member = header.member().map(|m| m.as_str()).unwrap_or_default();
        match member {
            "PrepareForSleep" => match msg.body().deserialize::<bool>() {
                Ok(b) => hub.update(|s| s.sleeping = b),
                Err(_) => true,
            },
            "Lock" if self.ours(path) => hub.update(|s| s.locked = true),
            "Unlock" if self.ours(path) => hub.update(|s| s.locked = false),
            "PropertiesChanged" if path == MANAGER => {
                let mut cur = hub.snapshot();
                apply_manager_props(conn, &mut cur);
                hub.update(|s| s.idle_inhibited = cur.idle_inhibited)
            }
            "PropertiesChanged" if self.ours(path) => {
                let mut cur = hub.snapshot();
                apply_session_props(conn, path, &mut cur);
                hub.update(|s| {
                    s.locked = cur.locked;
                    s.idle = cur.idle;
                })
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_block_inhibited() {
        assert!(super::blocks_idle("idle:sleep"));
        assert!(super::blocks_idle("idle"));
        assert!(!super::blocks_idle(""));
        assert!(!super::blocks_idle("shutdown:sleep"));
    }
}
