//! Generic Wayland backend for compositors without their own IPC (river, labwc, wayfire, and
//! anything else wlroots-like): the focused window and the window list from
//! `wlr-foreign-toplevel-management-v1`, workspaces from `ext-workspace-v1` when the compositor
//! has it. Hyprland, sway and niri keep their own, richer backends.
//!
//! One thread owns the Wayland connection and sleeps in `poll(2)` on the connection and on a
//! wake pipe. Actions (focus, close, switch workspace) are requests on the same connection, so
//! [`control::run`](crate::control::run) hands them to that thread through [`send`]: the
//! protocols give no ids that mean anything to a second connection, so ids here are numbers this
//! module hands out (stable while the window or workspace lives).
//!
//! The pure part ([`Model`]) is unit-tested; the Wayland glue is exercised by
//! `integration/wlroots/test.sh` against a nested, headless Hyprland.

use crate::control::Action;
use crate::{Backend, DesktopSource, DesktopState, Window, WindowInfo, WorkspaceInfo};
use std::collections::HashMap;
use std::io;
use std::ops::ControlFlow;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

// ---------------------------------------------------------------------------------------------
// The model: plain data, no Wayland objects.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Top {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub fullscreen: bool,
    pub minimized: bool,
    /// Set by the `done` event: the properties above are consistent and may be shown.
    pub ready: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ws {
    pub id: u64,
    pub name: String,
    pub active: bool,
    pub urgent: bool,
    pub hidden: bool,
    pub ready: bool,
}

#[derive(Debug, Default)]
pub struct Model {
    /// Windows in the order they appeared.
    pub tops: Vec<Top>,
    pub workspaces: Vec<Ws>,
}

/// `zwlr_foreign_toplevel_handle_v1.state` is an array of u32 (native endian).
pub fn parse_state(bytes: &[u8]) -> (bool, bool, bool) {
    let (mut activated, mut fullscreen, mut minimized) = (false, false, false);
    for c in bytes.chunks_exact(4) {
        match u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) {
            1 => minimized = true,
            2 => activated = true,
            3 => fullscreen = true,
            _ => {}
        }
    }
    (activated, fullscreen, minimized)
}

impl Model {
    pub fn top_mut(&mut self, id: u64) -> Option<&mut Top> {
        self.tops.iter_mut().find(|t| t.id == id)
    }

    pub fn ws_mut(&mut self, id: u64) -> Option<&mut Ws> {
        self.workspaces.iter_mut().find(|w| w.id == id)
    }

    pub fn to_state(&self) -> DesktopState {
        let active_ws = self
            .workspaces
            .iter()
            .find(|w| w.ready && w.active && !w.hidden);
        let ws_id = active_ws.map_or(0, |w| w.id as i64);
        let shown: Vec<&Top> = self.tops.iter().filter(|t| t.ready).collect();
        let focused = shown.iter().find(|t| t.activated);
        let mut st = DesktopState {
            connected: true,
            backend: Backend::Wlroots,
            ..Default::default()
        };
        st.window = focused.map(|t| Window {
            address: t.id.to_string(),
            class: t.app_id.clone(),
            title: t.title.clone(),
            pid: None,
            floating: false,
            fullscreen: t.fullscreen,
            xwayland: false,
        });
        st.workspace = active_ws.map(|w| crate::Workspace {
            id: w.id as i64,
            name: w.name.clone(),
        });
        // The protocols don't say which workspace a window is on: show them all on the active one.
        st.windows = shown
            .iter()
            .map(|t| WindowInfo {
                address: t.id.to_string(),
                class: t.app_id.clone(),
                title: t.title.clone(),
                workspace: ws_id,
                focused: t.activated,
                urgent: false,
                pid: None,
            })
            .collect();
        let count = st.windows.len() as u32;
        st.workspaces = self
            .workspaces
            .iter()
            .filter(|w| w.ready && !w.hidden)
            .map(|w| WorkspaceInfo {
                id: w.id as i64,
                name: if w.name.is_empty() {
                    w.id.to_string()
                } else {
                    w.name.clone()
                },
                active: w.active,
                urgent: w.urgent,
                monitor: None,
                windows: if w.active { count } else { 0 },
            })
            .collect();
        st.sync_flags();
        st
    }
}

// ---------------------------------------------------------------------------------------------
// Control: actions reach the watcher thread through here.

type Command = Action;

struct Control {
    tx: Sender<Command>,
    wake: UnixStream,
}

static CONTROL: OnceLock<Mutex<Option<Control>>> = OnceLock::new();

fn control_slot() -> &'static Mutex<Option<Control>> {
    CONTROL.get_or_init(|| Mutex::new(None))
}

/// Hand an action to the running watcher. `NotConnected` when there is none.
pub fn send(a: &Action) -> io::Result<()> {
    use std::io::Write;
    let guard = control_slot().lock().unwrap_or_else(|e| e.into_inner());
    let c = guard
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no Wayland desktop watcher"))?;
    c.tx.send(a.clone())
        .map_err(|_| io::Error::new(io::ErrorKind::NotConnected, "the desktop watcher is gone"))?;
    (&c.wake).write_all(&[1])
}

// ---------------------------------------------------------------------------------------------
// Wayland glue.

struct App {
    model: Model,
    next_id: u64,
    changed: bool,
    seat: Option<wl_seat::WlSeat>,
    toplevel_handles: HashMap<u32, (u64, ZwlrForeignToplevelHandleV1)>,
    ws_handles: HashMap<u32, (u64, ExtWorkspaceHandleV1)>,
    ws_manager: Option<ExtWorkspaceManagerV1>,
    /// Kept alive: dropping the manager would not stop the events, but this is the owner.
    _toplevel_manager: Option<ZwlrForeignToplevelManagerV1>,
}

impl App {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            let id = app.id();
            app.model.tops.push(Top {
                id,
                ..Default::default()
            });
            app.toplevel_handles
                .insert(toplevel.id().protocol_id(), (id, toplevel));
        }
    }

    event_created_child!(App, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for App {
    fn event(
        app: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event as E;
        let key = handle.id().protocol_id();
        let Some(&(id, _)) = app.toplevel_handles.get(&key) else {
            return;
        };
        match event {
            E::Title { title } => {
                if let Some(t) = app.model.top_mut(id) {
                    t.title = title;
                }
            }
            E::AppId { app_id } => {
                if let Some(t) = app.model.top_mut(id) {
                    t.app_id = app_id;
                }
            }
            E::State { state } => {
                let (a, f, m) = parse_state(&state);
                if let Some(t) = app.model.top_mut(id) {
                    (t.activated, t.fullscreen, t.minimized) = (a, f, m);
                }
            }
            E::Done => {
                if let Some(t) = app.model.top_mut(id) {
                    t.ready = true;
                }
                app.changed = true;
            }
            E::Closed => {
                app.model.tops.retain(|t| t.id != id);
                if let Some((_, h)) = app.toplevel_handles.remove(&key) {
                    h.destroy();
                }
                app.changed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_workspace_manager_v1::Event as E;
        match event {
            E::Workspace { workspace } => {
                let id = app.id();
                app.model.workspaces.push(Ws {
                    id,
                    ..Default::default()
                });
                app.ws_handles
                    .insert(workspace.id().protocol_id(), (id, workspace));
            }
            E::Done => {
                for w in &mut app.model.workspaces {
                    w.ready = true;
                }
                app.changed = true;
            }
            _ => {}
        }
    }

    event_created_child!(App, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ExtWorkspaceGroupHandleV1,
        _: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for App {
    fn event(
        app: &mut Self,
        handle: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_workspace_handle_v1::Event as E;
        let key = handle.id().protocol_id();
        let Some(&(id, _)) = app.ws_handles.get(&key) else {
            return;
        };
        match event {
            E::Name { name } => {
                if let Some(w) = app.model.ws_mut(id) {
                    w.name = name;
                }
            }
            E::State { state } => {
                let bits = state.into_result().map(|s| s.bits()).unwrap_or(0);
                if let Some(w) = app.model.ws_mut(id) {
                    w.active = bits & 1 != 0;
                    w.urgent = bits & 2 != 0;
                    w.hidden = bits & 4 != 0;
                }
            }
            E::Removed => {
                app.model.workspaces.retain(|w| w.id != id);
                if let Some((_, h)) = app.ws_handles.remove(&key) {
                    h.destroy();
                }
                app.changed = true;
            }
            _ => {}
        }
    }
}

/// A connection to the compositor and the model it keeps up to date.
pub struct Watcher {
    conn: Connection,
    queue: EventQueue<App>,
    app: App,
    state: DesktopState,
    wake: UnixStream,
    commands: Receiver<Command>,
}

impl Watcher {
    /// Connect through `WAYLAND_DISPLAY`. `ErrorKind::Unsupported` when the compositor has no
    /// `wlr-foreign-toplevel-management`: the caller should not retry.
    pub fn connect() -> io::Result<Watcher> {
        let conn = Connection::connect_to_env().map_err(io::Error::other)?;
        let (globals, mut queue) = registry_queue_init::<App>(&conn).map_err(io::Error::other)?;
        let qh = queue.handle();
        let toplevel_manager = globals
            .bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ())
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the compositor has no wlr-foreign-toplevel-management",
                )
            })?;
        // Workspaces are optional.
        let ws_manager = globals
            .bind::<ExtWorkspaceManagerV1, _, _>(&qh, 1..=1, ())
            .ok();
        let seat = globals.bind::<wl_seat::WlSeat, _, _>(&qh, 1..=1, ()).ok();
        let mut app = App {
            model: Model::default(),
            next_id: 0,
            changed: false,
            seat,
            toplevel_handles: HashMap::new(),
            ws_handles: HashMap::new(),
            ws_manager,
            _toplevel_manager: Some(toplevel_manager),
        };
        // The initial burst: toplevels, then their properties.
        queue.roundtrip(&mut app).map_err(io::Error::other)?;
        queue.roundtrip(&mut app).map_err(io::Error::other)?;
        let (wake_tx, wake_rx) = UnixStream::pair()?;
        wake_rx.set_nonblocking(true)?;
        let (tx, commands) = channel();
        *control_slot().lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Control { tx, wake: wake_tx });
        let state = app.model.to_state();
        Ok(Watcher {
            conn,
            queue,
            app,
            state,
            wake: wake_rx,
            commands,
        })
    }

    fn run_command(&mut self, a: &Action) {
        let id_of = |s: &str| s.parse::<u64>().ok();
        match a {
            Action::FocusWindow { address } => {
                let target = id_of(address);
                if let (Some(id), Some(seat)) = (target, &self.app.seat)
                    && let Some((_, h)) = self.app.toplevel_handles.values().find(|(i, _)| *i == id)
                {
                    h.unset_minimized();
                    h.activate(seat);
                }
            }
            Action::CloseWindow { address } => {
                if let Some(id) = id_of(address)
                    && let Some((_, h)) = self.app.toplevel_handles.values().find(|(i, _)| *i == id)
                {
                    h.close();
                }
            }
            Action::FocusWorkspace(w) => {
                if let (Ok(id), Some(m)) = (u64::try_from(w.id), &self.app.ws_manager)
                    && let Some((_, h)) = self.app.ws_handles.values().find(|(i, _)| *i == id)
                {
                    h.activate();
                    m.commit();
                }
            }
            // Layout switching has no protocol here.
            Action::NextLayout | Action::SetLayout(_) => {}
        }
        let _ = self.conn.flush();
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        *control_slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl DesktopSource for Watcher {
    fn state(&self) -> &DesktopState {
        &self.state
    }

    fn next_change(&mut self) -> io::Result<Option<DesktopState>> {
        use rustix::event::{PollFd, PollFlags, poll};
        use std::io::Read;
        loop {
            self.queue
                .dispatch_pending(&mut self.app)
                .map_err(io::Error::other)?;
            if self.app.changed {
                self.app.changed = false;
                let st = self.app.model.to_state();
                if st != self.state {
                    self.state = st.clone();
                    return Ok(Some(st));
                }
            }
            self.conn.flush().map_err(io::Error::other)?;
            let Some(guard) = self.queue.prepare_read() else {
                continue; // events arrived meanwhile
            };
            let wl_fd: OwnedFd = guard.connection_fd().try_clone_to_owned()?;
            let mut fds = [
                PollFd::new(&wl_fd, PollFlags::IN),
                PollFd::new(&self.wake, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
            let wl_ready = !fds[0].revents().is_empty();
            let wake_ready = !fds[1].revents().is_empty();
            if wl_ready {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => return Ok(None), // compositor went away
                }
            } else {
                drop(guard);
            }
            if wake_ready {
                let mut buf = [0u8; 64];
                while matches!((&self.wake).read(&mut buf), Ok(n) if n > 0) {}
                while let Ok(a) = self.commands.try_recv() {
                    self.run_command(&a);
                }
            }
        }
    }
}

/// [`crate::watch`] for generic Wayland compositors. Returns `Unsupported` right away when the
/// compositor lacks the protocol (instead of retrying forever).
pub fn watch(on_change: impl FnMut(&DesktopState) -> ControlFlow<()>) -> io::Result<()> {
    let mut first = Some(Watcher::connect()?);
    crate::watch(
        move || match first.take() {
            Some(w) => Ok(w),
            None => Watcher::connect(),
        },
        on_change,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(id: u64, app: &str, title: &str, activated: bool) -> Top {
        Top {
            id,
            app_id: app.into(),
            title: title.into(),
            activated,
            ready: true,
            ..Default::default()
        }
    }

    #[test]
    fn state_array_bits() {
        let bytes: Vec<u8> = [2u32, 3].iter().flat_map(|n| n.to_ne_bytes()).collect();
        assert_eq!(parse_state(&bytes), (true, true, false));
        assert_eq!(parse_state(&1u32.to_ne_bytes()), (false, false, true));
        assert_eq!(parse_state(&[]), (false, false, false));
        assert_eq!(
            parse_state(&[1, 2, 3]),
            (false, false, false),
            "ragged tail ignored"
        );
    }

    fn ws(id: u64, name: &str, active: bool, hidden: bool, ready: bool) -> Ws {
        Ws {
            id,
            name: name.into(),
            active,
            hidden,
            ready,
            ..Default::default()
        }
    }

    #[test]
    fn model_derives_windows_focus_and_workspaces() {
        let m = Model {
            tops: vec![
                top(1, "foot", "shell", true),
                top(2, "firefox", "docs", false),
                // no `done` yet
                Top {
                    id: 3,
                    app_id: "half".into(),
                    ..Default::default()
                },
            ],
            workspaces: vec![
                ws(10, "1", true, false, true),
                ws(11, "2", false, false, true),
                ws(12, "gone", false, true, true),
                ws(13, "new", false, false, false),
            ],
        };
        let s = m.to_state();
        assert_eq!(s.backend, Backend::Wlroots);
        let w = s.window.as_ref().unwrap();
        assert_eq!(
            (w.class.as_str(), w.title.as_str(), w.address.as_str()),
            ("foot", "shell", "1")
        );
        assert_eq!(s.windows.len(), 2, "a window without `done` is not shown");
        assert!(s.windows[0].focused && !s.windows[1].focused);
        assert_eq!(
            s.windows[1].workspace, 10,
            "all windows sit on the active workspace"
        );
        assert_eq!(
            s.workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
        assert!(
            s.workspaces[0].active && s.workspaces[0].windows == 2 && s.workspaces[1].windows == 0
        );
        assert_eq!(s.workspace.as_ref().unwrap().name, "1");
        // nothing focused, no workspaces
        let m = Model {
            tops: vec![top(1, "a", "b", false)],
            workspaces: vec![],
        };
        let s = m.to_state();
        assert!(s.window.is_none() && s.workspace.is_none() && s.workspaces.is_empty());
        assert_eq!(s.windows[0].workspace, 0);
    }

    #[test]
    fn control_without_a_watcher_is_an_error() {
        let e = send(&Action::NextLayout).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotConnected);
    }
}
