//! `Desktop1`: what a desktop integration (GNOME, KDE) reports when the agent has no
//! compositor backend of its own.

use super::*;

pub(super) struct Desktop {
    pub(super) inner: Arc<Inner>,
}

#[interface(name = "io.github.wawow830.Sliver.Desktop1")]
impl Desktop {
    async fn set_focus(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        window: HashMap<String, OwnedValue>,
    ) -> Result<()> {
        let sender = self.inner.admit_unsandboxed(conn, &hdr).await?;
        let p = to_props(&window);
        {
            let mut s = self.inner.lock();
            if s.focus_source == FocusSource::Backend {
                return Err(Error::Busy("the agent tracks focus itself".into()));
            }
            match &s.focus_owner {
                Some(o) if *o != sender => {
                    return Err(Error::Busy(format!("{o} is the focus source")));
                }
                _ => s.focus_owner = Some(sender),
            }
            s.focus.window = focus_window(&p);
            if let Some(Prop::Str(ws)) = p.get("workspace") {
                s.focus.workspace = Some(ws.clone());
            }
        }
        self.inner.host.changed();
        Ok(())
    }

    async fn set_workspaces(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        workspaces: Vec<HashMap<String, OwnedValue>>,
    ) -> Result<()> {
        self.inner.admit_unsandboxed(conn, &hdr).await?;
        self.inner.lock().workspaces = workspaces.iter().map(to_props).take(64).collect();
        self.inner.host.changed();
        Ok(())
    }

    /// `SetWorkspaces` for callers that can only send `av` (KWin scripts' `callDBus` turns JS
    /// arrays into `av`). Each element must be an `a{sv}`.
    async fn set_workspaces_v(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        workspaces: Vec<OwnedValue>,
    ) -> Result<()> {
        self.inner.admit_unsandboxed(conn, &hdr).await?;
        let list: Option<Vec<Props>> = workspaces
            .iter()
            .take(64)
            .map(|v| match prop_from_value(v) {
                Some(Prop::Map(m)) => Some(m),
                _ => None,
            })
            .collect();
        let list =
            list.ok_or_else(|| Error::InvalidArgs("every element must be an a{sv}".into()))?;
        self.inner.lock().workspaces = list;
        self.inner.host.changed();
        Ok(())
    }

    /// Every window, for the taskbar module (GNOME/KDE; keys as in `SetFocus`, plus `workspace`,
    /// `focused`, `urgent`). Replaces the previous list.
    async fn set_windows(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        windows: Vec<HashMap<String, OwnedValue>>,
    ) -> Result<()> {
        self.inner.admit_unsandboxed(conn, &hdr).await?;
        self.inner.lock().windows = windows.iter().map(to_props).take(MAX_WINDOWS).collect();
        self.inner.host.changed();
        Ok(())
    }

    /// `SetWindows` for KWin scripts (`av`, each element an `a{sv}`).
    async fn set_windows_v(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        windows: Vec<OwnedValue>,
    ) -> Result<()> {
        self.inner.admit_unsandboxed(conn, &hdr).await?;
        let list: Option<Vec<Props>> = windows
            .iter()
            .take(MAX_WINDOWS)
            .map(|v| match prop_from_value(v) {
                Some(Prop::Map(m)) => Some(m),
                _ => None,
            })
            .collect();
        let list =
            list.ok_or_else(|| Error::InvalidArgs("every element must be an a{sv}".into()))?;
        self.inner.lock().windows = list;
        self.inner.host.changed();
        Ok(())
    }
}

/// `SetWindows` keeps at most this many windows.
const MAX_WINDOWS: usize = 256;

/// A workspace id as `SetWorkspaces` sends it (`"3"`; a name-like id gets its position).
pub(super) fn workspace_id(p: &Props, index: usize) -> i64 {
    match p.get("id") {
        Some(Prop::Str(s)) => s.parse().unwrap_or(index as i64 + 1),
        Some(Prop::Int(i)) => *i,
        _ => index as i64 + 1,
    }
}

/// What a DE integration pushed over `Desktop1` (focus, workspaces, windows), as the desktop
/// state the agent's own backends produce. `None` while nothing was pushed or the agent tracks
/// focus itself.
pub fn pushed_desktop(s: &Shared, backend: sliver_desktop::Backend) -> Option<DesktopState> {
    if s.focus_source == FocusSource::Backend
        || (s.focus.window.is_none() && s.workspaces.is_empty() && s.windows.is_empty())
    {
        return None;
    }
    let st = |p: &Props, k: &str| match p.get(k) {
        Some(Prop::Str(v)) => v.clone(),
        _ => String::new(),
    };
    let flag = |p: &Props, k: &str| matches!(p.get(k), Some(Prop::Bool(true)));
    let mut windows: Vec<WindowInfo> = s
        .windows
        .iter()
        .filter_map(|p| {
            let w = focus_window(p)?;
            Some(WindowInfo {
                workspace: match p.get("workspace") {
                    Some(Prop::Str(v)) => v.parse().unwrap_or(0),
                    Some(Prop::Int(i)) => *i,
                    _ => 0,
                },
                focused: flag(p, "focused"),
                urgent: flag(p, "urgent"),
                pid: w.pid,
                address: w.address,
                class: w.class,
                title: w.title,
            })
        })
        .collect();
    let workspaces: Vec<WorkspaceInfo> = s
        .workspaces
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let id = workspace_id(p, i);
            WorkspaceInfo {
                id,
                name: Some(st(p, "name"))
                    .filter(|n| !n.is_empty())
                    .unwrap_or(id.to_string()),
                active: flag(p, "active"),
                urgent: flag(p, "urgent"),
                monitor: Some(st(p, "monitor")).filter(|m| !m.is_empty()),
                windows: windows.iter().filter(|w| w.workspace == id).count() as u32,
            }
        })
        .collect();
    let workspace = workspaces
        .iter()
        .find(|w| w.active)
        .map(|w| Workspace {
            id: w.id,
            name: w.name.clone(),
        })
        .or_else(|| {
            let name = s.focus.workspace.clone()?;
            Some(Workspace {
                id: name.parse().unwrap_or(0),
                name,
            })
        });
    let window = s.focus.window.clone();
    // A window list without a focused flag: the focused window is the one SetFocus named.
    if !windows.iter().any(|w| w.focused)
        && let Some(f) = &window
    {
        windows
            .iter_mut()
            .filter(|w| w.address == f.address)
            .for_each(|w| w.focused = true);
    }
    Some(DesktopState {
        connected: true,
        backend,
        window,
        workspace,
        workspaces,
        windows,
        ..DesktopState::default()
    })
}

/// `SetFocus` keys → a window. Empty dict = nothing focused.
pub(super) fn focus_window(p: &Props) -> Option<Window> {
    if p.is_empty() {
        return None;
    }
    let st = |k: &str| match p.get(k) {
        Some(Prop::Str(s)) => s.clone(),
        _ => String::new(),
    };
    let flag = |k: &str| matches!(p.get(k), Some(Prop::Bool(true)));
    let handle = st("window");
    Some(Window {
        address: handle
            .strip_prefix("hyprland:")
            .unwrap_or(&handle)
            .trim_start_matches("0x")
            .to_owned(),
        class: st("app-id"),
        title: st("title"),
        pid: match p.get("pid") {
            Some(Prop::Int(i)) => u32::try_from(*i).ok(),
            _ => None,
        },
        floating: flag("floating"),
        fullscreen: flag("fullscreen")
            || matches!(p.get("fullscreen"), Some(Prop::Int(i)) if *i > 0),
        xwayland: flag("xwayland"),
    })
}
