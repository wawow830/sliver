//! `Control1`: the user's own tools (sliverctl) driving the agent.

use super::*;

pub(super) struct ControlIface {
    pub(super) inner: Arc<Inner>,
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

    async fn error_list(
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
        self.error_list(c, &h, Control::ReloadConfig).await
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
                // with a name to show: the label, else the a11y label, else the id.
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
    /// What's wrong with the config, stylesheet or a module right now (one line each, the file
    /// and line first when known). Empty when all is well.
    #[zbus(property)]
    fn errors(&self) -> Vec<String> {
        self.inner.host.control_state().errors
    }
}
