//! Rust client for Sliver's public Touch Bar API (`docs/api.md`).
//!
//! ```no_run
//! use sliver_client::{Client, ContextOptions, Item};
//! let client = Client::session()?;
//! let ctx = client.context(ContextOptions::status("org.example.Pomodoro").title("Pomodoro"))?;
//! ctx.set_items(vec![Item::button("start").label("▶ 25:00"), Item::toggle("dnd").label("DND")])?;
//! for ev in client.events() {
//!     if ev.item == "start" && ev.kind == "tap" {
//!         ctx.update("start", Item::props().label("⏸ 24:59"))?;
//!     }
//! }
//! # Ok::<(), sliver_client::Error>(())
//! ```
//!
//! Contexts are local handles. The client remembers what it published and **re-publishes it by
//! itself** whenever `sliver-agent` (re)appears on the bus, so apps don't need to care about agent
//! restarts. Contexts vanish when the [`Client`] is dropped (they're tied to the bus connection).

use sliver_ipc::dbus::{BUS_NAME, IFACE_CONTEXT, IFACE_TOUCHBAR, ROOT_PATH};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use zbus::blocking::{Connection, MessageIterator, fdo::DBusProxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{MatchRule, message::Type as MessageType};

pub use zbus::Error;
pub type Result<T> = std::result::Result<T, Error>;

/// Item or option properties (`a{sv}`).
pub type Props = HashMap<String, OwnedValue>;

fn ov(v: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("plain values never contain fds")
}

fn clone_props(p: &Props) -> Props {
    p.iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.try_clone().expect("plain values never contain fds"),
            )
        })
        .collect()
}

/// Builder for one item (`docs/api.md` §3). Unknown keys are allowed (newer agents may know them).
#[derive(Debug)]
pub struct Item(Props);

impl Clone for Item {
    fn clone(&self) -> Self {
        Item(clone_props(&self.0))
    }
}

impl Item {
    fn typed(ty: &str, id: &str) -> Item {
        Item::props().set("type", ty).set("id", id)
    }
    /// An empty property set, for [`ContextHandle::update`].
    pub fn props() -> Item {
        Item(Props::new())
    }
    pub fn button(id: &str) -> Item {
        Self::typed("button", id)
    }
    pub fn toggle(id: &str) -> Item {
        Self::typed("toggle", id)
    }
    pub fn label_item(id: &str, text: &str) -> Item {
        Self::typed("label", id).label(text)
    }
    pub fn slider(id: &str, min: f64, max: f64, value: f64) -> Item {
        Self::typed("slider", id)
            .set("min", min)
            .set("max", max)
            .set("value", value)
    }
    pub fn progress(id: &str, value: f64) -> Item {
        Self::typed("progress", id).set("value", value)
    }
    pub fn spacer(id: &str, size: &str) -> Item {
        Self::typed("spacer", id).set("size", size)
    }
    pub fn segmented(id: &str, labels: &[&str], selected: i32) -> Item {
        Self::typed("segmented", id)
            .set("segments", Self::labels(labels))
            .set("selected", selected)
    }
    /// A scrollable strip of candidates (tabs, suggestions): swipe scrolls, tap picks
    /// (`selected` event); `scrolled` events report the first visible one.
    pub fn scrubber(id: &str, labels: &[&str], selected: i32) -> Item {
        Self::typed("scrubber", id)
            .set("candidates", Self::labels(labels))
            .set("selected", selected)
    }
    /// `[{label}, …]`, the shape of `segments` and `candidates`.
    pub fn labels(labels: &[&str]) -> Vec<HashMap<String, OwnedValue>> {
        labels
            .iter()
            .map(|l| HashMap::from([("label".to_owned(), ov(Value::from(*l)))]))
            .collect()
    }
    pub fn group(id: &str, items: Vec<Item>) -> Item {
        Self::typed("group", id).set("items", items.into_iter().map(|i| i.0).collect::<Vec<_>>())
    }
    pub fn popover(id: &str, items: Vec<Item>) -> Item {
        Self::typed("popover", id).set("items", items.into_iter().map(|i| i.0).collect::<Vec<_>>())
    }

    /// Set any property.
    pub fn set<'a>(mut self, key: &str, value: impl Into<Value<'a>>) -> Item {
        self.0.insert(key.to_owned(), ov(value.into()));
        self
    }
    pub fn label(self, s: &str) -> Item {
        self.set("label", s)
    }
    pub fn icon(self, name: &str) -> Item {
        self.set("icon", name)
    }
    pub fn icon_data(self, png_or_svg: Vec<u8>) -> Item {
        self.set("icon-data", png_or_svg)
    }
    pub fn enabled(self, b: bool) -> Item {
        self.set("enabled", b)
    }
    pub fn visible(self, b: bool) -> Item {
        self.set("visible", b)
    }
    pub fn state(self, on: bool) -> Item {
        self.set("state", on)
    }
    pub fn value(self, v: f64) -> Item {
        self.set("value", v)
    }
    pub fn classes(self, c: &[&str]) -> Item {
        self.set(
            "classes",
            c.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }
    /// Named values for the user's `format` strings (`{percentage}`, `{alt}`, …).
    pub fn values(self, v: &[(&str, Value<'_>)]) -> Item {
        let m: HashMap<String, OwnedValue> = v
            .iter()
            .map(|(k, v)| (k.to_string(), ov(v.try_clone().expect("plain value"))))
            .collect();
        self.set("values", m)
    }
    pub fn gestures(self, g: &[&str]) -> Item {
        self.set(
            "gestures",
            g.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }
    pub fn priority(self, p: i32) -> Item {
        self.set("priority", p)
    }
    pub fn into_props(self) -> Props {
        self.0
    }
    fn id(&self) -> Option<String> {
        self.0
            .get("id")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
    }
}

/// `CreateContext` options (`docs/api.md` §2).
#[derive(Debug)]
pub struct ContextOptions(Props);

impl Clone for ContextOptions {
    fn clone(&self) -> Self {
        ContextOptions(clone_props(&self.0))
    }
}

impl ContextOptions {
    /// Controls for your own app's windows (verified by the agent from your pid).
    pub fn app() -> Self {
        ContextOptions(Props::new()).set("scope", "app")
    }
    /// Always-on items in the user's Status items.
    pub fn status(provider: &str) -> Self {
        ContextOptions(Props::new())
            .set("scope", "status")
            .set("provider", provider)
    }
    /// Items for *another* app, e.g. a script adding buttons while Firefox is focused.
    pub fn target_app(provider: &str, app_id: &str) -> Self {
        let sel = HashMap::from([("app-id".to_owned(), ov(Value::from(app_id)))]);
        ContextOptions(Props::new())
            .set("scope", "target")
            .set("provider", provider)
            .set("target", sel)
    }
    pub fn set<'a>(mut self, key: &str, value: impl Into<Value<'a>>) -> Self {
        self.0.insert(key.to_owned(), ov(value.into()));
        self
    }
    pub fn provider(self, p: &str) -> Self {
        self.set("provider", p)
    }
    pub fn title(self, t: &str) -> Self {
        self.set("title", t)
    }
    pub fn modal(self, on: bool) -> Self {
        self.set("modal", on)
    }
    pub fn include_children(self, on: bool) -> Self {
        self.set("include-children", on)
    }
    /// Bind to one window (`hyprland:0x…`, `wayland:<xdg-foreign handle>`, `x11:<xid>`).
    pub fn window(self, handle: &str) -> Self {
        self.set("window", handle)
    }
}

/// An `Event` from the agent (`docs/api.md` §3).
#[derive(Debug)]
pub struct Event {
    pub context: u32,
    pub item: String,
    pub kind: String,
    pub data: HashMap<String, OwnedValue>,
}

impl Event {
    pub fn get_bool(&self, k: &str) -> Option<bool> {
        self.data.get(k).and_then(|v| bool::try_from(v).ok())
    }
    pub fn get_f64(&self, k: &str) -> Option<f64> {
        self.data.get(k).and_then(|v| f64::try_from(v).ok())
    }
    pub fn get_i64(&self, k: &str) -> Option<i64> {
        let v = self.data.get(k)?;
        i32::try_from(v)
            .map(i64::from)
            .ok()
            .or_else(|| i64::try_from(v).ok())
    }
    pub fn get_str(&self, k: &str) -> Option<String> {
        self.data
            .get(k)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
    }
}

struct Local {
    options: ContextOptions,
    items: Vec<Item>,
    visible: bool,
    remote: Option<OwnedObjectPath>,
}

#[derive(Default)]
struct State {
    contexts: HashMap<u32, Local>,
    next: u32,
}

struct Inner {
    conn: Connection,
    state: Mutex<State>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn create_remote(&self, l: &Local) -> Result<OwnedObjectPath> {
        let path: OwnedObjectPath = self
            .conn
            .call_method(
                Some(BUS_NAME),
                ROOT_PATH,
                Some(IFACE_TOUCHBAR),
                "CreateContext",
                &(&l.options.0,),
            )?
            .body()
            .deserialize()?;
        if !l.items.is_empty() {
            let items: Vec<&Props> = l.items.iter().map(|i| &i.0).collect();
            self.call(&path, "SetItems", &(items,))?;
        }
        if !l.visible {
            self.call(&path, "SetVisible", &(false,))?;
        }
        Ok(path)
    }

    fn call<B>(&self, path: &OwnedObjectPath, member: &str, body: &B) -> Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.conn.call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            member,
            body,
        )
    }

    /// The agent (re)appeared: publish everything again.
    fn republish(&self) {
        let ids: Vec<u32> = self.lock().contexts.keys().copied().collect();
        for id in ids {
            let snapshot = {
                let s = self.lock();
                s.contexts.get(&id).map(|l| Local {
                    options: l.options.clone(),
                    items: l.items.clone(),
                    visible: l.visible,
                    remote: None,
                })
            };
            let Some(l) = snapshot else { continue };
            let remote = self.create_remote(&l).ok();
            if let Some(c) = self.lock().contexts.get_mut(&id) {
                c.remote = remote;
            }
        }
    }
}

/// A connection to the Touch Bar API.
pub struct Client {
    inner: Arc<Inner>,
    events: Mutex<Option<Receiver<Event>>>,
}

impl Client {
    /// Connect to the session bus. The socket is opened directly (`sliver_bus`), so a
    /// long-running app embedding the client doesn't get zbus's idle pool thread waking it.
    pub fn session() -> Result<Client> {
        Self::new(sliver_bus::session()?)
    }

    /// Use an existing connection (e.g. a private bus in tests).
    pub fn new(conn: Connection) -> Result<Client> {
        let inner = Arc::new(Inner {
            conn,
            state: Mutex::new(State::default()),
        });
        let (tx, rx) = channel();
        spawn_event_thread(inner.clone(), tx)?;
        spawn_owner_thread(inner.clone())?;
        Ok(Client {
            inner,
            events: Mutex::new(Some(rx)),
        })
    }

    /// Whether `sliver-agent` is on the bus right now.
    pub fn agent_running(&self) -> bool {
        DBusProxy::new(&self.inner.conn)
            .and_then(|p| Ok(p.name_has_owner(BUS_NAME.try_into()?)?))
            .unwrap_or(false)
    }

    /// Create a context. If the agent isn't running, the context is kept locally and published
    /// as soon as it starts.
    pub fn context(&self, options: ContextOptions) -> Result<ContextHandle> {
        let mut l = Local {
            options,
            items: Vec::new(),
            visible: true,
            remote: None,
        };
        match self.inner.create_remote(&l) {
            Ok(p) => l.remote = Some(p),
            Err(Error::MethodError(name, _, _))
                if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" => {}
            Err(e) => return Err(e),
        }
        let id = {
            let mut s = self.inner.lock();
            s.next += 1;
            let id = s.next;
            s.contexts.insert(id, l);
            id
        };
        Ok(ContextHandle {
            inner: self.inner.clone(),
            id,
        })
    }

    /// The event stream (one per client; the second call gets an empty receiver).
    pub fn events(&self) -> Receiver<Event> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| channel().1)
    }
}

/// A published context. Dropping the handle keeps the context. Call [`ContextHandle::destroy`].
#[derive(Clone)]
pub struct ContextHandle {
    inner: Arc<Inner>,
    id: u32,
}

impl ContextHandle {
    /// Local id, matching [`Event::context`].
    pub fn id(&self) -> u32 {
        self.id
    }

    fn remote(&self) -> Option<OwnedObjectPath> {
        self.inner
            .lock()
            .contexts
            .get(&self.id)
            .and_then(|l| l.remote.clone())
    }

    /// Replace all items. Returns the agent's warnings (unknown keys/types, clamped values).
    pub fn set_items(&self, items: Vec<Item>) -> Result<Vec<String>> {
        let warnings = match self.remote() {
            Some(p) => {
                let list: Vec<&Props> = items.iter().map(|i| &i.0).collect();
                self.inner
                    .call(&p, "SetItems", &(list,))?
                    .body()
                    .deserialize()?
            }
            None => Vec::new(),
        };
        if let Some(l) = self.inner.lock().contexts.get_mut(&self.id) {
            l.items = items;
        }
        Ok(warnings)
    }

    /// Change some properties of one item.
    pub fn update(&self, item: &str, props: Item) -> Result<Vec<String>> {
        let warnings = match self.remote() {
            Some(p) => {
                let changes = HashMap::from([(item.to_owned(), &props.0)]);
                self.inner
                    .call(&p, "Update", &(changes,))?
                    .body()
                    .deserialize()?
            }
            None => Vec::new(),
        };
        // Keep the local copy current so a republish shows the latest state.
        if let Some(l) = self.inner.lock().contexts.get_mut(&self.id) {
            merge(&mut l.items, item, &props);
        }
        Ok(warnings)
    }

    pub fn set_visible(&self, visible: bool) -> Result<()> {
        if let Some(p) = self.remote() {
            self.inner.call(&p, "SetVisible", &(visible,))?;
        }
        if let Some(l) = self.inner.lock().contexts.get_mut(&self.id) {
            l.visible = visible;
        }
        Ok(())
    }

    pub fn destroy(self) -> Result<()> {
        let l = self.inner.lock().contexts.remove(&self.id);
        if let Some(p) = l.and_then(|l| l.remote) {
            self.inner.call(&p, "Destroy", &())?;
        }
        Ok(())
    }
}

/// Apply an update to the cached items (top level or one level down).
fn merge(items: &mut [Item], id: &str, props: &Item) {
    for it in items.iter_mut() {
        if it.id().as_deref() == Some(id) {
            for (k, v) in &props.0 {
                it.0.insert(k.clone(), v.try_clone().expect("plain value"));
            }
            return;
        }
        let Some(children) = it.0.get("items") else {
            continue;
        };
        let Ok(mut kids) = <Vec<Props>>::try_from(children.try_clone().expect("plain value"))
        else {
            continue;
        };
        let mut wrapped: Vec<Item> = kids.drain(..).map(Item).collect();
        if wrapped.iter().any(|c| c.id().as_deref() == Some(id)) {
            merge(&mut wrapped, id, props);
            let back: Vec<Props> = wrapped.into_iter().map(|i| i.0).collect();
            it.0.insert("items".into(), ov(Value::from(back)));
            return;
        }
    }
}

fn spawn_event_thread(inner: Arc<Inner>, tx: Sender<Event>) -> Result<()> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(IFACE_CONTEXT)?
        .member("Event")?
        .build();
    let it = MessageIterator::for_match_rule(rule, &inner.conn, Some(64))?;
    let weak = Arc::downgrade(&inner);
    drop(inner);
    std::thread::Builder::new()
        .name("sliver-client-events".into())
        .spawn(move || {
            for msg in it {
                let Ok(msg) = msg else { break };
                let Some(inner) = weak.upgrade() else { break };
                let header = msg.header();
                let Some(path) = header.path() else { continue };
                let Ok((item, kind, data)) =
                    msg.body()
                        .deserialize::<(String, String, HashMap<String, OwnedValue>)>()
                else {
                    continue;
                };
                let id = inner
                    .lock()
                    .contexts
                    .iter()
                    .find(|(_, l)| {
                        l.remote
                            .as_ref()
                            .is_some_and(|r| r.as_str() == path.as_str())
                    })
                    .map(|(id, _)| *id);
                if let Some(context) = id
                    && tx
                        .send(Event {
                            context,
                            item,
                            kind,
                            data,
                        })
                        .is_err()
                {
                    break;
                }
            }
        })
        .map_err(|e| Error::Failure(e.to_string()))?;
    Ok(())
}

fn spawn_owner_thread(inner: Arc<Inner>) -> Result<()> {
    let dbus = DBusProxy::new(&inner.conn)?;
    let changes = dbus.receive_name_owner_changed_with_args(&[(0, BUS_NAME)])?;
    let weak = Arc::downgrade(&inner);
    drop(inner);
    std::thread::Builder::new()
        .name("sliver-client-owner".into())
        .spawn(move || {
            for sig in changes {
                let Some(inner) = weak.upgrade() else { break };
                let Ok(args) = sig.args() else { continue };
                if args.new_owner().is_some() {
                    inner.republish();
                } else {
                    for l in inner.lock().contexts.values_mut() {
                        l.remote = None;
                    }
                }
            }
        })
        .map_err(|e| Error::Failure(e.to_string()))?;
    Ok(())
}
