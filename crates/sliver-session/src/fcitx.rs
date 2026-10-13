//! fcitx5 as a second source for the `language` module (integration.md §3): the current input
//! method (`pinyin`, `mozc`, `keyboard-us`...) with its short label (拼, あ, en) and the others in
//! the active group, for CJK users whose "layout" is an input method, not a keymap.
//!
//! Everything comes from `org.fcitx.Fcitx5` `/controller` (`org.fcitx.Fcitx.Controller1`). State
//! reads never start fcitx5 (`NoAutoStart`); when it isn't running `SessionState::input_method` is
//! `None` and the compositor's layout is shown as before.
//!
//! **No change signal exists.** `CurrentInputMethod` is a method, and fcitx5 emits nothing when the
//! user switches with the hotkey (checked on fcitx5 5.1.23: `Controller1` has only
//! `InputMethodGroupsChanged`, and the tray item sends no `NewToolTip` without a real UI).
//! So the state is re-read
//!
//! * when fcitx5 appears or leaves the bus, and when its groups change (signals),
//! * after our own switch ([`Handle::refresh_input_method`]) and when the focused window changes
//!   (each window has its own input context),
//! * on the tray item's `NewIcon`/`NewToolTip`/`NewTitle`, where a real session sends them,
//! * and every [`POLL`] (one call, `CurrentInputMethod`) while the bar is on ([`Handle::set_awake`]) and fcitx5 is running: that is
//!   the only way to see a hotkey switch. It stops when the bar is off, and never starts when
//!   fcitx5 isn't running (invariant 3: wakes only while shown).

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use crate::{Handle, props};

const NAME: &str = "org.fcitx.Fcitx5";
const PATH: &str = "/controller";
const IFACE: &str = "org.fcitx.Fcitx.Controller1";

/// How often the current input method is read while the bar is on and fcitx5 runs.
pub const POLL: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_millis(2000)
};

/// One input method.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// fcitx5's unique name: `keyboard-us`, `pinyin`, `mozc`.
    pub id: String,
    /// `Pinyin`, `Keyboard - English (US)`.
    pub name: String,
    /// The short label fcitx5 shows in its tray icon: `拼`, `あ`, `en`.
    pub label: String,
}

/// The current input method and the active group's others.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMethod {
    pub current: Item,
    /// Everything in the active group, in order (includes the current one).
    pub group: Vec<Item>,
}

type Info = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    bool,
    String,
    HashMap<String, OwnedValue>,
);

fn item(i: &Info) -> Item {
    // The label is empty for some methods: fall back to the id's last word.
    let label = if i.4.is_empty() {
        i.0.rsplit('-').next().unwrap_or(&i.0).to_owned()
    } else {
        i.4.clone()
    };
    Item {
        id: i.0.clone(),
        name: i.1.clone(),
        label,
    }
}

/// Reads fcitx5's current input method and group: whether fcitx5 is running at all, and its
/// state. The state is `None` while no input context has one (`CurrentInputMethod` is empty until
/// some window is focused).
pub fn read(conn: &Connection) -> (bool, Option<InputMethod>) {
    let Some(info) =
        props::call_no_autostart::<_, Info>(conn, NAME, PATH, IFACE, "CurrentInputMethodInfo", &())
    else {
        return (false, None);
    };
    if info.0.is_empty() {
        return (true, None);
    }
    let current = item(&info);
    let mut group = Vec::new();
    if let Some(name) = props::call_no_autostart::<_, String>(
        conn,
        NAME,
        PATH,
        IFACE,
        "CurrentInputMethodGroup",
        &(),
    ) && let Some((_, _, _, _, items)) = props::call_no_autostart::<
        _,
        (
            String,
            String,
            String,
            HashMap<String, OwnedValue>,
            Vec<Info>,
        ),
    >(
        conn,
        NAME,
        PATH,
        IFACE,
        "FullInputMethodGroupInfo",
        &(name,),
    ) {
        group = items.iter().map(item).collect();
    }
    if group.is_empty() {
        group.push(current.clone());
    }
    (true, Some(InputMethod { current, group }))
}

/// Switch input method: `None` toggles (fcitx5's own activate/deactivate: between the first two
/// of the group), `Some(id)` selects that one.
pub fn switch(conn: &Connection, id: Option<&str>) -> Result<(), String> {
    let proxy = zbus::blocking::Proxy::new(conn, NAME, PATH, IFACE).map_err(|e| e.to_string())?;
    match id {
        None => proxy.call_method("Toggle", &()),
        Some(id) => proxy.call_method("SetCurrentIM", &(id,)),
    }
    .map(|_| ())
    .map_err(|e| format!("fcitx5: {e}"))
}

/// Messages to the worker thread.
enum Msg {
    /// Read now.
    Refresh,
    /// The bar turned on or off.
    Awake(bool),
}

/// The running source's mailbox (set by [`Handle::start_input_method`]).
#[derive(Default, Clone)]
pub(crate) struct Mailbox(Arc<Mutex<Option<Sender<Msg>>>>);

impl Mailbox {
    fn send(&self, m: Msg) {
        if let Some(tx) = self.0.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(m);
        }
    }

    pub(crate) fn awake(&self, on: bool) {
        self.send(Msg::Awake(on));
    }
}

const RULES: [&str; 5] = [
    "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0='org.fcitx.Fcitx5'",
    "type='signal',interface='org.fcitx.Fcitx.Controller1',member='InputMethodGroupsChanged'",
    "type='signal',interface='org.kde.StatusNotifierItem',member='NewIcon'",
    "type='signal',interface='org.kde.StatusNotifierItem',member='NewToolTip'",
    "type='signal',interface='org.kde.StatusNotifierItem',member='NewTitle'",
];

/// Is `sender` (a unique name) a connection of the process that owns `org.fcitx.Fcitx5`? fcitx5's
/// tray item lives on another connection of the same process.
fn is_fcitx(conn: &Connection, sender: &str, cache: &mut HashMap<String, bool>) -> bool {
    if let Some(v) = cache.get(sender) {
        return *v;
    }
    let pid = |name: &str| -> Option<u32> {
        props::call_no_autostart(
            conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            &(name,),
        )
    };
    let v = matches!((pid(sender), pid(NAME)), (Some(a), Some(b)) if a == b);
    cache.insert(sender.to_owned(), v);
    v
}

impl Handle {
    /// Follow fcitx5's current input method into `SessionState::input_method`. `conn` is a
    /// session-bus connection dedicated to this. Cheap when fcitx5 isn't installed or running:
    /// one signal subscription and a sleeping thread.
    pub fn start_input_method(&self, conn: Connection) {
        let (tx, rx) = channel();
        *self
            .input_method
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
        let signals = crate::subscribe(&conn, &RULES);
        let hub = self.hub.clone();
        // Signals only say "look again".
        let reader = conn.clone();
        std::thread::Builder::new()
            .name("sliver-session-fcitx-signals".into())
            .spawn(move || {
                let mut cache = HashMap::new();
                for msg in signals {
                    let h = msg.header();
                    let member = h
                        .member()
                        .map(|m| m.as_str().to_owned())
                        .unwrap_or_default();
                    let relevant = match member.as_str() {
                        "NameOwnerChanged" => {
                            cache.clear();
                            true
                        }
                        "InputMethodGroupsChanged" => true,
                        _ => h
                            .sender()
                            .is_some_and(|s| is_fcitx(&reader, s.as_str(), &mut cache)),
                    };
                    if relevant && tx.send(Msg::Refresh).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn thread");
        std::thread::Builder::new()
            .name("sliver-session-fcitx".into())
            .spawn(move || work(conn, rx, hub))
            .expect("spawn thread");
    }

    /// Read fcitx5's state now (after a switch, or when the focused window changed).
    pub fn refresh_input_method(&self) {
        self.input_method.send(Msg::Refresh);
    }
}

/// `CurrentInputMethod`: fcitx5's unique name for the current method, `""` when no window has
/// one, `None` when fcitx5 isn't there. One cheap call: what the poll asks.
fn current(conn: &Connection) -> Option<String> {
    props::call_no_autostart(conn, NAME, PATH, IFACE, "CurrentInputMethod", &())
}

fn work(conn: Connection, rx: Receiver<Msg>, hub: crate::Hub) {
    let mut awake = true;
    let mut running = false;
    let mut first = true;
    // The current method's id as of the last full read (`""`: none).
    let mut last: Option<String> = None;
    loop {
        // Sleep until something happens; while fcitx5 is there and the bar is on, also wake after
        // POLL (a hotkey switch sends nothing). A message or the first round reads everything
        // (3 calls); a bare poll asks only for the current method (1 call) and reads the rest
        // only if that changed, because every call costs the agent a handful of wake-ups.
        let mut full = std::mem::take(&mut first);
        let mut poll = false;
        let msg = if full {
            None
        } else if awake && running {
            match rx.recv_timeout(POLL) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => {
                    poll = true;
                    None
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => return,
            }
        };
        let mut on_message = |m: Msg, full: &mut bool| match m {
            Msg::Refresh => *full = true,
            Msg::Awake(on) => {
                *full |= on && !awake;
                awake = on;
            }
        };
        if let Some(m) = msg {
            on_message(m, &mut full);
        }
        // Burst of signals: one read.
        while let Ok(m) = rx.try_recv() {
            on_message(m, &mut full);
        }
        if !awake {
            continue;
        }
        if poll && !full {
            match current(&conn) {
                Some(id) if last.as_deref() == Some(id.as_str()) => continue,
                _ => full = true,
            }
        }
        if !full {
            continue;
        }
        let (up, state) = read(&conn);
        running = up;
        last = Some(
            state
                .as_ref()
                .map(|s| s.current.id.clone())
                .unwrap_or_default(),
        );
        if !hub.update(|s| s.input_method = state) {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
