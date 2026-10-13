//! The zbus layer: `TouchBar1`, `Context1`, `Control1` and `Desktop1` on the session bus.
//!
//! Thin by design. Every call is translated into [`Registry`] methods (policy lives there) or
//! forwarded to the agent core through [`Host`]. The core drives the other direction through
//! [`Api`]: resolve what to draw, report interactions, on-screen state and display state.

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

/// D-Bus errors (`docs/api.md`). Names are checked against `sliver_ipc::dbus::error` in tests.
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

/// Requests the agent core answers (`Control1`). Module ids follow docs/customization.md.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    SwitchLayer(String),
    PushLayer(String, u32),
    PopLayer,
    ToggleLayer(String),
    SetLayout(String),
    /// `HoldLayout`/`HoldLayoutWith` (non-empty name): like `SetLayout`, and touches on it are
    /// reported to the holder (`HeldPressed`); `select_only` stops them from firing.
    HoldLayout {
        layout: String,
        select_only: bool,
    },
    SetProfile(String),
    ReloadConfig,
    ValidateConfig(String),
    Preview(String, u32),
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
    /// Why this layout: `rule`, `default` (the layer's default) or `forced` (`SetLayout` /
    /// `HoldLayout`). Empty before the first frame.
    pub layout_source: String,
    pub layers: Vec<String>,
    pub active_profile: String,
    pub profiles: Vec<String>,
    pub variables: Props,
    pub healthy: bool,
}

/// The agent core, as seen by the API.
pub trait Host: Send + Sync + 'static {
    /// Something published changed (contexts, items, focus). Re-resolve and re-render soon.
    fn changed(&self);
    fn control(&self, req: Control) -> std::result::Result<ControlReply, String>;
    fn control_state(&self) -> ControlState;
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
    /// Who holds the current `HoldLayout` override. It's dropped when that client disconnects,
    /// so a crashed editor (Mirror to Touch Bar) never leaves the bar on its layout.
    layout_owner: Option<String>,
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
    /// Serve the API on `conn` and request [`BUS_NAME`]. Use [`Api::serve_unnamed`] in tests that
    /// run several agents on one bus.
    pub async fn serve(
        conn: Connection,
        host: Arc<dyn Host>,
        proc: Arc<dyn ProcInfo>,
    ) -> zbus::Result<Api> {
        let api = Self::serve_unnamed(conn, host, proc).await?;
        api.conn.request_name(BUS_NAME).await?;
        Ok(api)
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
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let mut changes = dbus.receive_name_owner_changed().await?;
        conn.executor()
            .spawn(
                async move {
                    use futures_lite::StreamExt;
                    while let Some(sig) = changes.next().await {
                        let Ok(args) = sig.args() else { continue };
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
            let layout_gone = s.layout_owner.as_deref() == Some(name);
            if layout_gone {
                s.layout_owner = None;
            }
            (s.reg.client_gone(name), layout_gone)
        };
        let (gone, layout_gone) = gone;
        if layout_gone {
            // The core answers on its own thread; don't block this executor on it.
            let host = self.inner.host.clone();
            std::thread::spawn(move || {
                let _ = host.control(Control::SetLayout(String::new()));
            });
        }
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
        i.healthy_changed(e).await
    }

    /// The user touched `module` (zone `zone`, `index` in its `modules`) on the held layout:
    /// tell the holder (`Control1.HeldPressed`, unicast). Nothing if no one holds it.
    pub async fn held_pressed(&self, module: &str, zone: &str, index: u32) -> zbus::Result<()> {
        let Some(owner) = self.inner.lock().layout_owner.clone() else {
            return Ok(());
        };
        let dest = BusName::try_from(owner.as_str())?;
        let e = SignalEmitter::new(&self.conn, ROOT_PATH)?.set_destination(dest);
        ControlIface::held_pressed(&e, module, zone, index).await
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

struct TouchBar {
    inner: Arc<Inner>,
}

#[interface(name = "io.github.wawow830.Sliver.TouchBar1")]
impl TouchBar {
    async fn create_context(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        options: HashMap<String, OwnedValue>,
    ) -> Result<OwnedObjectPath> {
        let sender = self.inner.admit(conn, &hdr).await?;
        let (id, _warnings) =
            self.inner
                .lock()
                .reg
                .create_context(&sender, &to_props(&options), Instant::now())?;
        let ctx = ContextIface {
            inner: self.inner.clone(),
            id,
        };
        if let Err(e) = conn.object_server().at(id.path(), ctx).await {
            let _ = self.inner.lock().reg.destroy(&sender, id);
            return Err(e.into());
        }
        self.inner.host.changed();
        Ok(OwnedObjectPath::try_from(id.path()).map_err(zbus::Error::from)?)
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        API_MINOR
    }

    #[zbus(property)]
    fn features(&self) -> Vec<String> {
        [
            "item:button",
            "item:toggle",
            "item:label",
            "item:slider",
            "item:segmented",
            "item:scrubber",
            "item:progress",
        ]
        .into_iter()
        .chain([
            "item:spacer",
            "item:group",
            "item:popover",
            "gesture:hold",
            "gesture:swipe",
            "gesture:double-tap",
        ])
        .chain(["modal", "values", "classes"])
        .map(str::to_owned)
        .collect()
    }

    #[zbus(property)]
    fn display_state(&self) -> String {
        self.inner.lock().display_state.clone()
    }
}

struct ContextIface {
    inner: Arc<Inner>,
    id: ContextId,
}

#[interface(name = "io.github.wawow830.Sliver.Context1")]
impl ContextIface {
    async fn set_items(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        items: Vec<HashMap<String, OwnedValue>>,
    ) -> Result<Vec<String>> {
        let sender = self.inner.admit(conn, &hdr).await?;
        let list: Vec<Props> = items.iter().map(to_props).collect();
        let w = self
            .inner
            .lock()
            .reg
            .set_items(&sender, self.id, &list, Instant::now())?;
        self.inner.host.changed();
        Ok(w)
    }

    async fn update(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        changes: HashMap<String, HashMap<String, OwnedValue>>,
    ) -> Result<Vec<String>> {
        let sender = self.inner.admit(conn, &hdr).await?;
        let ch: BTreeMap<String, Props> = changes
            .iter()
            .map(|(k, v)| (k.clone(), to_props(v)))
            .collect();
        let w = self
            .inner
            .lock()
            .reg
            .update(&sender, self.id, &ch, Instant::now())?;
        self.inner.host.changed();
        Ok(w)
    }

    async fn set_visible(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        visible: bool,
    ) -> Result<()> {
        let sender = self.inner.admit(conn, &hdr).await?;
        self.inner
            .lock()
            .reg
            .set_visible(&sender, self.id, visible, Instant::now())?;
        self.inner.host.changed();
        Ok(())
    }

    async fn destroy(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<()> {
        let sender = self.inner.admit(conn, &hdr).await?;
        self.inner.lock().reg.destroy(&sender, self.id)?;
        conn.object_server()
            .remove::<ContextIface, _>(self.id.path())
            .await?;
        self.inner.host.changed();
        Ok(())
    }

    #[zbus(property)]
    fn on_screen(&self) -> bool {
        self.inner
            .lock()
            .reg
            .context(self.id)
            .is_some_and(|c| c.on_screen)
    }

    #[zbus(property)]
    fn scope(&self) -> String {
        self.inner
            .lock()
            .reg
            .context(self.id)
            .map(|c| c.opts.scope.name())
            .unwrap_or_default()
            .to_owned()
    }

    #[zbus(property)]
    fn provider(&self) -> String {
        self.inner
            .lock()
            .reg
            .context(self.id)
            .map(|c| c.provider.clone())
            .unwrap_or_default()
    }

    #[zbus(signal)]
    async fn event(
        emitter: &SignalEmitter<'_>,
        item: &str,
        kind: &str,
        data: HashMap<String, Value<'_>>,
    ) -> zbus::Result<()>;
}

struct ControlIface {
    inner: Arc<Inner>,
}

impl ControlIface {
    async fn call(
        &self,
        conn: &Connection,
        hdr: &Header<'_>,
        req: Control,
    ) -> Result<ControlReply> {
        self.inner.admit_unsandboxed(conn, hdr).await?;
        self.inner.host.control(req).map_err(Error::Failed)
    }

    async fn unit(&self, conn: &Connection, hdr: &Header<'_>, req: Control) -> Result<()> {
        self.call(conn, hdr, req).await.map(|_| ())
    }

    async fn errors(
        &self,
        conn: &Connection,
        hdr: &Header<'_>,
        req: Control,
    ) -> Result<Vec<String>> {
        match self.call(conn, hdr, req).await? {
            ControlReply::Errors(e) => Ok(e),
            _ => Ok(Vec::new()),
        }
    }
}

impl ControlIface {
    async fn hold(
        &self,
        c: &Connection,
        h: &Header<'_>,
        name: String,
        select_only: bool,
    ) -> Result<()> {
        let owner = (!name.is_empty())
            .then(|| h.sender().map(|s| s.to_string()))
            .flatten();
        let req = if name.is_empty() {
            Control::SetLayout(name)
        } else {
            Control::HoldLayout {
                layout: name,
                select_only,
            }
        };
        self.unit(c, h, req).await?;
        self.inner.lock().layout_owner = owner;
        Ok(())
    }
}

#[interface(name = "io.github.wawow830.Sliver.Control1")]
impl ControlIface {
    async fn switch_layer(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::SwitchLayer(name)).await
    }
    async fn push_layer(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
        timeout_ms: u32,
    ) -> Result<()> {
        self.unit(c, &h, Control::PushLayer(name, timeout_ms)).await
    }
    async fn pop_layer(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
    ) -> Result<()> {
        self.unit(c, &h, Control::PopLayer).await
    }
    async fn toggle_layer(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::ToggleLayer(name)).await
    }
    /// Persistent override (`sliverctl layout NAME`): stays after the caller exits.
    async fn set_layout(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::SetLayout(name)).await?;
        self.inner.lock().layout_owner = None;
        Ok(())
    }
    /// Override held by the caller's connection (Mirror to Touch Bar): dropped when the caller
    /// disconnects, so a crashed editor never pins the bar. `""` releases it now.
    async fn hold_layout(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
    ) -> Result<()> {
        self.hold(c, &h, name, false).await
    }
    /// `HoldLayout` with options: `select-only` (b) reports touches (`HeldPressed`) without
    /// firing the items. Calling it again while holding changes the options.
    async fn hold_layout_with(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
        options: HashMap<String, OwnedValue>,
    ) -> Result<()> {
        let select_only = match options.get("select-only") {
            None => false,
            Some(v) => bool::try_from(v)
                .map_err(|_| Error::InvalidArgs("select-only must be a boolean".into()))?,
        };
        self.hold(c, &h, name, select_only).await
    }
    /// A touch on the held layout (`HoldLayout`), sent to the holder only: the touched
    /// module's key (`clock`, `group/tools/key/Mute`), its zone, and its index in that zone's
    /// `modules` list in the config. Settings selects it (ux.md §4 Mirror).
    #[zbus(signal)]
    async fn held_pressed(
        emitter: &SignalEmitter<'_>,
        module: &str,
        zone: &str,
        index: u32,
    ) -> zbus::Result<()>;
    async fn set_profile(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::SetProfile(name)).await
    }
    async fn reload_config(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
    ) -> Result<Vec<String>> {
        self.errors(c, &h, Control::ReloadConfig).await
    }
    async fn validate_config(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        toml: String,
    ) -> Result<Vec<String>> {
        self.errors(c, &h, Control::ValidateConfig(toml)).await
    }
    async fn preview(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        layout: String,
        timeout_ms: u32,
    ) -> Result<()> {
        self.unit(c, &h, Control::Preview(layout, timeout_ms)).await
    }
    async fn render_preview(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        layout: String,
        options: HashMap<String, OwnedValue>,
    ) -> Result<Vec<u8>> {
        match self
            .call(c, &h, Control::RenderPreview(layout, to_props(&options)))
            .await?
        {
            ControlReply::Png(p) => Ok(p),
            _ => Err(Error::Failed("no image".into())),
        }
    }
    async fn set_variable(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
        value: OwnedValue,
    ) -> Result<()> {
        let v = prop_from_value(&value)
            .ok_or_else(|| Error::InvalidArgs("unsupported value type".into()))?;
        self.unit(c, &h, Control::SetVariable(name, v)).await
    }
    async fn list_contexts(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
    ) -> Result<Vec<HashMap<String, OwnedValue>>> {
        self.inner.admit_unsandboxed(c, &h).await?;
        let s = self.inner.lock();
        Ok(s.reg
            .contexts()
            .map(|ctx| {
                let mut p = Props::new();
                p.insert("path".into(), Prop::Str(ctx.id.path()));
                p.insert("provider".into(), Prop::Str(ctx.provider.clone()));
                p.insert(
                    "title".into(),
                    Prop::Str(ctx.opts.title.clone().unwrap_or_default()),
                );
                p.insert("scope".into(), Prop::Str(ctx.opts.scope.name().into()));
                p.insert("visible".into(), Prop::Bool(ctx.visible));
                p.insert("on-screen".into(), Prop::Bool(ctx.on_screen));
                p.insert("items".into(), Prop::Int(ctx.items.len() as i64));
                // What `app/<provider>#<id>` can place (top-level items; spacers aren't worth it),
                // with a name for pickers: the label, else the a11y label, else the id.
                let placeable: Vec<&crate::item::Item> = ctx
                    .items
                    .iter()
                    .filter(|i| !matches!(i.kind, crate::item::ItemKind::Spacer { .. }))
                    .collect();
                p.insert(
                    "item-ids".into(),
                    Prop::List(placeable.iter().map(|i| Prop::Str(i.id.clone())).collect()),
                );
                p.insert(
                    "item-labels".into(),
                    Prop::List(
                        placeable
                            .iter()
                            .map(|i| {
                                Prop::Str(
                                    i.label
                                        .clone()
                                        .filter(|l| !l.is_empty())
                                        .or_else(|| i.a11y_label.clone())
                                        .unwrap_or_else(|| i.id.clone()),
                                )
                            })
                            .collect(),
                    ),
                );
                let sandboxed = s
                    .reg
                    .client(&ctx.owner)
                    .is_some_and(|c| c.identity.sandboxed());
                p.insert("sandboxed".into(), Prop::Bool(sandboxed));
                if let Scope::Target(t) = &ctx.opts.scope
                    && let Some(a) = &t.app_id
                {
                    p.insert("target-app-id".into(), Prop::Str(a.clone()));
                }
                to_owned_dict(&p)
            })
            .collect())
    }
    async fn signal(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        n: u32,
    ) -> Result<()> {
        self.unit(c, &h, Control::Signal(n)).await
    }
    async fn refresh(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        module: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::Refresh(module)).await
    }
    async fn show(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        module: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::Show(module)).await
    }
    async fn hide(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        module: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::Hide(module)).await
    }
    async fn toggle(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        module: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::Toggle(module)).await
    }
    async fn reset_visibility(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
    ) -> Result<()> {
        self.unit(c, &h, Control::ResetVisibility).await
    }
    async fn trigger(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        module: String,
        gesture: String,
    ) -> Result<()> {
        let g = Gesture::parse(&gesture)
            .ok_or_else(|| Error::InvalidArgs(format!("unknown gesture {gesture:?}")))?;
        self.unit(c, &h, Control::Trigger(module, g)).await
    }
    async fn update(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        module: String,
        output: HashMap<String, OwnedValue>,
    ) -> Result<()> {
        self.unit(c, &h, Control::Update(module, to_props(&output)))
            .await
    }
    async fn list_modules(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
    ) -> Result<Vec<HashMap<String, OwnedValue>>> {
        match self.call(c, &h, Control::ListModules).await? {
            ControlReply::Modules(m) => Ok(m.iter().map(to_owned_dict).collect()),
            _ => Ok(Vec::new()),
        }
    }
    async fn bar(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        action: String,
    ) -> Result<()> {
        if !["show", "hide", "toggle"].contains(&action.as_str()) {
            return Err(Error::InvalidArgs(
                "action must be show, hide or toggle".into(),
            ));
        }
        self.unit(c, &h, Control::Bar(action)).await
    }

    #[zbus(property)]
    fn active_layer(&self) -> String {
        self.inner.host.control_state().active_layer
    }
    #[zbus(property)]
    fn active_layout(&self) -> String {
        self.inner.host.control_state().active_layout
    }
    #[zbus(property)]
    fn active_rule(&self) -> i32 {
        self.inner.host.control_state().active_rule
    }
    #[zbus(property)]
    fn active_rule_name(&self) -> String {
        self.inner.host.control_state().active_rule_name
    }
    #[zbus(property)]
    fn active_layout_source(&self) -> String {
        self.inner.host.control_state().layout_source
    }
    #[zbus(property)]
    fn layers(&self) -> Vec<String> {
        self.inner.host.control_state().layers
    }
    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.inner.host.control_state().active_profile
    }
    #[zbus(property)]
    fn profiles(&self) -> Vec<String> {
        self.inner.host.control_state().profiles
    }
    #[zbus(property)]
    fn variables(&self) -> HashMap<String, OwnedValue> {
        to_owned_dict(&self.inner.host.control_state().variables)
    }
    #[zbus(property)]
    fn healthy(&self) -> bool {
        self.inner.host.control_state().healthy
    }
}

struct Desktop {
    inner: Arc<Inner>,
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
fn workspace_id(p: &Props, index: usize) -> i64 {
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

/// `SetFocus` keys (`docs/api.md` §5) → a window. Empty dict = nothing focused.
fn focus_window(p: &Props) -> Option<Window> {
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

/// zvariant → [`Prop`]. Unsupported types (fds, structs of mixed types) become `None`.
pub fn prop_from_value(v: &Value<'_>) -> Option<Prop> {
    Some(match v {
        Value::Bool(b) => Prop::Bool(*b),
        Value::U8(x) => Prop::Int((*x).into()),
        Value::I16(x) => Prop::Int((*x).into()),
        Value::U16(x) => Prop::Int((*x).into()),
        Value::I32(x) => Prop::Int((*x).into()),
        Value::U32(x) => Prop::Int((*x).into()),
        Value::I64(x) => Prop::Int(*x),
        Value::U64(x) => Prop::Int(i64::try_from(*x).ok()?),
        Value::F64(x) => Prop::Num(*x),
        Value::Str(s) => Prop::Str(s.to_string()),
        Value::ObjectPath(p) => Prop::Str(p.to_string()),
        Value::Signature(s) => Prop::Str(s.to_string()),
        Value::Value(inner) => return prop_from_value(inner),
        Value::Array(a) if a.element_signature() == &Signature::U8 => Prop::Bytes(
            a.inner()
                .iter()
                .filter_map(|x| if let Value::U8(b) = x { Some(*b) } else { None })
                .collect(),
        ),
        Value::Array(a) => Prop::List(a.inner().iter().filter_map(prop_from_value).collect()),
        Value::Dict(d) => Prop::Map(
            d.iter()
                .filter_map(|(k, v)| match k {
                    Value::Str(k) => Some((k.to_string(), prop_from_value(v)?)),
                    _ => None,
                })
                .collect(),
        ),
        Value::Structure(s) => Prop::List(s.fields().iter().filter_map(prop_from_value).collect()),
        _ => return None,
    })
}

fn to_props(m: &HashMap<String, OwnedValue>) -> Props {
    m.iter()
        .filter_map(|(k, v)| Some((k.clone(), prop_from_value(v)?)))
        .collect()
}

/// [`Prop`] → zvariant. Ints become `i` when they fit, else `x`. Lists of mixed types become `av`.
pub fn prop_to_value(p: &Prop) -> Value<'static> {
    match p {
        Prop::Bool(b) => Value::Bool(*b),
        Prop::Int(i) => i32::try_from(*i).map(Value::I32).unwrap_or(Value::I64(*i)),
        Prop::Num(n) => Value::F64(*n),
        Prop::Str(s) => Value::Str(Str::from(s.clone())),
        Prop::Bytes(b) => Value::from(b.clone()),
        Prop::List(l) => {
            let mut a = Array::new(&Signature::Variant);
            for x in l {
                let _ = a.append(Value::Value(Box::new(prop_to_value(x))));
            }
            Value::Array(a)
        }
        Prop::Map(m) => Value::Dict(to_dict_value(m)),
    }
}

fn to_dict_value(m: &Props) -> Dict<'static, 'static> {
    let mut d = Dict::new(&Signature::Str, &Signature::Variant);
    for (k, v) in m {
        let _ = d.append(
            Value::Str(Str::from(k.clone())),
            Value::Value(Box::new(prop_to_value(v))),
        );
    }
    d
}

fn to_dict(m: &Props) -> HashMap<String, Value<'static>> {
    m.iter()
        .map(|(k, v)| (k.clone(), prop_to_value(v)))
        .collect()
}

fn to_owned_dict(m: &Props) -> HashMap<String, OwnedValue> {
    m.iter()
        .filter_map(|(k, v)| Some((k.clone(), OwnedValue::try_from(prop_to_value(v)).ok()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sliver_ipc::dbus;

    #[test]
    fn error_names_match_the_spec() {
        use zbus::DBusError;
        assert_eq!(
            Error::AccessDenied(String::new()).name().as_str(),
            dbus::error::ACCESS_DENIED
        );
        assert_eq!(
            Error::LimitExceeded(String::new()).name().as_str(),
            dbus::error::LIMIT_EXCEEDED
        );
        assert_eq!(
            Error::InvalidArgs(String::new()).name().as_str(),
            dbus::error::INVALID_ARGS
        );
        assert_eq!(
            Error::Busy(String::new()).name().as_str(),
            dbus::error::BUSY
        );
    }

    #[test]
    fn value_roundtrip() {
        let p = Prop::Map(Props::from([
            (
                "a".into(),
                Prop::List(vec![Prop::Int(1), Prop::Str("x".into())]),
            ),
            ("b".into(), Prop::Bytes(vec![1, 2, 3])),
            ("c".into(), Prop::Num(0.5)),
            ("d".into(), Prop::Int(1 << 40)),
        ]));
        assert_eq!(prop_from_value(&prop_to_value(&p)), Some(p));
    }

    #[test]
    fn focus_keys() {
        let p = Props::from([
            ("app-id".into(), Prop::Str("firefox".into())),
            ("pid".into(), Prop::Int(42)),
            ("window".into(), Prop::Str("hyprland:0xabc".into())),
            ("fullscreen".into(), Prop::Int(2)),
        ]);
        let w = focus_window(&p).unwrap();
        assert_eq!(
            (w.class.as_str(), w.pid, w.address.as_str(), w.fullscreen),
            ("firefox", Some(42), "abc", true)
        );
        assert!(focus_window(&Props::new()).is_none());
    }
}
