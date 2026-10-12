//! niri backend: line-delimited JSON on `$NIRI_SOCKET`.
//!
//! Send `"EventStream"`; niri answers `{"Ok":"Handled"}` and then pushes one JSON object per
//! line, starting with the full current state (`WorkspacesChanged`, `WindowsChanged`,
//! `KeyboardLayoutsChanged`, ...), so no separate snapshot is needed. Every event is folded
//! into a [`Model`] and the [`DesktopState`] is derived from it.
//!
//! Written from the `niri-ipc` documentation and tested against a fake server; not yet run
//! against a real niri (none installed on the dev machine). Unknown events are ignored.
//! Niri has no per-window fullscreen flag in the event stream, so `fullscreen` stays false.

use crate::{Backend, DesktopSource, DesktopState, Window, Workspace};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader, Write};
use std::ops::ControlFlow;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn socket_from_env(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    get("NIRI_SOCKET")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[derive(Debug, Clone, Default)]
struct Ws {
    idx: i64,
    name: Option<String>,
    output: Option<String>,
    focused: bool,
}

#[derive(Debug, Clone, Default)]
struct Win {
    title: String,
    app_id: String,
    pid: Option<u32>,
    floating: bool,
}

/// niri's world as the event stream describes it.
#[derive(Debug, Default)]
pub struct Model {
    workspaces: BTreeMap<u64, Ws>,
    windows: BTreeMap<u64, Win>,
    focused: Option<u64>,
    layouts: Vec<String>,
    layout_idx: usize,
}

fn u(v: &Value, k: &str) -> Option<u64> {
    v.get(k)?.as_u64()
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(str::to_string)
}

fn ws_from(v: &Value) -> Option<(u64, Ws)> {
    Some((
        u(v, "id")?,
        Ws {
            idx: v.get("idx").and_then(Value::as_i64).unwrap_or(0),
            name: s(v, "name"),
            output: s(v, "output"),
            focused: v.get("is_focused").and_then(Value::as_bool) == Some(true),
        },
    ))
}

fn win_from(v: &Value) -> Option<(u64, Win, bool)> {
    Some((
        u(v, "id")?,
        Win {
            title: s(v, "title").unwrap_or_default(),
            app_id: s(v, "app_id").unwrap_or_default(),
            pid: v
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|p| u32::try_from(p).ok()),
            floating: v.get("is_floating").and_then(Value::as_bool) == Some(true),
        },
        v.get("is_focused").and_then(Value::as_bool) == Some(true),
    ))
}

impl Model {
    /// Fold one event object (`{"EventName": {...}}`). Unknown events are ignored.
    pub fn apply(&mut self, event: &Value) {
        let Some((name, body)) = event.as_object().and_then(|o| o.iter().next()) else {
            return;
        };
        match name.as_str() {
            "WorkspacesChanged" => {
                self.workspaces = body
                    .get("workspaces")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(ws_from)
                    .collect();
            }
            "WorkspaceActivated" => {
                if let (Some(id), Some(focused)) =
                    (u(body, "id"), body.get("focused").and_then(Value::as_bool))
                {
                    let output = self.workspaces.get(&id).and_then(|w| w.output.clone());
                    for (wid, w) in self.workspaces.iter_mut() {
                        // Activating a workspace deactivates its siblings on the same output.
                        if *wid == id {
                            w.focused = focused;
                        } else if focused || w.output == output {
                            w.focused = false;
                        }
                    }
                }
            }
            "WindowsChanged" => {
                self.windows.clear();
                for w in body
                    .get("windows")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some((id, win, focused)) = win_from(w) {
                        if focused {
                            self.focused = Some(id);
                        }
                        self.windows.insert(id, win);
                    }
                }
            }
            "WindowOpenedOrChanged" => {
                if let Some((id, win, focused)) = body.get("window").and_then(win_from) {
                    self.windows.insert(id, win);
                    if focused {
                        self.focused = Some(id);
                    }
                }
            }
            "WindowClosed" => {
                if let Some(id) = u(body, "id") {
                    self.windows.remove(&id);
                    if self.focused == Some(id) {
                        self.focused = None;
                    }
                }
            }
            "WindowFocusChanged" => self.focused = u(body, "id"),
            "KeyboardLayoutsChanged" => {
                let kl = body.get("keyboard_layouts").unwrap_or(body);
                self.layouts = kl
                    .get("names")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect();
                self.layout_idx = u(kl, "current_idx").unwrap_or(0) as usize;
            }
            "KeyboardLayoutSwitched" => self.layout_idx = u(body, "idx").unwrap_or(0) as usize,
            _ => {}
        }
    }

    pub fn to_state(&self) -> DesktopState {
        let mut st = DesktopState {
            connected: true,
            backend: Backend::Niri,
            ..Default::default()
        };
        st.window = self
            .focused
            .and_then(|id| self.windows.get(&id).map(|w| (id, w)))
            .map(|(id, w)| Window {
                address: id.to_string(),
                class: w.app_id.clone(),
                title: w.title.clone(),
                pid: w.pid,
                floating: w.floating,
                fullscreen: false,
                xwayland: false,
            });
        if let Some(w) = self.workspaces.values().find(|w| w.focused) {
            st.workspace = Some(Workspace {
                id: w.idx,
                name: w.name.clone().unwrap_or_else(|| w.idx.to_string()),
            });
            st.monitor = w.output.clone();
        }
        let outputs: BTreeSet<String> = self
            .workspaces
            .values()
            .filter_map(|w| w.output.clone())
            .collect();
        st.set_monitors(outputs.into_iter().collect());
        st.keyboard_layout = self.layouts.get(self.layout_idx).cloned();
        st
    }
}

pub struct Watcher {
    reader: BufReader<UnixStream>,
    model: Model,
    state: DesktopState,
}

impl Watcher {
    pub fn connect(socket: &Path) -> io::Result<Self> {
        let mut stream = UnixStream::connect(socket)?;
        stream.write_all(b"\"EventStream\"\n")?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        let mut reader = BufReader::new(stream);
        let mut reply = String::new();
        reader.read_line(&mut reply)?;
        let ok = serde_json::from_str::<Value>(&reply)
            .ok()
            .is_some_and(|v| v.get("Ok").is_some());
        if !ok {
            return Err(io::Error::other(format!(
                "niri refused EventStream: {}",
                reply.trim()
            )));
        }
        let mut w = Self {
            reader,
            model: Model::default(),
            state: DesktopState::default(),
        };
        // niri sends the whole current state right away; take it all before reporting.
        w.reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(150)))?;
        loop {
            let mut line = String::new();
            match w.reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => w.fold(&line),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        w.reader.get_ref().set_read_timeout(None)?;
        w.state = w.model.to_state();
        Ok(w)
    }

    fn fold(&mut self, line: &str) {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            self.model.apply(&v);
        }
    }
}

impl DesktopSource for Watcher {
    fn state(&self) -> &DesktopState {
        &self.state
    }

    fn next_change(&mut self) -> io::Result<Option<DesktopState>> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                self.state = DesktopState::default();
                return Ok(None);
            }
            self.fold(&line);
            let new = self.model.to_state();
            if new != self.state {
                self.state = new.clone();
                return Ok(Some(new));
            }
        }
    }
}

/// [`crate::watch`] for niri.
pub fn watch(socket: &Path, on_change: impl FnMut(&DesktopState) -> ControlFlow<()>) {
    crate::watch(|| Watcher::connect(socket), on_change);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn initial() -> Vec<Value> {
        vec![
            json!({"WorkspacesChanged":{"workspaces":[
                {"id":10,"idx":1,"name":null,"output":"eDP-1","is_active":true,"is_focused":true},
                {"id":11,"idx":2,"name":"web","output":"HDMI-A-1","is_active":true,"is_focused":false}]}}),
            json!({"WindowsChanged":{"windows":[
                {"id":1,"title":"term","app_id":"foot","pid":50,"workspace_id":10,"is_focused":true,"is_floating":false},
                {"id":2,"title":"docs","app_id":"firefox","pid":51,"workspace_id":11,"is_focused":false,"is_floating":true}]}}),
            json!({"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["English (US)","German"],"current_idx":0}}}),
        ]
    }

    #[test]
    fn model_derives_state() {
        let mut m = Model::default();
        for e in initial() {
            m.apply(&e);
        }
        let s = m.to_state();
        let w = s.window.as_ref().unwrap();
        assert_eq!(
            (
                w.class.as_str(),
                w.title.as_str(),
                w.pid,
                w.address.as_str()
            ),
            ("foot", "term", Some(50), "1")
        );
        assert_eq!(
            s.workspace.as_ref().map(|w| (w.id, w.name.as_str())),
            Some((1, "1"))
        );
        assert_eq!(s.monitor.as_deref(), Some("eDP-1"));
        assert_eq!((s.monitors, s.external_display), (2, true));
        assert_eq!(s.keyboard_layout.as_deref(), Some("English (US)"));
        assert_eq!(s.backend, Backend::Niri);
    }

    #[test]
    fn model_folds_updates() {
        let mut m = Model::default();
        for e in initial() {
            m.apply(&e);
        }
        m.apply(&json!({"WindowFocusChanged":{"id":2}}));
        let s = m.to_state();
        assert_eq!(s.window.as_ref().unwrap().class, "firefox");
        assert!(s.window.as_ref().unwrap().floating);
        m.apply(&json!({"WindowOpenedOrChanged":{"window":{"id":2,"title":"new title","app_id":"firefox","pid":51,"is_focused":true,"is_floating":true}}}));
        assert_eq!(m.to_state().window.unwrap().title, "new title");
        m.apply(&json!({"WorkspaceActivated":{"id":11,"focused":true}}));
        let s = m.to_state();
        assert_eq!(s.workspace.as_ref().map(|w| w.name.as_str()), Some("web"));
        assert_eq!(s.monitor.as_deref(), Some("HDMI-A-1"));
        m.apply(&json!({"KeyboardLayoutSwitched":{"idx":1}}));
        assert_eq!(m.to_state().keyboard_layout.as_deref(), Some("German"));
        m.apply(&json!({"WindowClosed":{"id":2}}));
        assert_eq!(m.to_state().window, None);
        m.apply(&json!({"SomethingNew":{"x":1}}));
        m.apply(&json!("garbage"));
    }

    #[test]
    fn socket_env() {
        assert_eq!(
            socket_from_env(|k| (k == "NIRI_SOCKET").then(|| "/n".into())),
            Some("/n".into())
        );
        assert_eq!(socket_from_env(|_| None), None);
    }

    fn fake_niri(then: Vec<Value>) -> (PathBuf, PathBuf) {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sliver-niri-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("niri.sock");
        let l = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            let Ok((mut c, _)) = l.accept() else { return };
            let mut req = String::new();
            BufReader::new(c.try_clone().unwrap())
                .read_line(&mut req)
                .unwrap();
            assert_eq!(req.trim(), "\"EventStream\"");
            let _ = writeln!(c, "{}", json!({"Ok":"Handled"}));
            for e in initial() {
                let _ = writeln!(c, "{e}");
            }
            std::thread::sleep(Duration::from_millis(400));
            for e in then {
                let _ = writeln!(c, "{e}");
            }
        });
        (sock, dir)
    }

    #[test]
    fn watcher_end_to_end() {
        let (sock, dir) = fake_niri(vec![
            json!({"OverviewOpenedOrClosed":{"is_open":true}}), // no visible change
            json!({"WindowFocusChanged":{"id":2}}),
            json!({"KeyboardLayoutSwitched":{"idx":1}}),
        ]);
        let mut w = Watcher::connect(&sock).unwrap();
        let s0 = w.state().clone();
        assert!(s0.connected);
        assert_eq!(
            s0.window.as_ref().unwrap().class,
            "foot",
            "initial burst is folded before reporting"
        );
        let s1 = w.next_change().unwrap().unwrap();
        assert_eq!(s1.window.as_ref().unwrap().class, "firefox");
        let s2 = w.next_change().unwrap().unwrap();
        assert_eq!(s2.keyboard_layout.as_deref(), Some("German"));
        assert_eq!(w.next_change().unwrap(), None);
        assert!(!w.state().connected);
        let _ = std::fs::remove_dir_all(dir);
    }
}
