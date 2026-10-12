//! sway / i3 backend: the i3-ipc protocol on `$SWAYSOCK` (or `$I3SOCK`).
//!
//! Framing: `"i3-ipc"`, payload length (u32), message type (u32), payload; integers in native
//! byte order. Events are replies whose type has the high bit set. We open two connections: one
//! subscribed to `window`, `workspace`, `mode`, `input`, `output`, `shutdown` events, and a
//! fresh one per query (`GET_TREE`, `GET_WORKSPACES`, `GET_OUTPUTS`, `GET_INPUTS`).
//!
//! Written from the i3/sway protocol documentation and tested against a fake server; not yet
//! run against a real sway (none installed on the dev machine). i3 itself lacks `app_id` and
//! `pid` is only present in newer builds; the window class then comes from `window_properties`.

use crate::{Backend, DesktopSource, DesktopState, Window, Workspace};
use serde_json::Value;
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAGIC: &[u8; 6] = b"i3-ipc";
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
const GET_OUTPUTS: u32 = 3;
const GET_TREE: u32 = 4;
const GET_INPUTS: u32 = 100;

const EVENT_FLAG: u32 = 0x8000_0000;
const EV_WORKSPACE: u32 = 0;
const EV_OUTPUT: u32 = 1;
const EV_MODE: u32 = 2;
const EV_WINDOW: u32 = 3;
const EV_SHUTDOWN: u32 = 6;
const EV_INPUT: u32 = 21;

/// `$SWAYSOCK`, else `$I3SOCK`.
pub fn socket_from_env(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    ["SWAYSOCK", "I3SOCK"]
        .iter()
        .find_map(|k| get(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

pub fn encode(ty: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(14 + payload.len());
    v.extend_from_slice(MAGIC);
    v.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    v.extend_from_slice(&ty.to_ne_bytes());
    v.extend_from_slice(payload);
    v
}

/// Read one frame: `Ok(None)` on a clean EOF before the header.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<(u32, Vec<u8>)>> {
    let mut head = [0u8; 14];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    if &head[..6] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad i3-ipc magic",
        ));
    }
    let len = u32::from_ne_bytes(head[6..10].try_into().expect("4 bytes")) as usize;
    let ty = u32::from_ne_bytes(head[10..14].try_into().expect("4 bytes"));
    // Trees can be large, but never this large; refuse rather than allocate gigabytes.
    if len > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "i3-ipc frame too large",
        ));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(Some((ty, payload)))
}

/// One query on a fresh connection.
pub fn query(path: &Path, ty: u32) -> io::Result<Value> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(&encode(ty, b""))?;
    let (_, payload) = read_frame(&mut s)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no reply"))?;
    serde_json::from_slice(&payload).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k)?.as_str()
}

/// Build a [`Window`] from a sway container node (tree node or event `container`).
pub fn window_from_container(c: &Value) -> Window {
    let app_id = str_of(c, "app_id").filter(|s| !s.is_empty());
    let props_class = c.get("window_properties").and_then(|p| str_of(p, "class"));
    let xwayland = app_id.is_none() && c.get("window_properties").is_some_and(|p| !p.is_null())
        || str_of(c, "shell") == Some("xwayland");
    Window {
        address: c
            .get("id")
            .and_then(Value::as_i64)
            .map(|i| i.to_string())
            .unwrap_or_default(),
        class: app_id.or(props_class).unwrap_or_default().to_string(),
        title: str_of(c, "name").unwrap_or_default().to_string(),
        pid: c
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok()),
        floating: str_of(c, "type") == Some("floating_con"),
        fullscreen: c
            .get("fullscreen_mode")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            >= 1,
        xwayland,
    }
}

fn is_leaf_window(n: &Value) -> bool {
    n.get("pid").is_some_and(|p| !p.is_null())
        || str_of(n, "app_id").is_some()
        || n.get("window_properties").is_some_and(|p| !p.is_null())
}

/// The focused window in a `GET_TREE` reply, if any.
pub fn focused_window(tree: &Value) -> Option<Window> {
    fn walk(n: &Value, floating: bool) -> Option<Window> {
        if n.get("focused").and_then(Value::as_bool) == Some(true) && is_leaf_window(n) {
            let mut w = window_from_container(n);
            w.floating |= floating;
            return Some(w);
        }
        for (key, fl) in [("nodes", false), ("floating_nodes", true)] {
            if let Some(kids) = n.get(key).and_then(Value::as_array) {
                if let Some(w) = kids.iter().find_map(|k| walk(k, fl || floating)) {
                    return Some(w);
                }
            }
        }
        None
    }
    walk(tree, false)
}

/// Every window in a `GET_TREE` reply with the workspace it sits on, in tree order.
pub fn windows_from_tree(tree: &Value) -> Vec<crate::WindowInfo> {
    fn walk(n: &Value, ws: i64, out: &mut Vec<crate::WindowInfo>) {
        let ws = if str_of(n, "type") == Some("workspace") {
            n.get("num").and_then(Value::as_i64).unwrap_or(-1)
        } else {
            ws
        };
        let leaf = is_leaf_window(n)
            && n.get("nodes")
                .and_then(Value::as_array)
                .is_none_or(|a| a.is_empty())
            && n.get("floating_nodes")
                .and_then(Value::as_array)
                .is_none_or(|a| a.is_empty());
        if leaf && str_of(n, "type") != Some("workspace") {
            let w = window_from_container(n);
            out.push(crate::WindowInfo {
                address: w.address,
                class: w.class,
                title: w.title,
                workspace: ws,
                focused: n.get("focused").and_then(Value::as_bool) == Some(true),
                urgent: n.get("urgent").and_then(Value::as_bool) == Some(true),
                pid: w.pid,
            });
        }
        for key in ["nodes", "floating_nodes"] {
            for k in n.get(key).and_then(Value::as_array).into_iter().flatten() {
                walk(k, ws, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, -1, &mut out);
    out
}

/// All workspaces from `GET_WORKSPACES`, counting windows from `windows`.
pub fn workspaces_from_list(
    list: &Value,
    windows: &[crate::WindowInfo],
) -> Vec<crate::WorkspaceInfo> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let ws = workspace_from(w)?;
            Some(crate::WorkspaceInfo {
                id: ws.id,
                windows: windows.iter().filter(|x| x.workspace == ws.id).count() as u32,
                active: w.get("focused").and_then(Value::as_bool) == Some(true),
                urgent: w.get("urgent").and_then(Value::as_bool) == Some(true),
                monitor: str_of(w, "output").map(str::to_string),
                name: ws.name,
            })
        })
        .collect()
}

fn workspace_from(v: &Value) -> Option<Workspace> {
    let name = str_of(v, "name")?.to_string();
    // Named workspaces have num -1; the name is then their only identity.
    let id = v.get("num").and_then(Value::as_i64).unwrap_or(-1);
    Some(Workspace { id, name })
}

/// The focused workspace and its output, from `GET_WORKSPACES`.
pub fn focused_workspace(list: &Value) -> Option<(Workspace, Option<String>)> {
    let ws = list
        .as_array()?
        .iter()
        .find(|w| w.get("focused").and_then(Value::as_bool) == Some(true))?;
    Some((
        workspace_from(ws)?,
        str_of(ws, "output").map(str::to_string),
    ))
}

/// Active output names from `GET_OUTPUTS`.
pub fn active_outputs(list: &Value) -> Vec<String> {
    let mut v: Vec<String> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o.get("active").and_then(Value::as_bool) != Some(false))
        .filter_map(|o| str_of(o, "name").map(str::to_string))
        .collect();
    v.sort();
    v
}

/// Active layout of the first keyboard in `GET_INPUTS`.
pub fn keyboard_layout(list: &Value) -> Option<String> {
    list.as_array()?
        .iter()
        .filter(|i| str_of(i, "type") == Some("keyboard"))
        .find_map(|i| str_of(i, "xkb_active_layout_name").map(str::to_string))
}

/// What the watcher must do after folding an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Followup {
    None,
    /// Re-read the focused window from the tree (an empty workspace got focus, ...).
    RefreshWindow,
    RefreshOutputs,
    /// sway is shutting down.
    Disconnect,
}

/// Fold one event into the state. Returns whether the state changed and what to re-query.
pub fn apply_event(state: &mut DesktopState, ty: u32, body: &Value) -> (bool, Followup) {
    let before = state.clone();
    let mut follow = Followup::None;
    let change = str_of(body, "change").unwrap_or_default();
    match ty & !EVENT_FLAG {
        EV_WINDOW => {
            let c = body.get("container").cloned().unwrap_or(Value::Null);
            let id = window_from_container(&c).address;
            match change {
                "focus" => state.window = Some(window_from_container(&c)),
                "title" => {
                    if let Some(w) = state.window.as_mut().filter(|w| w.address == id) {
                        w.title = str_of(&c, "name").unwrap_or_default().to_string();
                    }
                    follow = Followup::RefreshWindow; // the taskbar shows titles too
                }
                "new" | "move" | "urgent" => follow = Followup::RefreshWindow,
                "close" => {
                    if state.window.as_ref().is_some_and(|w| w.address == id) {
                        state.window = None;
                        follow = Followup::RefreshWindow; // focus moved to a sibling
                    }
                }
                "fullscreen_mode" | "floating" => {
                    if let Some(w) = state.window.as_mut().filter(|w| w.address == id) {
                        let n = window_from_container(&c);
                        w.fullscreen = n.fullscreen;
                        w.floating = n.floating;
                    }
                }
                _ => {}
            }
        }
        EV_WORKSPACE if change == "focus" => {
            if let Some(ws) = body.get("current").and_then(workspace_from) {
                state.workspace = Some(ws);
            }
            if let Some(o) = body.get("current").and_then(|c| str_of(c, "output")) {
                state.monitor = Some(o.to_string());
            }
            follow = Followup::RefreshWindow;
        }
        // init, empty, rename, urgent, move, reload: the workspace list changed.
        EV_WORKSPACE => follow = Followup::RefreshWindow,
        EV_OUTPUT => follow = Followup::RefreshOutputs,
        EV_MODE => {
            state.submap = Some(change.to_string()).filter(|m| !m.is_empty() && m != "default");
        }
        EV_INPUT if change == "xkb_layout" || change == "xkb_keymap" => {
            let input = body.get("input").cloned().unwrap_or(Value::Null);
            if str_of(&input, "type") == Some("keyboard") {
                if let Some(l) = str_of(&input, "xkb_active_layout_name") {
                    state.keyboard_layout = Some(l.to_string());
                }
            }
        }
        EV_SHUTDOWN => follow = Followup::Disconnect,
        _ => {}
    }
    (*state != before, follow)
}

/// A live sway connection.
pub struct Watcher {
    socket: PathBuf,
    events: UnixStream,
    state: DesktopState,
}

impl Watcher {
    /// Subscribe first, then snapshot, so no change in between is lost.
    pub fn connect(socket: &Path) -> io::Result<Self> {
        let mut events = UnixStream::connect(socket)?;
        events.write_all(&encode(
            SUBSCRIBE,
            br#"["window","workspace","mode","input","output","shutdown"]"#,
        ))?;
        let (_, reply) = read_frame(&mut events)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no subscribe reply"))?;
        let ok = serde_json::from_slice::<Value>(&reply)
            .ok()
            .and_then(|v| v.get("success").and_then(Value::as_bool));
        if ok != Some(true) {
            return Err(io::Error::other("sway refused the event subscription"));
        }
        let mut w = Self {
            socket: socket.to_path_buf(),
            events,
            state: DesktopState::default(),
        };
        w.state.connected = true;
        w.state.backend = Backend::Sway;
        w.resync();
        Ok(w)
    }

    fn resync(&mut self) {
        self.refresh_window();
        if let Ok(v) = query(&self.socket, GET_WORKSPACES) {
            if let Some((ws, mon)) = focused_workspace(&v) {
                self.state.workspace = Some(ws);
                self.state.monitor = mon;
            }
        }
        self.refresh_outputs();
        if let Ok(v) = query(&self.socket, GET_INPUTS) {
            if let Some(l) = keyboard_layout(&v) {
                self.state.keyboard_layout = Some(l);
            }
        }
        self.state.sync_flags();
    }

    fn refresh_window(&mut self) {
        if let Ok(tree) = query(&self.socket, GET_TREE) {
            self.state.window = focused_window(&tree);
            self.state.windows = windows_from_tree(&tree);
        }
        if let Ok(list) = query(&self.socket, GET_WORKSPACES) {
            self.state.workspaces = workspaces_from_list(&list, &self.state.windows);
        }
        self.state.sync_flags();
    }

    fn refresh_outputs(&mut self) {
        if let Ok(v) = query(&self.socket, GET_OUTPUTS) {
            self.state.set_monitors(active_outputs(&v));
        }
    }
}

impl DesktopSource for Watcher {
    fn state(&self) -> &DesktopState {
        &self.state
    }

    fn next_change(&mut self) -> io::Result<Option<DesktopState>> {
        loop {
            let Some((ty, payload)) = read_frame(&mut self.events)? else {
                self.state = DesktopState::default();
                return Ok(None);
            };
            if ty & EVENT_FLAG == 0 {
                continue; // a stray reply
            }
            let body: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
            let (mut changed, follow) = apply_event(&mut self.state, ty, &body);
            self.state.sync_flags();
            let before = self.state.clone();
            match follow {
                Followup::None => {}
                Followup::RefreshWindow => self.refresh_window(),
                Followup::RefreshOutputs => self.refresh_outputs(),
                Followup::Disconnect => {
                    self.state = DesktopState::default();
                    return Ok(None);
                }
            }
            changed |= self.state != before;
            if changed {
                return Ok(Some(self.state.clone()));
            }
        }
    }
}

/// [`crate::watch`] for sway/i3.
pub fn watch(socket: &Path, on_change: impl FnMut(&DesktopState) -> ControlFlow<()>) {
    crate::watch(|| Watcher::connect(socket), on_change);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn frame_roundtrip() {
        let f = encode(EVENT_FLAG | EV_WINDOW, b"{\"a\":1}");
        let mut r = &f[..];
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some((EVENT_FLAG | EV_WINDOW, b"{\"a\":1}".to_vec()))
        );
        assert_eq!(read_frame(&mut r).unwrap(), None);
        let mut bad = &b"nope-nope-nope-"[..];
        assert!(read_frame(&mut bad).is_err());
    }

    fn tree() -> Value {
        json!({"type":"root","nodes":[{"type":"output","name":"eDP-1","nodes":[
            {"type":"workspace","name":"1","num":1,"focused":false,"nodes":[
                {"id":10,"type":"con","name":"term","app_id":"foot","pid":100,"focused":false,"fullscreen_mode":0,"nodes":[],"floating_nodes":[]}],
             "floating_nodes":[]},
            {"type":"workspace","name":"2","num":2,"focused":false,"nodes":[
                {"id":11,"type":"con","name":"Firefox","app_id":"firefox","pid":101,"focused":true,"fullscreen_mode":1,"nodes":[],"floating_nodes":[]}],
             "floating_nodes":[
                {"id":12,"type":"floating_con","name":"X","app_id":null,"pid":102,"window_properties":{"class":"Xclock"},"focused":false,"nodes":[],"floating_nodes":[]}]}
        ],"floating_nodes":[]}],"floating_nodes":[]})
    }

    #[test]
    fn finds_focused_leaf_not_workspace() {
        let w = focused_window(&tree()).unwrap();
        assert_eq!(
            (w.address.as_str(), w.class.as_str(), w.title.as_str()),
            ("11", "firefox", "Firefox")
        );
        assert_eq!(
            (w.pid, w.fullscreen, w.floating, w.xwayland),
            (Some(101), true, false, false)
        );
        // a focused workspace container with no window is not a window
        let empty = json!({"type":"workspace","focused":true,"nodes":[],"floating_nodes":[]});
        assert_eq!(focused_window(&empty), None);
    }

    #[test]
    fn xwayland_and_floating_from_node() {
        let c = json!({"id":12,"type":"floating_con","name":"X","app_id":null,"pid":102,
                       "window_properties":{"class":"Xclock"}});
        let w = window_from_container(&c);
        assert_eq!(
            (w.class.as_str(), w.xwayland, w.floating),
            ("Xclock", true, true)
        );
    }

    #[test]
    fn parses_lists() {
        let ws = json!([{"name":"1","num":1,"focused":false,"output":"A"},{"name":"web","num":-1,"focused":true,"output":"B"}]);
        let (w, mon) = focused_workspace(&ws).unwrap();
        assert_eq!(
            (w.id, w.name.as_str(), mon.as_deref()),
            (-1, "web", Some("B"))
        );
        assert_eq!(
            active_outputs(
                &json!([{"name":"HDMI-A-1","active":true},{"name":"eDP-1","active":true},{"name":"off","active":false}])
            ),
            ["HDMI-A-1", "eDP-1"]
        );
        let inputs =
            json!([{"type":"pointer"},{"type":"keyboard","xkb_active_layout_name":"English (US)"}]);
        assert_eq!(keyboard_layout(&inputs).as_deref(), Some("English (US)"));
    }

    fn ev(state: &mut DesktopState, ty: u32, body: Value) -> (bool, Followup) {
        apply_event(state, EVENT_FLAG | ty, &body)
    }

    #[test]
    fn folds_events() {
        let mut s = DesktopState {
            connected: true,
            ..Default::default()
        };
        let c = json!({"id":11,"type":"con","name":"Firefox","app_id":"firefox","pid":101});
        assert!(
            ev(
                &mut s,
                EV_WINDOW,
                json!({"change":"focus","container":c.clone()})
            )
            .0
        );
        assert_eq!(s.window.as_ref().unwrap().class, "firefox");
        // title change of another window is ignored
        assert!(
            !ev(
                &mut s,
                EV_WINDOW,
                json!({"change":"title","container":{"id":99,"name":"x"}})
            )
            .0
        );
        assert!(
            ev(
                &mut s,
                EV_WINDOW,
                json!({"change":"title","container":{"id":11,"name":"Docs"}})
            )
            .0
        );
        assert_eq!(s.window.as_ref().unwrap().title, "Docs");
        assert!(
            ev(
                &mut s,
                EV_WINDOW,
                json!({"change":"fullscreen_mode","container":{"id":11,"fullscreen_mode":1}})
            )
            .0
        );
        assert!(s.window.as_ref().unwrap().fullscreen);
        let (changed, follow) = ev(
            &mut s,
            EV_WINDOW,
            json!({"change":"close","container":{"id":11}}),
        );
        assert!(changed && follow == Followup::RefreshWindow && s.window.is_none());
        // workspace focus
        let (c, f) = ev(
            &mut s,
            EV_WORKSPACE,
            json!({"change":"focus","current":{"name":"3","num":3,"output":"eDP-1"}}),
        );
        assert!(c && f == Followup::RefreshWindow);
        assert_eq!(
            (s.workspace.as_ref().unwrap().id, s.monitor.as_deref()),
            (3, Some("eDP-1"))
        );
        // mode and layout
        assert!(ev(&mut s, EV_MODE, json!({"change":"resize"})).0);
        assert_eq!(s.submap.as_deref(), Some("resize"));
        assert!(ev(&mut s, EV_MODE, json!({"change":"default"})).0);
        assert_eq!(s.submap, None);
        assert!(ev(&mut s, EV_INPUT, json!({"change":"xkb_layout","input":{"type":"keyboard","xkb_active_layout_name":"German"}})).0);
        assert_eq!(s.keyboard_layout.as_deref(), Some("German"));
        assert!(
            !ev(
                &mut s,
                EV_INPUT,
                json!({"change":"xkb_layout","input":{"type":"pointer"}})
            )
            .0
        );
        assert_eq!(
            ev(&mut s, EV_OUTPUT, json!({"change":"unspecified"})).1,
            Followup::RefreshOutputs
        );
        assert_eq!(
            ev(&mut s, EV_SHUTDOWN, json!({"change":"exit"})).1,
            Followup::Disconnect
        );
    }

    #[test]
    fn lists_from_tree_and_workspaces() {
        let w = windows_from_tree(&tree());
        let ids: Vec<(&str, i64)> = w
            .iter()
            .map(|x| (x.address.as_str(), x.workspace))
            .collect();
        assert_eq!(ids, [("10", 1), ("11", 2), ("12", 2)]);
        assert!(w[1].focused && !w[0].focused);
        let list = json!([
            {"name":"1","num":1,"focused":false,"urgent":false,"output":"eDP-1"},
            {"name":"2","num":2,"focused":true,"urgent":true,"output":"eDP-1"}
        ]);
        let ws = workspaces_from_list(&list, &w);
        assert_eq!(
            ws.iter()
                .map(|x| (x.id, x.windows, x.active, x.urgent))
                .collect::<Vec<_>>(),
            [(1, 1, false, false), (2, 2, true, true)]
        );
    }

    #[test]
    fn socket_env_order() {
        let g = |k: &str| (k == "I3SOCK").then(|| "/i3".to_string());
        assert_eq!(socket_from_env(g), Some("/i3".into()));
        let both = |k: &str| Some(format!("/{k}"));
        assert_eq!(socket_from_env(both), Some("/SWAYSOCK".into()));
        assert_eq!(socket_from_env(|_| None), None);
    }

    /// Fake sway: answers queries, acks the subscription, then pushes `events` and hangs up.
    fn fake_sway(events: Vec<(u32, Value)>) -> (PathBuf, impl Drop) {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sliver-sway-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("sway.sock");
        let l = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            let mut events = Some(events);
            for conn in l.incoming() {
                let Ok(mut c) = conn else { return };
                let Ok(Some((ty, _))) = read_frame(&mut c) else {
                    continue;
                };
                let reply = match ty {
                    SUBSCRIBE => json!({"success":true}),
                    GET_TREE => tree(),
                    GET_WORKSPACES => json!([{"name":"2","num":2,"focused":true,"output":"eDP-1"}]),
                    GET_OUTPUTS => json!([{"name":"eDP-1","active":true}]),
                    GET_INPUTS => {
                        json!([{"type":"keyboard","xkb_active_layout_name":"English (US)"}])
                    }
                    _ => json!({}),
                };
                let _ = c.write_all(&encode(ty, reply.to_string().as_bytes()));
                if ty == SUBSCRIBE {
                    let evs = events.take().unwrap_or_default();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(100));
                        for (t, b) in evs {
                            let _ = c.write_all(&encode(EVENT_FLAG | t, b.to_string().as_bytes()));
                        }
                        // drop = sway died
                    });
                }
            }
        });
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        (sock, Cleanup(dir))
    }

    #[test]
    fn watcher_end_to_end() {
        let (sock, _guard) = fake_sway(vec![
            (EV_MODE, json!({"change":"resize"})),
            (
                EV_WORKSPACE,
                json!({"change":"focus","current":{"name":"1","num":1,"output":"eDP-1"}}),
            ),
            (
                EV_WINDOW,
                json!({"change":"title","container":{"id":999,"name":"ignored"}}),
            ),
        ]);
        let mut w = Watcher::connect(&sock).unwrap();
        let s0 = w.state().clone();
        assert!(s0.connected);
        assert_eq!(s0.backend, Backend::Sway);
        assert_eq!(s0.window.as_ref().unwrap().class, "firefox");
        assert_eq!(s0.workspace.as_ref().unwrap().name, "2");
        assert_eq!((s0.monitors, s0.external_display), (1, false));
        assert_eq!(s0.windows.len(), 3, "taskbar list from the tree");
        assert_eq!(s0.workspaces.len(), 1);
        assert!(s0.workspaces[0].active && s0.windows.iter().filter(|w| w.focused).count() == 1);
        assert_eq!(s0.keyboard_layout.as_deref(), Some("English (US)"));

        assert_eq!(
            w.next_change().unwrap().unwrap().submap.as_deref(),
            Some("resize")
        );
        let s2 = w.next_change().unwrap().unwrap();
        assert_eq!(s2.workspace.as_ref().unwrap().name, "1");
        assert_eq!(w.next_change().unwrap(), None, "hang-up ends the stream");
        assert!(!w.state().connected);
    }
}
