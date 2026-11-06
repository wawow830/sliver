//! `Context1`: one app's published items, at its own object path.

use super::*;

pub(super) struct ContextIface {
    pub(super) inner: Arc<Inner>,
    pub(super) id: ContextId,
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
    pub(super) async fn event(
        emitter: &SignalEmitter<'_>,
        item: &str,
        kind: &str,
        data: HashMap<String, Value<'_>>,
    ) -> zbus::Result<()>;
}
