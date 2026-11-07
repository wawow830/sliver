//! The zbus layer: `TouchBar1`, `Context1`, `Control1` and `Desktop1` on the session bus.
//!
//! Calls go to [`Registry`] methods or to the agent core through [`Host`]; the core drives the
//! other direction through [`Api`].

use crate::identity::ProcInfo;
use crate::item::{Gesture, Prop, Props};
use crate::registry::{
    ApiError, ContextId, Focus, Interaction, Outgoing, Policy, Registry, Resolved, Scope,
};
use sliver_desktop::{DesktopState, Window, WindowInfo, Workspace, WorkspaceInfo};
use sliver_ipc::dbus::{API_MINOR, BUS_NAME, ROOT_PATH};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{Array, Dict, OwnedObjectPath, OwnedValue, Signature, Str, Value};
use zbus::{Connection, interface, names::BusName};

/// D-Bus errors. Names are checked against `sliver_ipc::dbus::error` in tests.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.wawow830.Sliver.Error")]
pub enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    AccessDenied(String),
    LimitExceeded(String),
    InvalidArgs(String),
    Busy(String),
    Failed(String),
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        match e {
            ApiError::AccessDenied(m) => Error::AccessDenied(m),
            ApiError::LimitExceeded(m) => Error::LimitExceeded(m),
            ApiError::InvalidArgs(m) => Error::InvalidArgs(m),
            ApiError::Busy(m) => Error::Busy(m),
            ApiError::UnknownContext => Error::AccessDenied("no such context".into()),
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Requests the agent core answers (`Control1`). Module ids follow sliver-config(5).
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    SwitchLayer(String),
    PushLayer(String, u32),
    PopLayer,
    ToggleLayer(String),
    SetLayout(String),
    SetProfile(String),
    ReloadConfig,
    RenderPreview(String, Props),
    SetVariable(String, Prop),
    Signal(u32),
    Refresh(String),
    Show(String),
    Hide(String),
    Toggle(String),
    ResetVisibility,
    Trigger(String, Gesture),
    Update(String, Props),
    ListModules,
    /// `show`, `hide` or `toggle` the whole bar (the hatch keeps working).
    Bar(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlReply {
    Done,
    Errors(Vec<String>),
    Png(Vec<u8>),
    Modules(Vec<Props>),
}

/// `Control1` properties.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControlState {
    pub active_layer: String,
    /// The layout the base layer shows now.
    pub active_layout: String,
    /// Index into the config's `rules` of the rule that picked it, or -1.
    pub active_rule: i32,
    /// That rule's `name` (empty if unnamed or none).
    pub active_rule_name: String,
    /// Why this layout: `rule`, `default` (the layer's default) or `forced`
    /// (`SetLayout`). Empty before the first frame.
    pub layout_source: String,
    pub layers: Vec<String>,
    pub active_profile: String,
    pub profiles: Vec<String>,
    pub variables: Props,
    pub healthy: bool,
    /// Config, stylesheet and module problems now, one per line (`Errors`; empty = none).
    pub errors: Vec<String>,
}

/// The agent core, as seen by the API.
pub trait Host: Send + Sync + 'static {
    /// Something published changed (contexts, items, focus). Re-resolve and re-render soon.
    fn changed(&self);
    fn control(&self, req: Control) -> std::result::Result<ControlReply, String>;
    fn control_state(&self) -> ControlState;
    /// This connection got or lost [`BUS_NAME`] (another agent took it with `--replace`, or gave
    /// it back by exiting).
    fn name_owned(&self, _owned: bool) {}
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FocusSource {
    /// The agent's own compositor backend (Hyprland/sway IPC). `Desktop1.SetFocus` is refused.
    Backend,
    /// A `Desktop1` client feeds focus. The first one to call wins until it disconnects.
    #[default]
    External,
}

/// State shared between D-Bus handlers and the agent core.
#[derive(Debug, Default)]
pub struct Shared {
    pub reg: Registry,
    pub focus: Focus,
    pub policy: Policy,
    pub focus_source: FocusSource,
    focus_owner: Option<String>,
    pub workspaces: Vec<Props>,
    /// `Desktop1.SetWindows`: every window, as a DE integration reports them.
    pub windows: Vec<Props>,
    display_state: String,
}

struct Inner {
    shared: Mutex<Shared>,
    host: Arc<dyn Host>,
    proc: Arc<dyn ProcInfo>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Make sure the sender is a known client (credentials from the bus, identity from /proc).
    async fn admit(&self, conn: &Connection, hdr: &Header<'_>) -> Result<String> {
        let sender = hdr
            .sender()
            .ok_or_else(|| Error::AccessDenied("no sender".into()))?
            .to_string();
        if self.lock().reg.client(&sender).is_some() {
            return Ok(sender);
        }
        let dbus = zbus::fdo::DBusProxy::new(conn).await?;
        let creds = dbus
            .get_connection_credentials(
                BusName::try_from(sender.as_str()).map_err(zbus::Error::from)?,
            )
            .await
            .map_err(zbus::Error::from)?;
        let pid = creds.process_id();
        let identity = pid.map(|p| self.proc.identity(p)).unwrap_or_default();
        self.lock()
            .reg
            .connect(&sender, pid, identity, Instant::now());
        Ok(sender)
    }

    async fn admit_unsandboxed(&self, conn: &Connection, hdr: &Header<'_>) -> Result<String> {
        let sender = self.admit(conn, hdr).await?;
        let sandboxed = self
            .lock()
            .reg
            .client(&sender)
            .is_some_and(|c| c.identity.sandboxed());
        if sandboxed {
            return Err(Error::AccessDenied(
                "not available to sandboxed apps".into(),
            ));
        }
        Ok(sender)
    }
}

/// Handle to the running API, for the agent core.
#[derive(Clone)]
pub struct Api {
    conn: Connection,
    inner: Arc<Inner>,
}

impl Api {
    /// Serve the API on `conn` and request [`BUS_NAME`]; `NameTaken` if another agent owns it.
    /// Use [`Api::serve_unnamed`] in tests that run several agents on one bus.
    pub async fn serve(
        conn: Connection,
        host: Arc<dyn Host>,
        proc: Arc<dyn ProcInfo>,
    ) -> zbus::Result<Api> {
        Self::serve_claiming(conn, host, proc, false).await
    }

    /// [`Api::serve`]; with `replace`, take [`BUS_NAME`] from a running agent (`--replace`). It
    /// waits in the bus's queue and gets the name back when this one exits.
    ///
    /// Never replace by default: systemd runs the agent as `Type=dbus`, so a second agent taking
    /// the name and exiting would stop the installed one (zbus's `request_name` both replaces and
    /// allows replacement). No `DoNotQueue`: queueing is what gives a replaced owner the name back.
    pub async fn serve_claiming(
        conn: Connection,
        host: Arc<dyn Host>,
        proc: Arc<dyn ProcInfo>,
        replace: bool,
    ) -> zbus::Result<Api> {
        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        let api = Self::serve_unnamed(conn, host, proc).await?;
        let flags = if replace {
            RequestNameFlags::AllowReplacement | RequestNameFlags::ReplaceExisting
        } else {
            RequestNameFlags::AllowReplacement.into()
        };
        match api.conn.request_name_with_flags(BUS_NAME, flags).await? {
            RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(api),
            RequestNameReply::InQueue | RequestNameReply::Exists => {
                // Don't wait in line: the caller says why and stops.
                let _ = api.conn.release_name(BUS_NAME).await;
                Err(zbus::Error::NameTaken)
            }
        }
    }

    pub async fn serve_unnamed(
        conn: Connection,
        host: Arc<dyn Host>,
        proc: Arc<dyn ProcInfo>,
    ) -> zbus::Result<Api> {
        let inner = Arc::new(Inner {
            shared: Mutex::new(Shared {
                display_state: "on".into(),
                ..Default::default()
            }),
            host,
            proc,
        });
        let server = conn.object_server();
        server
            .at(
                ROOT_PATH,
                TouchBar {
                    inner: inner.clone(),
                },
            )
            .await?;
        server
            .at(
                ROOT_PATH,
                ControlIface {
                    inner: inner.clone(),
                },
            )
            .await?;
        server
            .at(
                ROOT_PATH,
                Desktop {
                    inner: inner.clone(),
                },
            )
            .await?;
        let api = Api {
            conn: conn.clone(),
            inner,
        };
        let watcher = api.clone();
        let me = conn.unique_name().map(|n| n.to_string());
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let mut changes = dbus.receive_name_owner_changed().await?;
        conn.executor()
            .spawn(
                async move {
                    use futures_lite::StreamExt;
                    while let Some(sig) = changes.next().await {
                        let Ok(args) = sig.args() else { continue };
                        if args.name() == BUS_NAME {
                            let owner = args.new_owner().as_ref().map(|o| o.to_string());
                            watcher
                                .inner
                                .host
                                .name_owned(owner.is_some() && owner == me);
                        }
                        if args.new_owner().is_none() && args.name().starts_with(':') {
                            let _ = watcher.client_gone(args.name()).await;
                        }
                    }
                },
                "sliver-name-owner-changed",
            )
            .detach();
        Ok(api)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Lock the shared state (registry, focus, policy). Don't hold it across `.await`.
    pub fn state(&self) -> MutexGuard<'_, Shared> {
        self.inner.lock()
    }

    /// Who fills App controls / Status items right now.
    pub fn resolve(&self) -> Resolved {
        let s = self.inner.lock();
        s.reg.resolve(&s.focus, &s.policy, self.inner.proc.as_ref())
    }

    /// Focus from the agent's own compositor backend. This also makes `Desktop1.SetFocus` busy.
    pub fn set_focus(&self, focus: Focus) {
        {
            let mut s = self.inner.lock();
            s.focus = focus;
            s.focus_source = FocusSource::Backend;
        }
        self.inner.host.changed();
    }

    async fn client_gone(&self, name: &str) -> zbus::Result<()> {
        let gone = {
            let mut s = self.inner.lock();
            if s.focus_owner.as_deref() == Some(name) {
                s.focus_owner = None;
            }
            s.reg.client_gone(name)
        };
        for id in &gone {
            self.conn
                .object_server()
                .remove::<ContextIface, _>(id.path())
                .await?;
        }
        if !gone.is_empty() {
            self.inner.host.changed();
        }
        Ok(())
    }

    /// The user did something to a published item: update state and tell the owner.
    pub async fn interact(
        &self,
        ctx: ContextId,
        item: &str,
        what: Interaction,
    ) -> zbus::Result<bool> {
        let out = self.inner.lock().reg.interact(ctx, item, what);
        match out {
            Some(o) => {
                self.emit(&o).await?;
                // Optimistic state (a picked segment or candidate, a scroll, a closed modal):
                // redraw now rather than wait for the app. Slider drags draw locally already.
                if !matches!(what, Interaction::Slider { .. }) {
                    self.inner.host.changed();
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Send `Event` signals, unicast.
    pub async fn emit(&self, o: &Outgoing) -> zbus::Result<()> {
        let path = o.context.path();
        let dest = BusName::try_from(o.destination.as_str())?;
        let emitter = SignalEmitter::new(&self.conn, path.as_str())?.set_destination(dest);
        ContextIface::event(&emitter, &o.item, o.kind, to_dict(&o.data)).await
    }

    /// Tell the API which contexts the agent drew (`OnScreen` property).
    pub async fn set_on_screen(&self, drawn: &HashSet<ContextId>) -> zbus::Result<()> {
        let changed = self.inner.lock().reg.set_on_screen(drawn);
        for (id, _) in changed {
            let iface = self
                .conn
                .object_server()
                .interface::<_, ContextIface>(id.path())
                .await?;
            iface
                .get()
                .await
                .on_screen_changed(iface.signal_emitter())
                .await?;
        }
        Ok(())
    }

    /// `on`, `dimmed` or `off` (from sliverd's `display` message).
    pub async fn set_display_state(&self, state: &str) -> zbus::Result<()> {
        self.inner.lock().display_state = state.to_owned();
        let iface = self
            .conn
            .object_server()
            .interface::<_, TouchBar>(ROOT_PATH)
            .await?;
        iface
            .get()
            .await
            .display_state_changed(iface.signal_emitter())
            .await
    }

    /// The core's `Control1` properties changed.
    pub async fn control_state_changed(&self) -> zbus::Result<()> {
        let iface = self
            .conn
            .object_server()
            .interface::<_, ControlIface>(ROOT_PATH)
            .await?;
        let e = iface.signal_emitter();
        let i = iface.get().await;
        i.active_layer_changed(e).await?;
        i.active_layout_changed(e).await?;
        i.active_rule_changed(e).await?;
        i.active_rule_name_changed(e).await?;
        i.active_layout_source_changed(e).await?;
        i.layers_changed(e).await?;
        i.active_profile_changed(e).await?;
        i.profiles_changed(e).await?;
        i.variables_changed(e).await?;
        i.healthy_changed(e).await?;
        i.errors_changed(e).await
    }

    /// `Control1.Signal(n)` reached published contexts whose user config sets `signal = n`.
    pub async fn signal_contexts(&self, contexts: &[ContextId], n: u32) -> zbus::Result<()> {
        let outs = self.inner.lock().reg.signal(contexts, n);
        for o in &outs {
            self.emit(o).await?;
        }
        Ok(())
    }
}

mod context;
mod control;
mod desktop;
mod props;
mod touchbar;

use context::ContextIface;
use control::ControlIface;
use desktop::Desktop;
pub use desktop::pushed_desktop;
use props::*;
pub use props::{prop_from_value, prop_to_value};
use touchbar::TouchBar;

#[cfg(test)]
mod tests;
