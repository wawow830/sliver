//! Hyprland backend: the `.socket2.sock` event stream plus `.socket.sock` queries.
//!
//! Wire format (Hyprland IPC): `socket2` pushes lines `EVENT>>DATA\n`; `socket1` takes one
//! command per connection (`j/activewindow` for JSON) and replies until EOF. Everything here is
//! read-only: we never send dispatch/keyword commands.
//!
//! [`parse_event`] and [`apply`] are unit-tested; the socket plumbing is tested against a fake
//! compositor on a temp Unix socket.

use crate::{Backend, DesktopSource, DesktopState, Window, Workspace};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::ops::ControlFlow;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where one Hyprland instance listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub command: PathBuf,
    pub events: PathBuf,
}

impl Paths {
    /// Both sockets live in one directory (`$XDG_RUNTIME_DIR/hypr/$SIG`).
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            command: dir.join(".socket.sock"),
            events: dir.join(".socket2.sock"),
        }
    }

    /// From the session environment (`HYPRLAND_INSTANCE_SIGNATURE`, `XDG_RUNTIME_DIR`).
    /// Falls back to the pre-0.40 `/tmp/hypr/$SIG` location.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let sig = get("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| !s.is_empty())?;
        if let Some(rt) = get("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
            let dir = Path::new(&rt).join("hypr").join(&sig);
            if dir.is_dir() {
                return Some(Self::in_dir(&dir));
            }
        }
        Some(Self::in_dir(&Path::new("/tmp/hypr").join(sig)))
    }
}

/// One line of the event stream, reduced to the events that change [`DesktopState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Workspace {
        id: i64,
        name: String,
    },
    FocusedMonitor {
        monitor: String,
        workspace_id: i64,
    },
    /// Legacy `activewindow`: class and title; both empty = nothing focused.
    ActiveWindow {
        class: String,
        title: String,
    },
    /// `activewindowv2`: the address; `None` = nothing focused.
    ActiveWindowV2 {
        address: Option<String>,
    },
    WindowTitle {
        address: String,
        title: String,
    },
    CloseWindow {
        address: String,
    },
    Fullscreen(bool),
    FloatingMode {
        address: String,
        floating: bool,
    },
    ActiveLayout {
        keyboard: String,
        layout: String,
    },
    Submap(Option<String>),
    ActiveSpecial {
        name: Option<String>,
        monitor: String,
    },
    Screencast(bool),
    /// An output appeared or vanished (`monitoraddedv2` / `monitorremovedv2`): re-query them.
    MonitorsChanged,
    /// Workspaces or windows were created, destroyed, moved or renamed: re-query the lists.
    ListsChanged,
    /// A window wants attention (`urgent>>ADDRESS`).
    Urgent {
        address: String,
    },
    /// Config reloaded: resync from the command socket.
    ConfigReloaded,
    /// Anything we don't track.
    Other,
}

/// Split `a,b` at the first comma only: titles contain commas, the fields before them don't.
fn split1(s: &str) -> (&str, &str) {
    s.split_once(',').unwrap_or((s, ""))
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Parse one event line (without the trailing newline). Unknown or malformed events become
/// [`Event::Other`].
pub fn parse_event(line: &str) -> Event {
    let Some((name, data)) = line.split_once(">>") else {
        return Event::Other;
    };
    match name {
        "workspacev2" => {
            let (id, name) = split1(data);
            match id.parse() {
                Ok(id) => Event::Workspace {
                    id,
                    name: name.to_string(),
                },
                Err(_) => Event::Other,
            }
        }
        "focusedmonv2" => {
            let (monitor, ws) = split1(data);
            match ws.parse() {
                Ok(workspace_id) => Event::FocusedMonitor {
                    monitor: monitor.to_string(),
                    workspace_id,
                },
                Err(_) => Event::Other,
            }
        }
        "activewindow" => {
            let (class, title) = split1(data);
            Event::ActiveWindow {
                class: class.to_string(),
                title: title.to_string(),
            }
        }
        // Old versions send ",," or "" when nothing is focused.
        "activewindowv2" => Event::ActiveWindowV2 {
            address: non_empty(data.trim_start_matches("0x").trim_matches(',')),
        },
        "windowtitlev2" => {
            let (address, title) = split1(data);
            Event::WindowTitle {
                address: address.to_string(),
                title: title.to_string(),
            }
        }
        "closewindow" => Event::CloseWindow {
            address: data.to_string(),
        },
        "fullscreen" => Event::Fullscreen(data.trim() == "1"),
        "changefloatingmode" => {
            let (address, f) = split1(data);
            Event::FloatingMode {
                address: address.to_string(),
                floating: f.trim() == "1",
            }
        }
        "activelayout" => {
            // Keyboard device names are sanitized (no commas); layout names may contain them.
            let (keyboard, layout) = split1(data);
            Event::ActiveLayout {
                keyboard: keyboard.to_string(),
                layout: layout.to_string(),
            }
        }
        "submap" => Event::Submap(non_empty(data)),
        "activespecialv2" => {
            // ID,NAME,MONITOR; NAME is empty when the special workspace closes.
            let mut it = data.splitn(3, ',');
            let _id = it.next();
            let name = it.next().unwrap_or("");
            let monitor = it.next().unwrap_or("");
            Event::ActiveSpecial {
                name: non_empty(name),
                monitor: monitor.to_string(),
            }
        }
        "screencast" => Event::Screencast(data.starts_with('1')),
        "monitoraddedv2" | "monitorremovedv2" | "monitorremoved" | "monitoradded" => {
            Event::MonitorsChanged
        }
        "createworkspacev2" | "destroyworkspacev2" | "moveworkspacev2" | "renameworkspace"
        | "openwindow" | "movewindowv2" => Event::ListsChanged,
        "urgent" => Event::Urgent {
            address: data.trim_start_matches("0x").to_string(),
        },
        "configreloaded" => Event::ConfigReloaded,
        _ => Event::Other,
    }
}

/// Fold one event into the state. Returns true if the state changed.
///
/// `activewindow`/`activewindowv2` only carry class/title/address; pid, floating and xwayland
/// need a query ([`Watcher`] does it on every focus change via [`window_from_json`]).
pub fn apply(state: &mut DesktopState, event: &Event) -> bool {
    let before = state.clone();
    match event {
        Event::Workspace { id, name } if *id >= 0 => {
            state.workspace = Some(Workspace {
                id: *id,
                name: name.clone(),
            });
        }
        Event::FocusedMonitor {
            monitor,
            workspace_id,
        } => {
            state.monitor = Some(monitor.clone());
            if state.workspace.as_ref().map(|w| w.id) != Some(*workspace_id) {
                state.workspace = Some(Workspace {
                    id: *workspace_id,
                    name: workspace_id.to_string(),
                });
            }
        }
        Event::ActiveWindow { class, title } => {
            state.window = if class.is_empty() && title.is_empty() {
                None
            } else {
                Some(Window {
                    class: class.clone(),
                    title: title.clone(),
                    ..Window::default()
                })
            };
        }
        Event::ActiveWindowV2 { address: Some(a) } => {
            if let Some(w) = state.window.as_mut() {
                if w.address != *a {
                    w.address = a.clone();
                }
            }
        }
        Event::ActiveWindowV2 { address: None } => state.window = None,
        Event::WindowTitle { address, title } => {
            if let Some(w) = state.window.as_mut().filter(|w| w.address == *address) {
                w.title = title.clone();
            }
        }
        Event::CloseWindow { address } => {
            if state.window.as_ref().is_some_and(|w| w.address == *address) {
                state.window = None;
            }
        }
        Event::Fullscreen(f) => {
            if let Some(w) = state.window.as_mut() {
                w.fullscreen = *f;
            }
        }
        Event::FloatingMode { address, floating } => {
            if let Some(w) = state.window.as_mut().filter(|w| w.address == *address) {
                w.floating = *floating;
            }
        }
        Event::ActiveLayout { layout, .. } => state.keyboard_layout = Some(layout.clone()),
        Event::Submap(s) => state.submap = s.clone(),
        Event::ActiveSpecial { name, monitor } => {
            if state.monitor.as_deref().is_none_or(|m| m == monitor) {
                state.special_workspace = name.clone();
            }
        }
        Event::Screencast(on) => state.screencast = *on,
        Event::Workspace { .. }
        | Event::MonitorsChanged
        | Event::ListsChanged
        | Event::Urgent { .. }
        | Event::ConfigReloaded
        | Event::Other => {}
    }
    *state != before
}

/// Parse `j/activewindow`. Returns `None` for `{}` (nothing focused) and for garbage.
pub fn window_from_json(json: &str) -> Option<Window> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let class = v.get("class")?.as_str()?.to_string();
    Some(Window {
        address: v
            .get("address")?
            .as_str()?
            .trim_start_matches("0x")
            .to_string(),
        class,
        title: v
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        pid: v
            .get("pid")
            .and_then(|p| p.as_u64())
            .and_then(|p| u32::try_from(p).ok()),
        floating: v.get("floating").and_then(|b| b.as_bool()).unwrap_or(false),
        fullscreen: v.get("fullscreen").and_then(|n| n.as_i64()) == Some(2),
        xwayland: v.get("xwayland").and_then(|b| b.as_bool()).unwrap_or(false),
    })
}

/// Parse `j/activeworkspace` into (workspace, monitor name).
pub fn workspace_from_json(json: &str) -> Option<(Workspace, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let ws = Workspace {
        id: v.get("id")?.as_i64()?,
        name: v.get("name")?.as_str()?.to_string(),
    };
    Some((
        ws,
        v.get("monitor")
            .and_then(|m| m.as_str())
            .map(str::to_string),
    ))
}

/// Parse `j/monitors` into output names (enabled ones; `j/monitors` omits disabled outputs).
pub fn monitors_from_json(json: &str) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut names: Vec<String> = v
        .as_array()?
        .iter()
        .filter(|m| m.get("disabled").and_then(|d| d.as_bool()) != Some(true))
        .filter_map(|m| m.get("name")?.as_str().map(str::to_string))
        .collect();
    names.sort();
    Some(names)
}

/// Parse `j/workspaces`: real workspaces (special/scratchpad ones have negative ids), by id.
pub fn workspaces_from_json(json: &str) -> Option<Vec<crate::WorkspaceInfo>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut out: Vec<crate::WorkspaceInfo> = v
        .as_array()?
        .iter()
        .filter_map(|w| {
            let id = w.get("id")?.as_i64()?;
            if id < 0 {
                return None;
            }
            Some(crate::WorkspaceInfo {
                id,
                name: w.get("name")?.as_str()?.to_string(),
                active: false,
                urgent: false,
                monitor: w
                    .get("monitor")
                    .and_then(|m| m.as_str())
                    .map(str::to_string),
                windows: w.get("windows").and_then(|n| n.as_u64()).unwrap_or(0) as u32,
            })
        })
        .collect();
    out.sort_by_key(|w| w.id);
    Some(out)
}

/// Parse `j/clients`: mapped windows.
pub fn windows_from_json(json: &str) -> Option<Vec<crate::WindowInfo>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(
        v.as_array()?
            .iter()
            .filter(|c| c.get("mapped").and_then(|m| m.as_bool()) != Some(false))
            .filter_map(|c| {
                Some(crate::WindowInfo {
                    address: c
                        .get("address")?
                        .as_str()?
                        .trim_start_matches("0x")
                        .to_string(),
                    class: c
                        .get("class")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    title: c
                        .get("title")
                        .and_then(|s| s.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    workspace: c.get("workspace")?.get("id")?.as_i64()?,
                    focused: false,
                    urgent: false,
                    pid: c
                        .get("pid")
                        .and_then(|p| p.as_u64())
                        .and_then(|p| u32::try_from(p).ok()),
                })
            })
            .collect(),
    )
}

/// Parse `j/devices`: the active layout of the main keyboard, else of the first physical one.
/// (On Apple laptops no keyboard is flagged `main`, and virtual ones are listed first.)
pub fn layout_from_devices_json(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let kbs = v.get("keyboards")?.as_array()?;
    let name = |k: &serde_json::Value| {
        k.get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string()
    };
    let virt = |k: &serde_json::Value| {
        let n = name(k);
        n.starts_with("hl-virtual") || n.contains("virtual") || n.ends_with("lid-events")
    };
    let pick = kbs
        .iter()
        .find(|k| k.get("main").and_then(|m| m.as_bool()) == Some(true))
        .or_else(|| kbs.iter().find(|k| !virt(k)))
        .or_else(|| kbs.first())?;
    pick.get("active_keymap")?.as_str().map(str::to_string)
}

/// The keymap Hyprland applies to sliverd's virtual keyboard (it lowercases and dashes device
/// names: `sliver-touch-bar-keys`). Empty rules/model mean Hyprland's defaults.
pub fn bar_keymap_from_devices_json(json: &str) -> Option<crate::XkbNames> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let want = crate::BAR_KEYBOARD.to_lowercase().replace(' ', "-");
    let k = v
        .get("keyboards")?
        .as_array()?
        .iter()
        .find(|k| k.get("name").and_then(|n| n.as_str()) == Some(want.as_str()))?;
    let s = |f: &str| k.get(f).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(crate::XkbNames {
        rules: s("rules"),
        model: s("model"),
        layout: s("layout"),
        variant: s("variant"),
        options: s("options"),
        index: k
            .get("active_layout_index")
            .and_then(|i| i.as_u64())
            .unwrap_or(0) as u32,
    })
}

/// The layouts configured on the same keyboard, as `us`, `de(nodeadkeys)` (Hyprland only gives
/// the comma-separated `layout` and `variant` options, not the long names, and not which one is
/// active, so the picker labels by code).
pub fn layouts_from_devices_json(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(kbs) = v.get("keyboards").and_then(|k| k.as_array()) else {
        return Vec::new();
    };
    let virt = |k: &serde_json::Value| {
        let n = k.get("name").and_then(|n| n.as_str()).unwrap_or("");
        n.starts_with("hl-virtual") || n.contains("virtual") || n.ends_with("lid-events")
    };
    let Some(pick) = kbs
        .iter()
        .find(|k| k.get("main").and_then(|m| m.as_bool()) == Some(true))
        .or_else(|| kbs.iter().find(|k| !virt(k)))
        .or_else(|| kbs.first())
    else {
        return Vec::new();
    };
    let field = |name: &str| {
        pick.get(name)
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .collect::<Vec<_>>()
    };
    let (layouts, variants) = (field("layout"), field("variant"));
    layouts
        .into_iter()
        .enumerate()
        .filter(|(_, l)| !l.is_empty())
        .map(|(i, l)| match variants.get(i).filter(|v| !v.is_empty()) {
            Some(v) => format!("{l}({v})"),
            None => l,
        })
        .collect()
}

/// One request/response on the command socket.
pub fn request(path: &Path, command: &str) -> io::Result<String> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(command.as_bytes())?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// A live connection: event stream plus the folded state.
pub struct Watcher {
    paths: Paths,
    events: BufReader<UnixStream>,
    state: DesktopState,
    /// Windows that asked for attention and have not been focused since.
    urgent: std::collections::HashSet<String>,
}

impl Watcher {
    /// Subscribe to events first, then snapshot, so nothing between the two is lost.
    pub fn connect(paths: &Paths) -> io::Result<Self> {
        let events = BufReader::new(UnixStream::connect(&paths.events)?);
        let mut w = Self {
            paths: paths.clone(),
            events,
            state: DesktopState::default(),
            urgent: Default::default(),
        };
        w.resync();
        Ok(w)
    }

    /// Re-read everything from the command socket. Failures keep the previous value.
    fn resync(&mut self) {
        self.state.connected = true;
        self.state.backend = Backend::Hyprland;
        self.refresh_window();
        if let Ok(j) = request(&self.paths.command, "j/activeworkspace") {
            if let Some((ws, mon)) = workspace_from_json(&j) {
                self.state.workspace = Some(ws);
                self.state.monitor = mon.or(self.state.monitor.take());
            }
        }
        self.refresh_monitors();
        self.refresh_lists();
        if let Ok(j) = request(&self.paths.command, "j/devices") {
            if let Some(l) = layout_from_devices_json(&j) {
                self.state.keyboard_layout = Some(l);
            }
            self.state.keyboard_layouts = layouts_from_devices_json(&j);
            self.state.bar_keymap = bar_keymap_from_devices_json(&j);
        }
        if let Ok(s) = request(&self.paths.command, "submap") {
            let s = s.trim();
            self.state.submap = (s != "default" && !s.is_empty()).then(|| s.to_string());
        }
    }

    /// Re-read the workspace and window lists (taskbar, workspaces bar).
    fn refresh_lists(&mut self) {
        if let Ok(j) = request(&self.paths.command, "j/workspaces") {
            if let Some(w) = workspaces_from_json(&j) {
                self.state.workspaces = w;
            }
        }
        if let Ok(j) = request(&self.paths.command, "j/clients") {
            if let Some(w) = windows_from_json(&j) {
                self.state.windows = w;
            }
        }
        self.mark_urgent();
    }

    /// Fold the urgent set and the focus into the lists.
    fn mark_urgent(&mut self) {
        if let Some(w) = &self.state.window {
            self.urgent.remove(&w.address);
        }
        let live: std::collections::HashSet<&str> = self
            .state
            .windows
            .iter()
            .map(|w| w.address.as_str())
            .collect();
        self.urgent.retain(|a| live.contains(a.as_str()));
        for w in &mut self.state.windows {
            w.urgent = self.urgent.contains(&w.address);
        }
        let urgent_ws: std::collections::HashSet<i64> = self
            .state
            .windows
            .iter()
            .filter(|w| w.urgent)
            .map(|w| w.workspace)
            .collect();
        for ws in &mut self.state.workspaces {
            ws.urgent = urgent_ws.contains(&ws.id);
        }
        self.state.sync_flags();
    }

    fn refresh_monitors(&mut self) {
        if let Ok(j) = request(&self.paths.command, "j/monitors") {
            if let Some(names) = monitors_from_json(&j) {
                self.state.set_monitors(names);
            }
        }
    }

    /// Replace the focused window with the full query result (adds pid/floating/xwayland).
    fn refresh_window(&mut self) {
        if let Ok(j) = request(&self.paths.command, "j/activewindow") {
            self.state.window = window_from_json(&j);
        }
    }

    fn read_event(&mut self) -> io::Result<Option<Event>> {
        let mut buf = Vec::new();
        if self.events.read_until(b'\n', &mut buf)? == 0 {
            return Ok(None);
        }
        let line = String::from_utf8_lossy(&buf);
        Ok(Some(parse_event(line.trim_end_matches(['\n', '\r']))))
    }
}

impl DesktopSource for Watcher {
    fn state(&self) -> &DesktopState {
        &self.state
    }

    /// Sleeps in `read(2)` until Hyprland sends something; only returns on a real change.
    fn next_change(&mut self) -> io::Result<Option<DesktopState>> {
        loop {
            let Some(ev) = self.read_event()? else {
                self.state = DesktopState::default();
                return Ok(None);
            };
            let mut changed = false;
            match ev {
                // Legacy `activewindow` is always followed by `activewindowv2`; acting on both
                // would emit a half-filled window first. The query has everything.
                Event::ActiveWindow { .. } => {}
                Event::ActiveWindowV2 { address: Some(_) } => {
                    let before = self.state.clone();
                    self.refresh_window();
                    // The window list's `focused` flags (the taskbar) and urgency follow.
                    self.mark_urgent();
                    changed = self.state != before;
                }
                // Our virtual keyboard switched layout: re-read its keymap (combos follow it).
                Event::ActiveLayout { ref keyboard, .. }
                    if *keyboard == crate::BAR_KEYBOARD.to_lowercase().replace(' ', "-") =>
                {
                    let before = self.state.clone();
                    if let Ok(j) = request(&self.paths.command, "j/devices") {
                        self.state.bar_keymap = bar_keymap_from_devices_json(&j);
                    }
                    changed = self.state != before;
                }
                Event::MonitorsChanged => {
                    let before = self.state.clone();
                    self.refresh_monitors();
                    changed = self.state != before;
                }
                Event::ConfigReloaded => {
                    let before = self.state.clone();
                    self.resync();
                    changed |= self.state != before;
                }
                Event::Urgent { ref address } => {
                    let before = self.state.clone();
                    self.urgent.insert(address.clone());
                    self.mark_urgent();
                    changed = self.state != before;
                }
                ref other => {
                    let before = self.state.clone();
                    apply(&mut self.state, other);
                    if matches!(
                        other,
                        Event::ListsChanged | Event::CloseWindow { .. } | Event::WindowTitle { .. }
                    ) {
                        self.refresh_lists();
                    } else {
                        self.mark_urgent();
                    }
                    changed = self.state != before;
                }
            }
            if changed {
                return Ok(Some(self.state.clone()));
            }
        }
    }
}

/// [`crate::watch`] for Hyprland: connect, deliver the initial state and every change, retry
/// with backoff.
pub fn watch(paths: &Paths, on_change: impl FnMut(&DesktopState) -> ControlFlow<()>) {
    crate::watch(|| Watcher::connect(paths), on_change);
}

#[cfg(test)]
mod tests;
