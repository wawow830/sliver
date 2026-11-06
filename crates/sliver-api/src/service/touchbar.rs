//! `TouchBar1`: what the bar is and does, for any app.

use super::*;

pub(super) struct TouchBar {
    pub(super) inner: Arc<Inner>,
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
            "item:image",
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
