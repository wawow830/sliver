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

#[derive(Debug, Clone)]
pub struct Target {
    pub app_id: Option<String>,
    pub workspace: Option<String>,
    pub title: Option<regex_lite::Regex>,
}

#[derive(Debug, Clone)]
pub enum Scope {
    App,
    Target(Target),
    Status,
}

impl Scope {
    pub fn name(&self) -> &'static str {
        match self {
            Scope::App => "app",
            Scope::Target(_) => "target",
            Scope::Status => "status",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextOptions {
    pub scope: Scope,
    pub provider: Option<String>,
    pub title: Option<String>,
    pub window: Option<String>,
    pub include_children: bool,
    pub modal: bool,
}

pub fn parse_options(p: &Props, w: &mut Warnings) -> Result<ContextOptions, ApiError> {
    let st = |k: &str| match p.get(k) {
        Some(Prop::Str(s)) => Ok(Some(s.clone())),
        None => Ok(None),
        Some(_) => Err(ApiError::InvalidArgs(format!(
            "option {k:?} must be a string"
        ))),
    };
    let flag = |k: &str| match p.get(k) {
        Some(Prop::Bool(b)) => Ok(*b),
        None => Ok(false),
        Some(_) => Err(ApiError::InvalidArgs(format!(
            "option {k:?} must be a boolean"
        ))),
    };
    let scope = match st("scope")?.as_deref() {
        None | Some("app") => Scope::App,
        Some("status") => Scope::Status,
        Some("target") => {
            let Some(Prop::Map(t)) = p.get("target") else {
                return Err(ApiError::InvalidArgs(
                    "scope \"target\" needs a \"target\" a{sv} selector".into(),
                ));
            };
            let get = |k: &str| {
                t.get(k).and_then(|v| {
                    if let Prop::Str(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
            };
            let title = match get("title-regex") {
                Some(r) if r.len() <= 256 => Some(
                    regex_lite::Regex::new(&r)
                        .map_err(|e| ApiError::InvalidArgs(format!("title-regex: {e}")))?,
                ),
                Some(_) => return Err(ApiError::InvalidArgs("title-regex too long".into())),
                None => None,
            };
            let target = Target {
                app_id: get("app-id"),
                workspace: get("workspace"),
                title,
            };
            if target.app_id.is_none() && target.workspace.is_none() && target.title.is_none() {
                return Err(ApiError::InvalidArgs("empty target selector".into()));
            }
            Scope::Target(target)
        }
        Some(other) => return Err(ApiError::InvalidArgs(format!("unknown scope {other:?}"))),
    };
    for k in p.keys() {
        if ![
            "scope",
            "provider",
            "title",
            "window",
            "target",
            "include-children",
            "modal",
        ]
        .contains(&k.as_str())
        {
            w.push(format!("unknown option {k:?} ignored"));
        }
    }
    let provider = st("provider")?;
    if let Some(pr) = &provider
        && (pr.is_empty()
            || pr.len() > 255
            || pr.contains('/')
            || pr.chars().any(char::is_whitespace))
    {
        return Err(ApiError::InvalidArgs(
            "provider must be a reverse-DNS id".into(),
        ));
    }
    let opts = ContextOptions {
        provider,
        title: st("title")?.map(|t| t.chars().filter(|c| !c.is_control()).take(64).collect()),
        window: st("window")?,
        include_children: flag("include-children")?,
        modal: flag("modal")?,
        scope,
    };
    if !matches!(opts.scope, Scope::App)
        && (opts.modal || opts.include_children || opts.window.is_some())
    {
        w.push("modal/include-children/window only apply to app scope, ignored".into());
    }
    Ok(opts)
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

/// What the agent knows about the desktop right now.
#[derive(Debug, Clone, Default)]
pub struct Focus {
    pub window: Option<Window>,
    pub workspace: Option<String>,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AppSource {
    #[default]
    Auto,
    /// No app controls for this app.
    Disabled,
    /// Only this provider may fill App controls for this app.
    Provider(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPolicy {
    pub source: AppSource,
    /// "May take the whole bar" (docs/ux.md §6). Default on.
    pub allow_modal: bool,
}

impl Default for AppPolicy {
    fn default() -> Self {
        AppPolicy {
            source: AppSource::Auto,
            allow_modal: true,
        }
    }
}

/// The user's preferences that affect published items (from config).
#[derive(Debug, Clone)]
pub struct Policy {
    pub allow_app: bool,
    pub allow_target: bool,
    /// Keyed by window class / app id, matched case-insensitively. `"*"` sets the default for
    /// apps without their own entry (config `[apps."*"]`).
    pub per_app: HashMap<String, AppPolicy>,
    pub hidden_providers: HashSet<String>,
    /// Status providers in the user's order. Unlisted ones follow, sorted by id.
    pub status_order: Vec<String>,
    /// Sandboxed app ids allowed to publish `status` contexts.
    pub sandbox_status_allowed: HashSet<String>,
}

impl Policy {
    /// The policy for a focused app: its own entry (case-insensitive), else `"*"`, else defaults.
    pub fn for_app(&self, class: &str) -> AppPolicy {
        self.per_app
            .get(class)
            .or_else(|| {
                self.per_app
                    .iter()
                    .find(|(k, _)| k.as_str() != "*" && k.eq_ignore_ascii_case(class))
                    .map(|(_, v)| v)
            })
            .or_else(|| self.per_app.get("*"))
            .cloned()
            .unwrap_or_default()
    }
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            allow_app: true,
            allow_target: true,
            per_app: HashMap::new(),
            hidden_providers: HashSet::new(),
            status_order: Vec::new(),
            sandbox_status_allowed: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolved {
    /// Fills App controls.
    pub app: Option<ContextId>,
    /// `app` is modal and allowed to cover Main + Controls.
    pub modal: bool,
    /// Status items, in order.
    pub status: Vec<ContextId>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Begin,
    Change,
    End,
}

/// Something the user did to a published item, after the agent's gesture recognition and after
/// the user's own gesture bindings decided the app should hear about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Interaction {
    Gesture(Gesture),
    Slider {
        value: f64,
        phase: Phase,
    },
    Select(i32),
    Popover(bool),
    /// The ✕ on a modal context.
    CloseModal,
}

/// An `Event` signal to send, unicast to `destination`.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub destination: String,
    pub context: ContextId,
    pub item: String,
    pub kind: &'static str,
    pub data: Props,
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

    /// Who fills App controls and Status items (`docs/api.md` §4).
    pub fn resolve(&self, focus: &Focus, policy: &Policy, proc: &dyn ProcInfo) -> Resolved {
        if focus.locked {
            return Resolved::default();
        }
        let live = |c: &&Context| c.visible && !policy.hidden_providers.contains(&c.provider);

        // Status items.
        let mut status: Vec<&Context> = self
            .contexts
            .values()
            .filter(live)
            .filter(|c| matches!(c.opts.scope, Scope::Status))
            .filter(|c| {
                let id = &self
                    .clients
                    .get(&c.owner)
                    .map(|cl| cl.identity.clone())
                    .unwrap_or_default();
                !id.sandboxed()
                    || id
                        .verified_app_id()
                        .is_some_and(|a| policy.sandbox_status_allowed.contains(a))
            })
            .collect();
        let rank = |p: &str| {
            policy
                .status_order
                .iter()
                .position(|x| x == p)
                .unwrap_or(usize::MAX)
        };
        status.sort_by(|a, b| {
            (rank(&a.provider), &a.provider, a.id).cmp(&(rank(&b.provider), &b.provider, b.id))
        });
        let status = status.into_iter().map(|c| c.id).collect();

        // App controls.
        let Some(win) = &focus.window else {
            return Resolved {
                app: None,
                modal: false,
                status,
            };
        };
        let app_policy = policy.for_app(&win.class);
        if !policy.allow_app || app_policy.source == AppSource::Disabled {
            return Resolved {
                app: None,
                modal: false,
                status,
            };
        }
        let win_identity = win.pid.map(|p| proc.identity(p));
        let mut best: Option<(u8, u64, &Context)> = None;
        for c in self.contexts.values().filter(live) {
            if let AppSource::Provider(p) = &app_policy.source
                && &c.provider != p
            {
                continue;
            }
            let Some(rank) = self.app_rank(c, win, win_identity.as_ref(), focus, policy, proc)
            else {
                continue;
            };
            if best.is_none_or(|(r, s, _)| {
                (rank, std::cmp::Reverse(c.seq)) < (r, std::cmp::Reverse(s))
            }) {
                best = Some((rank, c.seq, c));
            }
        }
        let app = best.map(|b| b.2);
        Resolved {
            app: app.map(|c| c.id),
            modal: app.is_some_and(|c| {
                c.opts.modal && matches!(c.opts.scope, Scope::App) && app_policy.allow_modal
            }),
            status,
        }
    }

    /// Lower is better: 0 window-bound, 1 same app, 2 include-children, 3 target. `None` = no match.
    fn app_rank(
        &self,
        c: &Context,
        win: &Window,
        win_identity: Option<&ProcIdentity>,
        focus: &Focus,
        policy: &Policy,
        proc: &dyn ProcInfo,
    ) -> Option<u8> {
        let client = self.clients.get(&c.owner)?;
        match &c.opts.scope {
            Scope::Status => None,
            Scope::Target(t) => {
                if !policy.allow_target || client.identity.sandboxed() {
                    return None;
                }
                let ok = t
                    .app_id
                    .as_ref()
                    .is_none_or(|a| a.eq_ignore_ascii_case(&win.class))
                    && t.workspace
                        .as_ref()
                        .is_none_or(|ws| focus.workspace.as_ref() == Some(ws))
                    && t.title.as_ref().is_none_or(|re| re.is_match(&win.title));
                ok.then_some(3)
            }
            Scope::App => {
                let (cpid, wpid) = (client.pid?, win.pid?);
                let same_app = cpid == wpid
                    || win_identity.is_some_and(|wi| wi.same_instance(&client.identity));
                if same_app {
                    return match c.opts.window.as_deref().and_then(hyprland_address) {
                        Some(addr) if addr == hyprland_norm(&win.address) => Some(0),
                        Some(_) => None, // bound to another window of this app
                        None => Some(1), // unbound, or a handle we can't resolve: app-level
                    };
                }
                (c.opts.include_children && proc.is_ancestor(wpid, cpid)).then_some(2)
            }
        }
    }

    /// Apply an interaction: update optimistic state and return the event for the owner, if any.
    pub fn interact(
        &mut self,
        ctx: ContextId,
        item_id: &str,
        what: Interaction,
    ) -> Option<Outgoing> {
        let c = self.contexts.get_mut(&ctx)?;
        let owner = c.owner.clone();
        let out = |item: &str, kind: &'static str, data: Props| Outgoing {
            destination: owner.clone(),
            context: ctx,
            item: item.to_owned(),
            kind,
            data,
        };
        if let Interaction::CloseModal = what {
            if !c.opts.modal {
                return None;
            }
            c.visible = false;
            self.generation += 1;
            return Some(out("", "closed", Props::new()));
        }
        let it = c.find_mut(item_id)?;
        if !it.enabled || !it.visible {
            return None;
        }
        let one = |k: &str, v: Prop| Props::from([(k.to_owned(), v)]);
        let ev = match (what, &mut it.kind) {
            (Interaction::Gesture(Gesture::Tap), ItemKind::Toggle { state }) => {
                *state = !*state;
                out(item_id, "toggled", one("state", Prop::Bool(*state)))
            }
            (Interaction::Gesture(Gesture::Tap), ItemKind::Button) => {
                out(item_id, "tap", Props::new())
            }
            (Interaction::Gesture(g), k)
                if g != Gesture::Tap && g != Gesture::Drag && interactive(k) =>
            {
                if !it.gestures.contains(&g) {
                    return None;
                }
                out(item_id, g.name(), Props::new())
            }
            (
                Interaction::Slider { value, phase },
                ItemKind::Slider {
                    value: v,
                    min,
                    max,
                    step,
                },
            ) => {
                let mut x = value.clamp(*min, *max);
                if *step > 0.0 {
                    x = (*min + ((x - *min) / *step).round() * *step).clamp(*min, *max);
                }
                *v = x;
                let phase = match phase {
                    Phase::Begin => "begin",
                    Phase::Change => "change",
                    Phase::End => "end",
                };
                let mut d = one("value", Prop::Num(x));
                d.insert("phase".into(), Prop::Str(phase.into()));
                out(item_id, "value", d)
            }
            (Interaction::Select(i), ItemKind::Segmented { segments, selected }) => {
                if i < -1 || i >= segments.len() as i32 {
                    return None;
                }
                *selected = i;
                out(item_id, "selected", one("index", Prop::Int(i.into())))
            }
            (Interaction::Popover(open), ItemKind::Popover { .. }) => {
                out(item_id, "popover", one("open", Prop::Bool(open)))
            }
            _ => return None,
        };
        self.generation += 1;
        Some(ev)
    }

    /// `Control1.Signal(n)`: one `signal` event to every listed context.
    pub fn signal(&self, contexts: &[ContextId], n: u32) -> Vec<Outgoing> {
        contexts
            .iter()
            .filter_map(|id| self.contexts.get(id))
            .map(|c| Outgoing {
                destination: c.owner.clone(),
                context: c.id,
                item: String::new(),
                kind: "signal",
                data: Props::from([("n".to_owned(), Prop::Int(n.into()))]),
            })
            .collect()
    }
}

fn interactive(k: &ItemKind) -> bool {
    !matches!(k, ItemKind::Spacer { .. } | ItemKind::Group { .. })
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

fn hyprland_norm(a: &str) -> String {
    a.trim_start_matches("0x").to_ascii_lowercase()
}

/// `hyprland:0xabc` → `abc`. Other handle kinds aren't resolvable yet.
fn hyprland_address(handle: &str) -> Option<String> {
    handle.strip_prefix("hyprland:").map(hyprland_norm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::tests::FakeProc;
    use crate::item::tests::{button, props, s};
    use std::time::Duration;

    struct World {
        reg: Registry,
        proc: FakeProc,
        now: Instant,
    }

    impl World {
        fn new() -> World {
            World {
                reg: Registry::new(),
                proc: FakeProc::default(),
                now: Instant::now(),
            }
        }
        fn client(&mut self, name: &str, pid: u32, id: ProcIdentity) {
            self.proc.add(pid, 1, id.clone());
            self.reg.connect(name, Some(pid), id, self.now);
        }
        fn ctx(&mut self, client: &str, opts: &[(&str, Prop)]) -> ContextId {
            let id = self
                .reg
                .create_context(client, &props(opts), self.now)
                .unwrap()
                .0;
            self.reg
                .set_items(client, id, &[button("b")], self.now)
                .unwrap();
            id
        }
        fn focus(&self, class: &str, pid: u32) -> Focus {
            Focus {
                window: Some(Window {
                    address: "abc".into(),
                    class: class.into(),
                    title: "Doc - Editor".into(),
                    pid: Some(pid),
                    ..Default::default()
                }),
                workspace: Some("2".into()),
                locked: false,
            }
        }
        fn resolve(&self, f: &Focus, p: &Policy) -> Resolved {
            self.reg.resolve(f, p, &self.proc)
        }
    }

    fn flatpak(app: &str, inst: &str) -> ProcIdentity {
        ProcIdentity {
            flatpak: Some((app.into(), inst.into())),
            ..Default::default()
        }
    }

    #[test]
    fn own_app_context_follows_focus() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        let c = w.ctx(":1.1", &[]);
        let p = Policy::default();
        assert_eq!(w.resolve(&w.focus("editor", 100), &p).app, Some(c));
        assert_eq!(w.resolve(&w.focus("other", 200), &p).app, None);
        // Claiming an app id doesn't help: binding is by pid/instance only.
        w.client(":1.2", 300, ProcIdentity::default());
        let _ = w.ctx(":1.2", &[("provider", s("editor"))]);
        assert_eq!(w.resolve(&w.focus("editor", 100), &p).app, Some(c));
    }

    #[test]
    fn same_flatpak_instance_or_app_unit_counts() {
        let mut w = World::new();
        w.client(":1.1", 101, flatpak("org.gimp.GIMP", "7"));
        w.proc.add(100, 1, flatpak("org.gimp.GIMP", "7"));
        let c = w.ctx(":1.1", &[]);
        assert_eq!(
            w.resolve(&w.focus("gimp", 100), &Policy::default()).app,
            Some(c)
        );
        assert_eq!(w.reg.context(c).unwrap().provider, "org.gimp.GIMP");
    }

    #[test]
    fn precedence_window_app_children_target() {
        let mut w = World::new();
        let p = Policy::default();
        w.client(":1.term", 100, ProcIdentity::default());
        w.client(":1.vim", 150, ProcIdentity::default());
        w.proc
            .add(150, 120, ProcIdentity::default())
            .add(120, 100, ProcIdentity::default());
        w.client(":1.script", 900, ProcIdentity::default());
        let target = w.ctx(
            ":1.script",
            &[
                ("scope", s("target")),
                ("target", Prop::Map(props(&[("app-id", s("Term"))]))),
            ],
        );
        let f = w.focus("term", 100);
        assert_eq!(w.resolve(&f, &p).app, Some(target));
        let vim = w.ctx(":1.vim", &[("include-children", Prop::Bool(true))]);
        assert_eq!(w.resolve(&f, &p).app, Some(vim));
        let term = w.ctx(":1.term", &[]);
        assert_eq!(w.resolve(&f, &p).app, Some(term));
        let win = w.ctx(":1.term", &[("window", s("hyprland:0xABC"))]);
        assert_eq!(w.resolve(&f, &p).app, Some(win));
        // Bound to another window: never shown for this one.
        let _other = w.ctx(":1.term", &[("window", s("hyprland:0xdef"))]);
        assert_eq!(w.resolve(&f, &p).app, Some(win));
        // Users can disable targets.
        w.reg.destroy(":1.term", win).unwrap();
        w.reg.destroy(":1.term", term).unwrap();
        w.reg.destroy(":1.vim", vim).unwrap();
        let no_targets = Policy {
            allow_target: false,
            ..Policy::default()
        };
        assert_eq!(w.resolve(&f, &no_targets).app, None);
    }

    #[test]
    fn most_recently_shown_wins_within_a_tier() {
        let mut w = World::new();
        let p = Policy::default();
        w.client(":1.1", 100, ProcIdentity::default());
        let a = w.ctx(":1.1", &[]);
        let b = w.ctx(":1.1", &[]);
        let f = w.focus("x", 100);
        assert_eq!(w.resolve(&f, &p).app, Some(b));
        w.reg.set_visible(":1.1", a, false, w.now).unwrap();
        w.reg.set_visible(":1.1", a, true, w.now).unwrap();
        assert_eq!(w.resolve(&f, &p).app, Some(a));
        w.reg.set_visible(":1.1", a, false, w.now).unwrap();
        assert_eq!(w.resolve(&f, &p).app, Some(b));
    }

    #[test]
    fn user_policy_wins() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        w.client(":1.2", 900, ProcIdentity::default());
        let own = w.ctx(":1.1", &[("provider", s("org.app"))]);
        let tgt = w.ctx(
            ":1.2",
            &[
                ("provider", s("org.script")),
                ("scope", s("target")),
                ("target", Prop::Map(props(&[("workspace", s("2"))]))),
            ],
        );
        let f = w.focus("x", 100);
        let mut p = Policy::default();
        p.per_app.insert(
            "x".into(),
            AppPolicy {
                source: AppSource::Provider("org.script".into()),
                allow_modal: true,
            },
        );
        assert_eq!(w.resolve(&f, &p).app, Some(tgt));
        p.per_app.insert(
            "x".into(),
            AppPolicy {
                source: AppSource::Disabled,
                allow_modal: true,
            },
        );
        assert_eq!(w.resolve(&f, &p).app, None);
        let p = Policy {
            hidden_providers: HashSet::from(["org.app".to_owned()]),
            ..Policy::default()
        };
        assert_eq!(w.resolve(&f, &p).app, Some(tgt));
        let _ = own;
        // Locked: nothing at all.
        let mut locked = f.clone();
        locked.locked = true;
        assert_eq!(w.resolve(&locked, &Policy::default()), Resolved::default());
    }

    #[test]
    fn app_policy_lookup_is_case_insensitive_with_default() {
        let mut p = Policy::default();
        let off = AppPolicy {
            source: AppSource::Disabled,
            allow_modal: true,
        };
        let no_modal = AppPolicy {
            source: AppSource::Auto,
            allow_modal: false,
        };
        p.per_app.insert("*".into(), no_modal.clone());
        p.per_app.insert("Firefox".into(), off.clone());
        assert_eq!(p.for_app("firefox"), off);
        assert_eq!(p.for_app("FIREFOX"), off);
        assert_eq!(p.for_app("foot"), no_modal);
        assert_eq!(Policy::default().for_app("foot"), AppPolicy::default());
    }

    #[test]
    fn modal_needs_scope_and_permission() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        let m = w.ctx(":1.1", &[("modal", Prop::Bool(true))]);
        let f = w.focus("x", 100);
        let r = w.resolve(&f, &Policy::default());
        assert_eq!((r.app, r.modal), (Some(m), true));
        let mut p = Policy::default();
        p.per_app.insert(
            "x".into(),
            AppPolicy {
                source: AppSource::Auto,
                allow_modal: false,
            },
        );
        assert!(!w.resolve(&f, &p).modal);
        let ev = w.reg.interact(m, "", Interaction::CloseModal).unwrap();
        assert_eq!((ev.kind, ev.item.as_str()), ("closed", ""));
        assert_eq!(w.resolve(&f, &Policy::default()).app, None);
    }

    #[test]
    fn status_items_order_and_sandbox_consent() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        w.client(":1.2", 200, ProcIdentity::default());
        w.client(":1.3", 300, flatpak("org.flat.App", "1"));
        let z = w.ctx(":1.1", &[("scope", s("status")), ("provider", s("org.z"))]);
        let a = w.ctx(":1.2", &[("scope", s("status")), ("provider", s("org.a"))]);
        let f = w.ctx(":1.3", &[("scope", s("status"))]);
        let mut p = Policy::default();
        assert_eq!(w.resolve(&Focus::default(), &p).status, vec![a, z]);
        p.status_order = vec!["org.z".into()];
        p.sandbox_status_allowed.insert("org.flat.App".into());
        assert_eq!(w.resolve(&Focus::default(), &p).status, vec![z, a, f]);
    }

    #[test]
    fn sandbox_restrictions() {
        let mut w = World::new();
        w.client(":1.f", 100, flatpak("org.flat.App", "1"));
        let t = props(&[
            ("scope", s("target")),
            ("target", Prop::Map(props(&[("app-id", s("firefox"))]))),
        ]);
        assert!(matches!(
            w.reg.create_context(":1.f", &t, w.now),
            Err(ApiError::AccessDenied(_))
        ));
        let spoof = props(&[("provider", s("org.mozilla.firefox"))]);
        assert!(matches!(
            w.reg.create_context(":1.f", &spoof, w.now),
            Err(ApiError::AccessDenied(_))
        ));
        let sub = props(&[("provider", s("org.flat.App.Player"))]);
        w.reg.create_context(":1.f", &sub, w.now).unwrap();
    }

    #[test]
    fn ownership_limits_and_cleanup() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        w.client(":1.2", 200, ProcIdentity::default());
        let c = w.ctx(":1.1", &[]);
        assert!(matches!(
            w.reg.set_items(":1.2", c, &[], w.now),
            Err(ApiError::AccessDenied(_))
        ));
        assert!(matches!(
            w.reg.destroy(":1.2", c),
            Err(ApiError::AccessDenied(_))
        ));
        assert!(matches!(
            w.reg.create_context(":1.9", &Props::new(), w.now),
            Err(ApiError::AccessDenied(_))
        ));
        for _ in 1..limits::CONTEXTS_PER_CLIENT {
            w.ctx(":1.1", &[]);
        }
        assert!(matches!(
            w.reg.create_context(":1.1", &Props::new(), w.now),
            Err(ApiError::LimitExceeded(_))
        ));
        let before = w.reg.generation();
        assert_eq!(w.reg.client_gone(":1.1").len(), limits::CONTEXTS_PER_CLIENT);
        assert!(w.reg.generation() > before);
        assert_eq!(w.reg.contexts().count(), 0);
    }

    #[test]
    fn rate_limit_refills() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        let c = w.ctx(":1.1", &[]);
        let burst = (0..1000)
            .take_while(|_| w.reg.set_visible(":1.1", c, true, w.now).is_ok())
            .count();
        assert!((200..=240).contains(&burst), "burst {burst}");
        assert!(matches!(
            w.reg.set_visible(":1.1", c, true, w.now),
            Err(ApiError::LimitExceeded(_))
        ));
        let later = w.now + Duration::from_secs(1);
        assert!(w.reg.set_visible(":1.1", c, true, later).is_ok());
    }

    #[test]
    fn update_is_atomic() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        let c = w
            .reg
            .create_context(":1.1", &Props::new(), w.now)
            .unwrap()
            .0;
        w.reg
            .set_items(":1.1", c, &[button("a"), button("b"), button("c")], w.now)
            .unwrap();
        let ok = BTreeMap::from([
            ("a".to_owned(), props(&[("label", s("New"))])),
            ("nope".to_owned(), Props::new()),
        ]);
        let warns = w.reg.update(":1.1", c, &ok, w.now).unwrap();
        assert!(warns.iter().any(|x| x.contains("nope")));
        assert_eq!(
            w.reg
                .context(c)
                .unwrap()
                .find("a")
                .unwrap()
                .label
                .as_deref(),
            Some("New")
        );
        // Three max-size icons exceed the per-context budget: nothing at all is applied.
        let icon = |l: &str| {
            props(&[
                ("label", s(l)),
                ("icon-data", Prop::Bytes(vec![0; limits::ICON_BYTES])),
            ])
        };
        let big = BTreeMap::from([
            ("a".to_owned(), icon("X")),
            ("b".to_owned(), icon("Y")),
            ("c".to_owned(), icon("Z")),
        ]);
        assert!(matches!(
            w.reg.update(":1.1", c, &big, w.now),
            Err(ApiError::LimitExceeded(_))
        ));
        let ctx = w.reg.context(c).unwrap();
        assert_eq!(ctx.find("a").unwrap().label.as_deref(), Some("New"));
        assert!(ctx.find("b").unwrap().icon_data.is_none());
    }

    #[test]
    fn interactions_update_state_and_emit() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        let c = w
            .reg
            .create_context(":1.1", &Props::new(), w.now)
            .unwrap()
            .0;
        let items = [
            props(&[("id", s("t")), ("type", s("toggle"))]),
            props(&[
                ("id", s("v")),
                ("type", s("slider")),
                ("max", Prop::Num(10.0)),
                ("step", Prop::Num(2.0)),
            ]),
            props(&[
                ("id", s("seg")),
                ("type", s("segmented")),
                (
                    "segments",
                    Prop::List(vec![
                        Prop::Map(props(&[("label", s("A"))])),
                        Prop::Map(props(&[("label", s("B"))])),
                    ]),
                ),
            ]),
            props(&[
                ("id", s("b")),
                ("type", s("button")),
                ("gestures", Prop::List(vec![s("hold")])),
            ]),
            props(&[
                ("id", s("off")),
                ("type", s("button")),
                ("enabled", Prop::Bool(false)),
            ]),
        ];
        w.reg.set_items(":1.1", c, &items, w.now).unwrap();
        let ev = w
            .reg
            .interact(c, "t", Interaction::Gesture(Gesture::Tap))
            .unwrap();
        assert_eq!(
            (ev.kind, ev.data.get("state")),
            ("toggled", Some(&Prop::Bool(true)))
        );
        assert_eq!(ev.destination, ":1.1");
        let ev = w
            .reg
            .interact(
                c,
                "v",
                Interaction::Slider {
                    value: 4.9,
                    phase: Phase::Change,
                },
            )
            .unwrap();
        assert_eq!(ev.data.get("value"), Some(&Prop::Num(4.0)));
        assert_eq!(ev.data.get("phase"), Some(&s("change")));
        let ev = w.reg.interact(c, "seg", Interaction::Select(1)).unwrap();
        assert_eq!(ev.data.get("index"), Some(&Prop::Int(1)));
        assert!(w.reg.interact(c, "seg", Interaction::Select(2)).is_none());
        assert_eq!(
            w.reg
                .interact(c, "b", Interaction::Gesture(Gesture::Hold))
                .unwrap()
                .kind,
            "hold"
        );
        assert!(
            w.reg
                .interact(c, "b", Interaction::Gesture(Gesture::SwipeLeft))
                .is_none(),
            "not opted in"
        );
        assert!(
            w.reg
                .interact(c, "off", Interaction::Gesture(Gesture::Tap))
                .is_none()
        );
        assert!(
            w.reg
                .interact(c, "nope", Interaction::Gesture(Gesture::Tap))
                .is_none()
        );
        let sig = w.reg.signal(&[c], 8);
        assert_eq!(
            (sig[0].kind, sig[0].data.get("n")),
            ("signal", Some(&Prop::Int(8)))
        );
    }

    #[test]
    fn on_screen_changes() {
        let mut w = World::new();
        w.client(":1.1", 100, ProcIdentity::default());
        let a = w.ctx(":1.1", &[]);
        let b = w.ctx(":1.1", &[]);
        assert_eq!(w.reg.set_on_screen(&HashSet::from([a])), vec![(a, true)]);
        assert_eq!(
            w.reg.set_on_screen(&HashSet::from([b])),
            vec![(a, false), (b, true)]
        );
        assert!(w.reg.set_on_screen(&HashSet::from([b])).is_empty());
    }

    #[test]
    fn option_errors() {
        let mut w = Warnings::new();
        assert!(parse_options(&props(&[("scope", s("everywhere"))]), &mut w).is_err());
        assert!(parse_options(&props(&[("scope", s("target"))]), &mut w).is_err());
        assert!(
            parse_options(
                &props(&[("scope", s("target")), ("target", Prop::Map(Props::new()))]),
                &mut w
            )
            .is_err()
        );
        assert!(
            parse_options(
                &props(&[
                    ("scope", s("target")),
                    ("target", Prop::Map(props(&[("title-regex", s("(("))])))
                ]),
                &mut w
            )
            .is_err()
        );
        assert!(parse_options(&props(&[("modal", s("yes"))]), &mut w).is_err());
        assert!(parse_options(&props(&[("provider", s("has space"))]), &mut w).is_err());
        parse_options(
            &props(&[
                ("scope", s("status")),
                ("modal", Prop::Bool(true)),
                ("x", Prop::Int(1)),
            ]),
            &mut w,
        )
        .unwrap();
        assert_eq!(w.len(), 2);
    }
}
