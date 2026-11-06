//! What the bar shows right now: rule selection, zone inheritance, module instantiation
//! (sliver-config(5)). Runs in the agent. Pixel layout is the renderer's job.

use toml::{Table, Value};

use crate::condition::Context;
use crate::diag::Diagnostics;
use crate::model::{Config, ModuleRef, Zone};
use crate::module::{Common, ModuleId, key_defaults};
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
    /// Type-specific keys, read through [`ModuleInstance::get`].
    config: Table,
    /// Children of a `group/*`, in order.
    pub children: Vec<ModuleInstance>,
}

impl ModuleInstance {
    /// A type-specific key. Every one the runtime reads is declared in [`crate::MODULE_TYPES`],
    /// which is what validation and the man page go by.
    pub fn get(&self, key: &str) -> Option<&Value> {
        debug_assert!(
            crate::module::takes_key(&self.id.kind, key) != Some(false),
            "{} modules don't take {key:?}: declare it in MODULE_TYPES",
            self.id.kind
        );
        self.config.get(key)
    }

    /// Does this module's type take `key` at all (a `clock` is never a slider)?
    pub fn takes(&self, key: &str) -> bool {
        crate::module::takes_key(&self.id.kind, key) == Some(true)
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
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
        Ok(inherit_zones(levels, |z| &z.name))
    }

    /// The merged config block for a module reference: built-in defaults, then
    /// `[modules."<id>"]`, then the inline table.
    pub fn module_block(&self, m: &ModuleRef) -> Table {
        let id = m.id();
        let mut block = id.key().map(key_defaults).unwrap_or_default();
        if let Some(b) = self.modules.get(id) {
            deep_merge(&mut block, b);
        }
        if let ModuleRef::Inline(_, t) = m {
            deep_merge(&mut block, t);
        }
        block
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

    /// [`Config::module`], and whether its block sets its own width.
    fn parse_module(&self, m: &ModuleRef) -> (ModuleInstance, bool) {
        let id = m.id();
        let block = self.module_block(m);
        let sized = block.contains_key("width") || block.contains_key("expand");
        // Diagnostics are reported by `validate`; resolving is best-effort.
        let (common, mut config) = Common::parse(&block, "", &mut Diagnostics::default());
        if id.slider {
            config.entry("slider").or_insert(Value::Boolean(true));
        }
        let mut classes: Vec<String> = id.instance.iter().cloned().collect();
        classes.extend(common.class.iter().cloned());
        let inst = ModuleInstance {
            css_id: id.css_id(),
            id: id.clone(),
            classes,
            common,
            config,
            children: Vec::new(),
        };
        (inst, sized)
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
            // `module-width` on a group is the default width of its children, like on a zone.
            let child_width = inst
                .config
                .get("module-width")
                .and_then(|v| Width::parse(v).ok());
            if stack.contains(id) || stack.len() >= MAX_DEPTH {
                return None;
            }
            stack.push(id.clone());
            for child in group_members(&inst.config) {
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
pub fn extends_chain(
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
/// `zone-order` reorders and filters what there is so far. Generic over the zone, so editors
/// working on the raw TOML inherit exactly like the parsed config.
pub fn inherit_zones<Z: Clone>(
    levels: impl IntoIterator<Item = (Vec<Z>, Option<Vec<String>>)>,
    name: impl Fn(&Z) -> &str,
) -> Vec<Z> {
    let mut zones: Vec<Z> = Vec::new();
    for (own, order) in levels {
        for z in own {
            match zones.iter_mut().find(|o| name(o) == name(&z)) {
                Some(slot) => *slot = z,
                None => zones.push(z),
            }
        }
        if let Some(order) = order {
            zones = order
                .iter()
                .filter_map(|n| zones.iter().find(|z| name(z) == n.as_str()).cloned())
                .collect();
        }
    }
    zones
}

/// The `modules = [...]` of a group block, parsed (invalid names skipped; `validate` reports them).
pub(crate) fn group_members(config: &Table) -> Vec<ModuleId> {
    config
        .get("modules")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(|s| ModuleId::parse(s).ok())
                .collect()
        })
        .unwrap_or_default()
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
