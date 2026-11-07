//! The `Control1` interface: each method is a [`Control`] request to the [`Host`].

use std::collections::HashMap;
use std::sync::Arc;

use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, interface, names::BusName};

use super::prop::{to_owned_dict, to_props};
use super::{Control, ControlReply, Error, Host, prop_from_value, sandboxed};

type Result<T> = std::result::Result<T, Error>;

pub(super) struct ControlIface {
    pub(super) host: Arc<dyn Host>,
}

impl ControlIface {
    async fn call(
        &self,
        conn: &Connection,
        hdr: &Header<'_>,
        req: Control,
    ) -> Result<ControlReply> {
        let sender = hdr
            .sender()
            .ok_or_else(|| Error::AccessDenied("no sender".into()))?;
        let creds = zbus::fdo::DBusProxy::new(conn)
            .await?
            .get_connection_credentials(BusName::from(sender.to_owned()))
            .await
            .map_err(zbus::Error::from)?;
        if creds.process_id().is_none_or(sandboxed) {
            return Err(Error::AccessDenied(
                "not available to sandboxed apps".into(),
            ));
        }
        self.host.control(req).map_err(Error::Failed)
    }

    async fn unit(&self, conn: &Connection, hdr: &Header<'_>, req: Control) -> Result<()> {
        self.call(conn, hdr, req).await.map(|_| ())
    }

    /// Every property changed (the host says when, not which).
    pub(super) async fn state_changed(&self, e: &SignalEmitter<'_>) -> zbus::Result<()> {
        self.active_layer_changed(e).await?;
        self.active_layout_changed(e).await?;
        self.active_rule_changed(e).await?;
        self.active_rule_name_changed(e).await?;
        self.active_layout_source_changed(e).await?;
        self.layers_changed(e).await?;
        self.active_profile_changed(e).await?;
        self.profiles_changed(e).await?;
        self.variables_changed(e).await?;
        self.healthy_changed(e).await?;
        self.errors_changed(e).await
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
    /// Shows this layout on the base layer whatever the rules say (`sliverctl layout NAME`), until
    /// the next call; `""` goes back to the rules.
    async fn set_layout(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        name: String,
    ) -> Result<()> {
        self.unit(c, &h, Control::SetLayout(name)).await
    }
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
        match self.call(c, &h, Control::ReloadConfig).await? {
            ControlReply::Errors(e) => Ok(e),
            _ => Ok(Vec::new()),
        }
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
    /// `gesture`: `tap`, `double-tap`, `hold`, `swipe-left` or `swipe-right`.
    async fn trigger(
        &self,
        #[zbus(header)] h: Header<'_>,
        #[zbus(connection)] c: &Connection,
        module: String,
        gesture: String,
    ) -> Result<()> {
        use sliver_config::Gesture as G;
        let g = [G::Tap, G::DoubleTap, G::Hold, G::SwipeLeft, G::SwipeRight]
            .into_iter()
            .find(|g| g.key().strip_prefix("on-") == Some(gesture.as_str()))
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
        self.host.control_state().active_layer
    }
    #[zbus(property)]
    fn active_layout(&self) -> String {
        self.host.control_state().active_layout
    }
    #[zbus(property)]
    fn active_rule(&self) -> i32 {
        self.host.control_state().active_rule
    }
    #[zbus(property)]
    fn active_rule_name(&self) -> String {
        self.host.control_state().active_rule_name
    }
    #[zbus(property)]
    fn active_layout_source(&self) -> String {
        self.host.control_state().layout_source
    }
    #[zbus(property)]
    fn layers(&self) -> Vec<String> {
        self.host.control_state().layers
    }
    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.host.control_state().active_profile
    }
    #[zbus(property)]
    fn profiles(&self) -> Vec<String> {
        self.host.control_state().profiles
    }
    #[zbus(property)]
    fn variables(&self) -> HashMap<String, OwnedValue> {
        to_owned_dict(&self.host.control_state().variables)
    }
    #[zbus(property)]
    fn healthy(&self) -> bool {
        self.host.control_state().healthy
    }
    /// What's wrong with the config, stylesheet or a module right now (one line each, the file
    /// and line first when known). Empty when all is well.
    #[zbus(property)]
    fn errors(&self) -> Vec<String> {
        self.host.control_state().errors
    }
}
