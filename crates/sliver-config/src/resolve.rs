//! What the bar shows right now: rule selection, zone inheritance, module instantiation
//! (sliver-config(5)). Runs in the agent.

use std::collections::BTreeMap;
use std::sync::Arc;

use toml::{Table, Value};

use crate::condition::Context;
use crate::diag::Diagnostics;
use crate::model::{Config, ModuleRef, Zone};
use crate::module::{Common, ModuleId, key_defaults};
use crate::options::{Drawer, Kind, Opt};
use crate::values::{Align, Width};

/// Deepest `extends` chain and group nesting we follow.
pub const MAX_DEPTH: usize = 8;

/// Module types that never show while the session is locked (sliver-config(5)).
/// Whatever the layout says: published items, script output, notification text, and window titles
/// (a stale focus can still name the last window, e.g. an email subject).
pub const HIDDEN_WHILE_LOCKED: &[&str] = &[
    "app-controls",
    "status-items",
    "app",
    "custom",
    "graph",
    "image",
    "notifications",
    "window",
    "taskbar",
];

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleInstance {
    pub id: ModuleId,
    /// CSS id (`#custom-vpn`).
    pub css_id: String,
    /// The instance name (waybar puts it on as a class) plus static `class`es.
    pub classes: Vec<String>,
    pub common: Common,
    /// Type-specific keys, read by the accessors below.
    options: BTreeMap<&'static str, Opt>,
    /// Children of a `group/*`, in order.
    pub children: Vec<ModuleInstance>,
}

/// Type-specific keys, by [`crate::MODULE_TYPES`]: `None` when unset. Reading a key the type
/// doesn't declare, or as another kind, is a bug (a debug assertion).
impl ModuleInstance {
    fn opt(&self, key: &str, fits: fn(Kind) -> bool) -> Option<&Opt> {
        debug_assert!(
            crate::options::key_kind(&self.id.kind, key).is_none_or(|k| k.is_some_and(fits)),
            "{} modules don't take {key:?} as read: see MODULE_TYPES",
            self.id.kind
        );
        self.options.get(key)
    }

    pub fn has(&self, key: &str) -> bool {
        self.options.contains_key(key)
    }

    /// Does this module's type take `key` at all (a `clock` is never a slider)?
    pub fn takes(&self, key: &str) -> bool {
        crate::options::takes_key(&self.id.kind, key) == Some(true)
    }

    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.opt(key, |k| k == Kind::Flag)? {
            Opt::Flag(b) => Some(*b),
            _ => None,
        }
    }

    /// [`Kind::Text`] and [`Kind::OneOf`].
    pub fn text(&self, key: &str) -> Option<&str> {
        match self.opt(key, |k| matches!(k, Kind::Text | Kind::OneOf(_)))? {
            Opt::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn texts(&self, key: &str) -> &[String] {
        match self.opt(key, |k| k == Kind::Texts) {
            Some(Opt::Texts(v)) => v,
            _ => &[],
        }
    }

    /// [`Kind::Int`] and [`Kind::Clamped`].
    pub fn int(&self, key: &str) -> Option<i64> {
        match self.opt(key, |k| matches!(k, Kind::Int(..) | Kind::Clamped(..)))? {
            Opt::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        match self.opt(key, |k| k == Kind::Number)? {
            Opt::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn duration(&self, key: &str) -> Option<std::time::Duration> {
        match self.opt(key, |k| matches!(k, Kind::Duration(_)))? {
            Opt::Duration(t) => Some(*t),
            _ => None,
        }
    }

    pub fn module_ref(&self, key: &str) -> Option<&ModuleId> {
        match self.opt(key, |k| k == Kind::Module)? {
            Opt::Module(id) => Some(id),
            _ => None,
        }
    }

    pub fn env(&self, key: &str) -> &[(String, String)] {
        match self.opt(key, |k| k == Kind::Env) {
            Some(Opt::Env(v)) => v,
            _ => &[],
        }
    }

    /// A [`crate::CAVA`] key passed to cava; `None` also where the type's own key of that name
    /// shadows it (`graph`'s `source`).
    pub fn cava(&self, key: &str) -> Option<&str> {
        match self.options.get(key)? {
            Opt::Cava(s) => Some(s),
            _ => None,
        }
    }

    /// A group's `modules`; none for other types.
    pub fn members(&self) -> &[ModuleId] {
        if self.id.kind != "group" {
            return &[];
        }
        match self.opt("modules", |k| k == Kind::Modules) {
            Some(Opt::Modules(v)) => v,
            _ => &[],
        }
    }

    /// A group's `module-width`: the default width of its children.
    pub fn module_width(&self) -> Option<Width> {
        match self.opt("module-width", |k| k == Kind::Width)? {
            Opt::Width(w) => Some(*w),
            _ => None,
        }
    }

    /// A group's `drawer`; `None` for other types.
    pub fn drawer(&self) -> Option<&Drawer> {
        if self.id.kind != "group" {
            return None;
        }
        match self.opt("drawer", |k| k == Kind::Drawer)? {
            Opt::Drawer(dr) => Some(dr),
            _ => None,
        }
    }

    /// `window`'s `rewrite`, its regexes compiled when the config loaded.
    pub fn rewrite(&self) -> Option<&crate::Rewrite> {
        match self.opt("rewrite", |k| k == Kind::Rewrite)? {
            Opt::Rewrite(r) => Some(r),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedZone {
    pub name: String,
    pub width: Width,
    pub align: Align,
    pub class: Vec<String>,
    pub modules: Vec<ModuleInstance>,
}

/// One layer's content for the current context.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub layer: String,
    pub layout: String,
    /// Index into `Config::rules` of the rule that picked the layout, or `None` for the layer
    /// default.
    pub rule: Option<usize>,
    pub zones: Vec<ResolvedZone>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    UnknownLayer(String),
    UnknownLayout(String),
    ExtendsCycle(Vec<String>),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::UnknownLayer(l) => write!(f, "no layer {l:?} in [layers]"),
            ResolveError::UnknownLayout(l) => write!(f, "no layout {l:?}"),
            ResolveError::ExtendsCycle(chain) => {
                write!(f, "layouts extend in a loop: {}", chain.join(" → "))
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// A module's merged block, parsed: what rendering reads. Made when the config loads.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub(crate) instance: ModuleInstance,
    /// The block sets its own width (a zone's `module-width` doesn't apply).
    pub(crate) sized: bool,
    /// What parsing found wrong, at the block's path; `validate` reports it.
    pub(crate) diagnostics: Diagnostics,
}

impl Parsed {
    pub(crate) fn new(id: &ModuleId, block: &Table, path: &str) -> Parsed {
        let sized = block.contains_key("width") || block.contains_key("expand");
        let mut d = Diagnostics::default();
        let (common, rest) = Common::parse(block, path, &mut d);
        let mut options = crate::options::parse(id, &rest, path, &mut d);
        if id.slider {
            options.entry("slider").or_insert(Opt::Flag(true));
        }
        let mut classes: Vec<String> = id.instance.iter().cloned().collect();
        classes.extend(common.class.iter().cloned());
        let instance = ModuleInstance {
            css_id: id.css_id(),
            id: id.clone(),
            classes,
            common,
            options,
            children: Vec::new(),
        };
        Parsed {
            instance,
            sized,
            diagnostics: d,
        }
    }
}

/// Built-in defaults, then the module's `[modules."<id>"]` block, then an inline table.
pub(crate) fn merged_block(id: &ModuleId, named: Option<&Table>, inline: &Table) -> Table {
    let mut block = id.key().map(key_defaults).unwrap_or_default();
    if let Some(b) = named {
        deep_merge(&mut block, b);
    }
    deep_merge(&mut block, inline);
    block
}

impl Config {
    /// Picks the layout for `layer`: the first matching rule top to bottom, else the layer default.
    pub fn select_layout(
        &self,
        layer: &str,
        ctx: &Context,
    ) -> Result<(&str, Option<usize>), ResolveError> {
        if let Some((i, r)) = self
            .rules
            .iter()
            .enumerate()
            .find(|(_, r)| r.layer == layer && r.when.eval(ctx))
        {
            return Ok((&r.layout, Some(i)));
        }
        self.layers
            .get(layer)
            .map(|l| (l.as_str(), None))
            .ok_or_else(|| ResolveError::UnknownLayer(layer.to_owned()))
    }

    /// The zones of a layout after following `extends` (whole zones override by name).
    pub fn layout_zones(&self, name: &str) -> Result<Vec<Zone>, ResolveError> {
        let chain = extends_chain(name, |l| self.layouts.get(l).map(|l| l.extends.clone()))?;
        let levels = chain.iter().rev().map(|n| {
            let l = &self.layouts[n];
            (l.zones.clone(), l.zone_order.clone())
        });
        Ok(inherit_zones(levels))
    }

    /// The merged config block for a module reference: built-in defaults, then
    /// `[modules."<id>"]`, then the inline table.
    pub fn module_block(&self, m: &ModuleRef) -> Table {
        let inline = match m {
            ModuleRef::Inline(_, t, _) => t,
            ModuleRef::Named(_) => &Table::new(),
        };
        merged_block(m.id(), self.modules.get(m.id()), inline)
    }

    /// Every named module the config refers to, parsed: the `[modules]` blocks, the zones' named
    /// modules and the groups' members.
    pub(crate) fn parse_named(&self) -> BTreeMap<ModuleId, Arc<Parsed>> {
        let mut todo: Vec<ModuleId> = self.modules.keys().cloned().collect();
        for m in self
            .layouts
            .values()
            .flat_map(|l| &l.zones)
            .flat_map(|z| &z.modules)
        {
            match m {
                ModuleRef::Named(id) => todo.push(id.clone()),
                // An inline group's members are named.
                ModuleRef::Inline(id, _, p) if id.kind == "group" => {
                    todo.extend(p.instance.members().iter().cloned())
                }
                ModuleRef::Inline(..) => {}
            }
        }
        let mut out = BTreeMap::new();
        while let Some(id) = todo.pop() {
            if out.contains_key(&id) {
                continue;
            }
            let p = Parsed::new(
                &id,
                &merged_block(&id, self.modules.get(&id), &Table::new()),
                &crate::diag::join("modules", &id.to_string()),
            );
            todo.extend(p.instance.members().iter().cloned());
            out.insert(id, Arc::new(p));
        }
        out
    }

    /// Everything one layer shows in this context.
    pub fn resolve(&self, layer: &str, ctx: &Context) -> Result<Resolved, ResolveError> {
        let (layout, rule) = self.select_layout(layer, ctx)?;
        self.resolve_layout(layer, layout, rule, ctx)
    }

    /// `layout` shown on `layer`, whatever the rules would pick (`sliverctl layout`).
    pub fn resolve_as(
        &self,
        layer: &str,
        layout: &str,
        ctx: &Context,
    ) -> Result<Resolved, ResolveError> {
        self.resolve_layout(layer, layout, None, ctx)
    }

    fn resolve_layout(
        &self,
        layer: &str,
        layout: &str,
        rule: Option<usize>,
        ctx: &Context,
    ) -> Result<Resolved, ResolveError> {
        // Modules' `when` can ask which layout they're in. Rules can't: they choose it.
        let ctx = &ctx.clone().with("layout", layout);
        let zones = self
            .layout_zones(layout)?
            .into_iter()
            .map(|z| ResolvedZone {
                modules: z
                    .modules
                    .iter()
                    .filter_map(|m| self.instantiate(m, z.module_width, ctx, &mut Vec::new()))
                    .collect(),
                name: z.name,
                width: z.width,
                align: z.align,
                class: z.class,
            })
            .collect();
        Ok(Resolved {
            layer: layer.to_owned(),
            layout: layout.to_owned(),
            rule,
            zones,
        })
    }

    /// The module `id` as it would be instantiated in a zone for `ctx` (its `[modules."…"]` block
    /// applied; `None` if its `when` doesn't hold or it's hidden while locked). The agent uses it
    /// for app-published items configured by id but shown in a placeholder.
    pub fn instance(&self, id: &ModuleId, ctx: &Context) -> Option<ModuleInstance> {
        self.instantiate(&ModuleRef::Named(id.clone()), None, ctx, &mut Vec::new())
    }

    /// The module `m` with its block parsed, whatever the context: no `when`, no zone width,
    /// no group members. What runs behind a module (a user module's script, a graph's source)
    /// is the same wherever it shows.
    pub fn module(&self, m: &ModuleRef) -> ModuleInstance {
        self.parse_module(m).0
    }

    /// [`Config::module`], and whether its block sets its own width: parsed when the config
    /// loaded. A module nothing in the config names (an app's item) is parsed now.
    pub(crate) fn parse_module(&self, m: &ModuleRef) -> (ModuleInstance, bool) {
        let p = match m {
            ModuleRef::Inline(_, _, p) => p.clone(),
            ModuleRef::Named(id) => match self.parsed.get(id) {
                Some(p) => p.clone(),
                None => Arc::new(Parsed::new(id, &self.module_block(m), "")),
            },
        };
        (p.instance.clone(), p.sized)
    }

    fn instantiate(
        &self,
        m: &ModuleRef,
        default_width: Option<Width>,
        ctx: &Context,
        stack: &mut Vec<ModuleId>,
    ) -> Option<ModuleInstance> {
        let id = m.id();
        if ctx.flag("locked") && HIDDEN_WHILE_LOCKED.contains(&id.kind.as_str()) {
            return None;
        }
        let (mut inst, sized) = self.parse_module(m);
        if inst.common.when.as_ref().is_some_and(|w| !w.eval(ctx)) {
            return None;
        }
        if !sized && let Some(w) = default_width {
            inst.common.width = w;
        }
        if id.kind == "group" {
            if stack.contains(id) || stack.len() >= MAX_DEPTH {
                return None;
            }
            stack.push(id.clone());
            let child_width = inst.module_width();
            for child in inst.members().to_vec() {
                inst.children.extend(self.instantiate(
                    &ModuleRef::Named(child),
                    child_width,
                    ctx,
                    stack,
                ));
            }
            stack.pop();
        }
        Some(inst)
    }
}

/// `name` and the layouts it `extends`, itself first. `parent` says what a layout extends
/// (`None`: there's no such layout).
fn extends_chain(
    name: &str,
    parent: impl Fn(&str) -> Option<Option<String>>,
) -> Result<Vec<String>, ResolveError> {
    let mut chain = vec![name.to_owned()];
    let unknown = |l: &str| ResolveError::UnknownLayout(l.to_owned());
    let mut next = parent(name).ok_or_else(|| unknown(name))?;
    while let Some(p) = next {
        let cycle = chain.contains(&p) || chain.len() > MAX_DEPTH;
        chain.push(p.clone());
        if cycle {
            return Err(ResolveError::ExtendsCycle(chain));
        }
        next = parent(&p).ok_or_else(|| unknown(&p))?;
    }
    Ok(chain)
}

/// The zones a layout shows, from its `extends` chain's levels, farthest parent first (each its
/// zones and `zone-order`): a zone replaces the one of the same name or is added, and a level's
/// `zone-order` reorders and filters what there is so far.
fn inherit_zones(levels: impl IntoIterator<Item = (Vec<Zone>, Option<Vec<String>>)>) -> Vec<Zone> {
    let mut zones: Vec<Zone> = Vec::new();
    for (own, order) in levels {
        for z in own {
            match zones.iter_mut().find(|o| o.name == z.name) {
                Some(slot) => *slot = z,
                None => zones.push(z),
            }
        }
        if let Some(order) = order {
            zones = order
                .iter()
                .filter_map(|n| zones.iter().find(|z| z.name == *n).cloned())
                .collect();
        }
    }
    zones
}

/// Merges `over` into `base`. Tables merge recursively, and everything else (arrays included) is
/// replaced.
pub fn deep_merge(base: &mut Table, over: &Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(Value::Table(b)), Value::Table(o)) => deep_merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Action;
    use crate::keys::Key;
    use crate::module::Gesture;

    fn config(user: &str) -> Config {
        let mut t: Table = crate::model::BUILTIN.parse().unwrap();
        deep_merge(&mut t, &user.parse().unwrap());
        let (c, d) = Config::from_table(&t);
        assert!(d.ok(), "{d:#?}");
        c
    }

    fn ids(r: &Resolved) -> Vec<Vec<String>> {
        r.zones
            .iter()
            .map(|z| z.modules.iter().map(|m| m.id.to_string()).collect())
            .collect()
    }

    /// Every module a layout shows was parsed when the config loaded: named ones (zones, group
    /// members, an inline group's members) are in `parsed`, inline ones carry theirs.
    #[test]
    fn everything_shown_is_parsed_at_load() {
        let c = config(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["clock", "group/g", { type = "group/inline", modules = ["battery"] }, { type = "custom", format = "x" }]
            [modules."group/g"]
            modules = ["key/F1", "group/nested"]
            [modules."group/nested"]
            modules = ["audio/slider"]
            "#,
        );
        fn walk(ms: &[ModuleInstance], c: &Config, inline: &[String]) {
            for m in ms {
                let id = m.id.to_string();
                assert!(c.parsed.contains_key(&m.id) || inline.contains(&id), "{id}");
                walk(&m.children, c, inline);
            }
        }
        for name in c.layouts.keys() {
            let inline: Vec<String> = c.layouts[name]
                .zones
                .iter()
                .flat_map(|z| &z.modules)
                .filter(|m| matches!(m, ModuleRef::Inline(..)))
                .map(|m| m.id().to_string())
                .collect();
            let r = c.resolve_as("base", name, &Context::new()).unwrap();
            for z in &r.zones {
                walk(&z.modules, &c, &inline);
            }
        }
        let g = ModuleId::parse("group/nested").unwrap();
        assert!(c.parsed.contains_key(&g));
        assert!(c.parsed.contains_key(&ModuleId::parse("battery").unwrap()));
    }

    #[test]
    fn defaults_and_physical_esc() {
        let c = config("");
        let here = Context::new().with("physical_esc", true);
        let r = c.resolve("fn", &here).unwrap();
        assert_eq!(r.layout, "fkeys");
        assert_eq!(r.zones[0].modules.len(), 0, "this Mac has a physical Esc");
        assert_eq!(r.zones[1].modules.len(), 12);
        let f1 = &r.zones[1].modules[0];
        assert_eq!(f1.common.width, Width::Fill(1), "module-width applies");
        assert_eq!(
            f1.common.on.get(&Gesture::Tap),
            Some(&Action::Key(Key::function(1).unwrap()))
        );
        assert_eq!(f1.css_id, "key-f1");

        let old_mac = Context::new().with("physical_esc", false);
        let r = c.resolve("fn", &old_mac).unwrap();
        assert_eq!(ids(&r)[0], vec!["key/Esc#auto"]);
        assert_eq!(r.zones[0].modules[0].classes, vec!["auto"]);
    }

    #[test]
    fn module_when_sees_the_chosen_layout() {
        let c = config(
            r#"
            [[layouts.firefox.zones]]
            name = "main"
            modules = ["key/Back", "clock", "battery"]
            [layouts.firefox]
            extends = "default"
            [modules.clock]
            when = { layout = "firefox" }
            [modules.battery]
            when = { layout = "default" }
            [[rules]]
            when = { app-id = "firefox" }
            layout = "firefox"
            "#,
        );
        let ff = Context::new()
            .with("app_id", "firefox")
            .with("physical_esc", true);
        let r = c.resolve("base", &ff).unwrap();
        assert_eq!(r.layout, "firefox");
        assert_eq!(ids(&r)[1], ["key/Back", "clock"]);

        let rule: Table = "[[rules]]\nwhen = { not = { layout = \"x\" } }\nlayout = \"default\""
            .parse()
            .unwrap();
        let (_, d) = Config::from_table(&rule);
        assert!(
            d.warnings
                .iter()
                .any(|w| w.path == "rules[0].when" && w.message.contains("rules choose")),
            "{d:#?}"
        );
    }

    #[test]
    fn rules_first_match_and_inheritance() {
        let c = config(
            r#"
            [[layouts.firefox.zones]]
            name = "main"
            modules = ["key/Back", "key/Forward", "app-controls"]
            [layouts.firefox]
            extends = "default"

            [[rules]]
            name = "locked"
            when = { locked = true }
            layout = "fkeys"
            [[rules]]
            when = { app-id = "firefox" }
            layout = "firefox"
            "#,
        );
        let ff = Context::new()
            .with("app_id", "firefox")
            .with("physical_esc", true);
        let r = c.resolve("base", &ff).unwrap();
        assert_eq!((r.layout.as_str(), r.rule), ("firefox", Some(1)));
        let z: Vec<&str> = r.zones.iter().map(|z| z.name.as_str()).collect();
        assert_eq!(
            z,
            ["esc", "main", "controls"],
            "inherited zones keep their order"
        );
        assert_eq!(ids(&r)[1], ["key/Back", "key/Forward", "app-controls"]);
        assert!(
            ids(&r)[2].contains(&"group/control-strip".to_owned()),
            "controls inherited"
        );
        let group = r.zones[2]
            .modules
            .iter()
            .find(|m| m.id.kind == "group")
            .unwrap();
        assert_eq!(group.children.len(), 6);

        let locked = ff.clone().with("locked", true);
        assert_eq!(
            c.resolve("base", &locked).unwrap().rule,
            Some(0),
            "locked beats the app rule"
        );
        let other = Context::new().with("app_id", "foot");
        assert_eq!(c.resolve("base", &other).unwrap().layout, "default");
    }

    #[test]
    fn built_in_lock_screen_and_screencast() {
        let c = config(
            r#"[[layouts.default.zones]]
name = "main"
modules = ["notifications", "clock"]"#,
        );
        let locked = Context::new()
            .with("locked", true)
            .with("physical_esc", true);
        let r = c.resolve("base", &locked).unwrap();
        assert_eq!(
            r.layout, "locked",
            "the built-in rule picks the lock-screen layout"
        );
        assert_eq!(
            ids(&r),
            [
                vec![],
                vec!["key/PreviousSong", "key/PlayPause", "key/NextSong"],
                vec!["backlight/slider#locked", "audio/slider#locked", "key/Mute"],
            ]
        );
        let sharing = Context::new().with("screencast", true);
        assert_eq!(ids(&c.resolve("base", &sharing).unwrap()), [["clock"]]);
        assert_eq!(
            ids(&c.resolve("base", &Context::new()).unwrap()),
            [["notifications", "clock"]]
        );
    }

    #[test]
    fn privacy_while_locked() {
        // `rules = []` drops the built-in lock-screen rule: hiding must not depend on it.
        let c = config(
            r#"rules = []
[modules."custom/vpn"]
format = "vpn"
[[layouts.default.zones]]
name = "main"
modules = ["custom/vpn", "clock", "app-controls", "window", "taskbar"]"#,
        );
        let r = c
            .resolve("base", &Context::new().with("locked", true))
            .unwrap();
        // Redefining layouts.default.zones replaces the whole array (arrays never merge).
        assert_eq!(
            ids(&r),
            [["clock"]],
            "custom/* and app items are hidden while locked"
        );
    }

    #[test]
    fn zone_order_and_cycles() {
        let c = config(
            r#"
            [layouts.slim]
            extends = "default"
            zone-order = ["controls", "main"]
            [layouts.a]
            extends = "b"
            [layouts.b]
            extends = "a"
            "#,
        );
        let z: Vec<String> = c
            .layout_zones("slim")
            .unwrap()
            .into_iter()
            .map(|z| z.name)
            .collect();
        assert_eq!(z, ["controls", "main"]);
        assert!(matches!(
            c.layout_zones("a"),
            Err(ResolveError::ExtendsCycle(_))
        ));
        assert!(matches!(
            c.layout_zones("nope"),
            Err(ResolveError::UnknownLayout(_))
        ));
    }

    #[test]
    fn inline_modules_and_waybar_blocks() {
        let c = config(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = [{ type = "custom", format = "TTY1", on-tap = { keys = ["LeftCtrl", "LeftAlt", "F1"] } }, "pulseaudio"]
            [modules.pulseaudio]
            format = "{volume}%"
            on-click = "pavucontrol"
            "#,
        );
        let r = c.resolve("base", &Context::new()).unwrap();
        let main = &r.zones[0].modules;
        assert_eq!(main[0].id.to_string(), "custom#default-main-0");
        assert!(
            matches!(main[0].common.on.get(&Gesture::Tap), Some(Action::Keys(k)) if k.len() == 3)
        );
        assert_eq!(main[1].id.kind, "audio");
        assert_eq!(main[1].common.format.as_deref(), Some("{volume}%"));
    }
}
