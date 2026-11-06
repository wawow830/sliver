//! The sans-IO model behind the public API: clients, contexts, items, who fills the bar, events.
//!
//! The zbus layer ([`crate::service`]) only translates D-Bus calls into these methods. All policy
//! (ownership, tiers, limits, precedence, optimistic state) lives here and is unit-tested.

use crate::identity::{ProcIdentity, ProcInfo};
use crate::item::{self, Gesture, Item, ItemKind, ParseError, Prop, Props, Warnings};
use sliver_desktop::Window;
use sliver_ipc::dbus::{CONTEXT_PATH_PREFIX, limits};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContextId(pub u32);

impl ContextId {
    pub fn path(self) -> String {
        format!("{CONTEXT_PATH_PREFIX}/{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    AccessDenied(String),
    LimitExceeded(String),
    InvalidArgs(String),
    Busy(String),
    UnknownContext,
}

impl From<ParseError> for ApiError {
    fn from(e: ParseError) -> Self {
        match e {
            ParseError::Invalid(m) => ApiError::InvalidArgs(m),
            ParseError::Limit(m) => ApiError::LimitExceeded(m),
        }
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(now: Instant) -> Bucket {
        Bucket {
            tokens: 2.0 * limits::CALLS_PER_SEC as f64,
            last: now,
        }
    }
    fn take(&mut self, now: Instant) -> bool {
        let rate = limits::CALLS_PER_SEC as f64;
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + dt * rate).min(2.0 * rate);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
pub struct Client {
    pub name: String,
    pub pid: Option<u32>,
    pub identity: ProcIdentity,
    bucket: Bucket,
}

#[derive(Debug)]
pub struct Context {
    pub id: ContextId,
    pub owner: String,
    pub opts: ContextOptions,
    /// Stable provider id: the option, else the verified app id, else the unique name.
    pub provider: String,
    pub items: Vec<Item>,
    pub visible: bool,
    pub on_screen: bool,
    seq: u64,
}

impl Context {
    pub fn find(&self, id: &str) -> Option<&Item> {
        self.items.iter().flat_map(Item::walk).find(|i| i.id == id)
    }
    fn find_mut(&mut self, id: &str) -> Option<&mut Item> {
        for it in &mut self.items {
            if it.id == id {
                return Some(it);
            }
            if let ItemKind::Group { items } | ItemKind::Popover { items } = &mut it.kind
                && let Some(c) = items.iter_mut().find(|c| c.id == id)
            {
                return Some(c);
            }
        }
        None
    }
}

#[derive(Debug, Default)]
pub struct Registry {
    clients: HashMap<String, Client>,
    contexts: BTreeMap<ContextId, Context>,
    next_id: u32,
    seq: u64,
    generation: u64,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bumped on every change that can affect what's drawn.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn context(&self, id: ContextId) -> Option<&Context> {
        self.contexts.get(&id)
    }

    pub fn contexts(&self) -> impl Iterator<Item = &Context> {
        self.contexts.values()
    }

    pub fn client(&self, name: &str) -> Option<&Client> {
        self.clients.get(name)
    }

    /// Register a client (idempotent). The service calls it with credentials from the bus.
    pub fn connect(&mut self, name: &str, pid: Option<u32>, identity: ProcIdentity, now: Instant) {
        self.clients
            .entry(name.to_owned())
            .or_insert_with(|| Client {
                name: name.to_owned(),
                pid,
                identity,
                bucket: Bucket::new(now),
            });
    }

    fn admit(&mut self, client: &str, now: Instant) -> Result<&Client, ApiError> {
        let c = self
            .clients
            .get_mut(client)
            .ok_or_else(|| ApiError::AccessDenied("unknown client".into()))?;
        if !c.bucket.take(now) {
            return Err(ApiError::LimitExceeded(format!(
                "more than {} calls/s",
                limits::CALLS_PER_SEC
            )));
        }
        Ok(c)
    }

    fn owned(&mut self, client: &str, ctx: ContextId) -> Result<&mut Context, ApiError> {
        match self.contexts.get_mut(&ctx) {
            None => Err(ApiError::UnknownContext),
            Some(c) if c.owner != client => Err(ApiError::AccessDenied("not your context".into())),
            Some(c) => Ok(c),
        }
    }

    pub fn create_context(
        &mut self,
        client: &str,
        options: &Props,
        now: Instant,
    ) -> Result<(ContextId, Warnings), ApiError> {
        let mut w = Warnings::new();
        let opts = parse_options(options, &mut w)?;
        let c = self.admit(client, now)?;
        if c.identity.sandboxed() && matches!(opts.scope, Scope::Target(_)) {
            return Err(ApiError::AccessDenied(
                "sandboxed apps can't target other apps".into(),
            ));
        }
        let provider = opts
            .provider
            .clone()
            .or_else(|| c.identity.verified_app_id().map(str::to_owned))
            .unwrap_or_else(|| client.to_owned());
        // A sandboxed app can't claim another provider's identity (and thus its user prefs).
        if let (Some(app), Some(p)) = (c.identity.verified_app_id(), &opts.provider)
            && p != app
            && !p.starts_with(&format!("{app}."))
        {
            return Err(ApiError::AccessDenied(format!(
                "provider must be {app} or {app}.*"
            )));
        }
        let owned = self.contexts.values().filter(|x| x.owner == client).count();
        if owned >= limits::CONTEXTS_PER_CLIENT {
            return Err(ApiError::LimitExceeded(format!(
                "more than {} contexts",
                limits::CONTEXTS_PER_CLIENT
            )));
        }
        self.next_id += 1;
        self.seq += 1;
        let id = ContextId(self.next_id);
        self.contexts.insert(
            id,
            Context {
                id,
                owner: client.to_owned(),
                opts,
                provider,
                items: Vec::new(),
                visible: true,
                on_screen: false,
                seq: self.seq,
            },
        );
        self.generation += 1;
        Ok((id, w))
    }

    pub fn set_items(
        &mut self,
        client: &str,
        ctx: ContextId,
        list: &[Props],
        now: Instant,
    ) -> Result<Warnings, ApiError> {
        self.admit(client, now)?;
        let c = self.owned(client, ctx)?;
        let mut w = Warnings::new();
        c.items = item::parse_items(list, &mut w)?;
        self.generation += 1;
        Ok(w)
    }

    pub fn update(
        &mut self,
        client: &str,
        ctx: ContextId,
        changes: &BTreeMap<String, Props>,
        now: Instant,
    ) -> Result<Warnings, ApiError> {
        self.admit(client, now)?;
        let c = self.owned(client, ctx)?;
        let mut w = Warnings::new();
        // Atomic: apply to a copy, commit only if every change is acceptable.
        let mut scratch = Context {
            items: c.items.clone(),
            ..clone_shell(c)
        };
        for (id, props) in changes {
            match scratch.find_mut(id) {
                Some(it) => item::apply_update(it, props, &mut w)?,
                None => w.push(format!("no item {id:?}")),
            }
        }
        item::check_tree(&scratch.items)?;
        c.items = scratch.items;
        self.generation += 1;
        Ok(w)
    }

    pub fn set_visible(
        &mut self,
        client: &str,
        ctx: ContextId,
        visible: bool,
        now: Instant,
    ) -> Result<(), ApiError> {
        self.admit(client, now)?;
        self.seq += 1;
        let seq = self.seq;
        let c = self.owned(client, ctx)?;
        if visible && !c.visible {
            c.seq = seq;
        }
        c.visible = visible;
        self.generation += 1;
        Ok(())
    }

    pub fn destroy(&mut self, client: &str, ctx: ContextId) -> Result<(), ApiError> {
        self.owned(client, ctx)?;
        self.contexts.remove(&ctx);
        self.generation += 1;
        Ok(())
    }

    /// The client's connection is gone (`NameOwnerChanged`). Returns the removed contexts.
    pub fn client_gone(&mut self, client: &str) -> Vec<ContextId> {
        self.clients.remove(client);
        let gone: Vec<ContextId> = self
            .contexts
            .values()
            .filter(|c| c.owner == client)
            .map(|c| c.id)
            .collect();
        for id in &gone {
            self.contexts.remove(id);
        }
        if !gone.is_empty() {
            self.generation += 1;
        }
        gone
    }

    /// Record what the agent actually drew. Returns `(context, on_screen)` for each change
    /// (to emit `OnScreen` property changes).
    pub fn set_on_screen(&mut self, drawn: &HashSet<ContextId>) -> Vec<(ContextId, bool)> {
        let mut changed = Vec::new();
        for c in self.contexts.values_mut() {
            let now = drawn.contains(&c.id);
            if c.on_screen != now {
                c.on_screen = now;
                changed.push((c.id, now));
            }
        }
        changed
    }
}

fn clone_shell(c: &Context) -> Context {
    Context {
        id: c.id,
        owner: c.owner.clone(),
        opts: c.opts.clone(),
        provider: c.provider.clone(),
        items: Vec::new(),
        visible: c.visible,
        on_screen: c.on_screen,
        seq: c.seq,
    }
}

mod focus;
mod interact;
mod options;

pub use focus::*;
pub use interact::*;
pub use options::*;

#[cfg(test)]
mod tests;
