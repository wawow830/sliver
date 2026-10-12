//! Whole-config checks (customization.md §9). Errors block applying the config. Everything a user
//! may legitimately do, including removing every F-key (Q2/H4 revised), is at most a warning.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use toml::Value;

use crate::condition::Context;
use crate::diag::{Diagnostics, join};
use crate::keys::Key;
use crate::model::{Config, ModuleRef};
use crate::module::{Common, Gesture, ModuleId, check_type_keys};
use crate::resolve::group_members;
use crate::values::{Width, parse_duration};

/// Touch-target advice from birch (features.md §9): ~9 mm, with 60 px as the "really too small" mark.
pub const COMFORTABLE_TARGET_PX: u32 = 80;
pub const TINY_TARGET_PX: u32 = 60;

/// Facts about the machine that change what's safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hardware {
    /// The keyboard has an Esc key next to the bar (2008 px bars). 2170 px bars don't.
    pub physical_esc: bool,
}

impl Hardware {
    pub fn from_bar_width(px: u32) -> Self {
        Self {
            physical_esc: px < 2170,
        }
    }

    /// Reads the bar length from the Touch Bar's DSI connector (`/sys/class/drm/*DSI*/modes`, e.g.
    /// "60x2008"). Unknown hardware assumes no physical Esc, which fails safe: more warnings, not fewer.
    pub fn detect() -> Self {
        let width = std::fs::read_dir("/sys/class/drm")
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("DSI"))
            .filter_map(|e| std::fs::read_to_string(e.path().join("modes")).ok())
            .filter_map(|m| {
                let (a, b) = m.lines().next()?.split_once('x')?;
                Some(
                    a.trim()
                        .parse::<u32>()
                        .ok()?
                        .max(b.trim().parse::<u32>().ok()?),
                )
            })
            .next();
        width.map_or(
            Hardware {
                physical_esc: false,
            },
            Hardware::from_bar_width,
        )
    }

    /// The part of the context that only depends on hardware, for static checks.
    pub fn context(self) -> Context {
        Context::new().with("physical_esc", self.physical_esc)
    }
}

impl Config {
    pub fn validate(&self, hw: Hardware) -> Diagnostics {
        let mut d = Diagnostics::default();
        self.check_structure(&mut d);
        self.check_modules(&mut d);
        if d.ok() {
            self.check_reachability(hw, &mut d);
        }
        d
    }

    fn check_structure(&self, d: &mut Diagnostics) {
        for reserved in ["base", "fn"] {
            if !self.layers.contains_key(reserved) {
                d.error("layers", format!("the {reserved:?} layer needs a layout"));
            }
        }
        for (layer, layout) in &self.layers {
            if !self.layouts.contains_key(layout) {
                d.error(join("layers", layer), format!("no layout named {layout:?}"));
            }
        }
        for name in self.layer_options.keys() {
            if !self.layers.contains_key(name) {
                d.warn(join("layer-options", name), "no such layer");
            }
        }
        for name in self.layouts.keys() {
            if let Err(e) = self.layout_zones(name) {
                d.error(join("layouts", name), e.to_string());
            }
        }
        for (i, r) in self.rules.iter().enumerate() {
            let p = format!("rules[{i}]");
            if !self.layers.contains_key(&r.layer) {
                d.error(
                    join(&p, "layer"),
                    format!("no layer {:?} in [layers]", r.layer),
                );
            }
            if !self.layouts.contains_key(&r.layout) {
                d.error(
                    join(&p, "layout"),
                    format!("no layout named {:?}", r.layout),
                );
            }
            if r.when.is_always() && i + 1 < self.rules.len() {
                d.warn(
                    &p,
                    "this rule always matches, so the rules below it for this layer never apply",
                );
            }
        }
    }

    /// Every module block and inline module, checked once with a path the user can find.
    fn check_modules(&self, d: &mut Diagnostics) {
        let mut placed: BTreeSet<ModuleId> = BTreeSet::new();
        for (lname, layout) in &self.layouts {
            for (zi, z) in layout.zones.iter().enumerate() {
                for (mi, m) in z.modules.iter().enumerate() {
                    placed.insert(m.id().clone());
                    if let ModuleRef::Inline(..) = m {
                        let p = format!("layouts.{lname}.zones[{zi}].modules[{mi}]");
                        self.check_module(m, &p, z.module_width, d);
                    }
                }
            }
        }
        for (id, block) in &self.modules {
            let p = join("modules", &id.to_string());
            self.check_module(&ModuleRef::Named(id.clone()), &p, None, d);
            if id.kind == "group" {
                let members = group_members(block);
                placed.extend(members.iter().cloned());
                if let Some(Value::Array(list)) = block.get("modules") {
                    for (i, v) in list.iter().enumerate() {
                        if let Err(e) = v
                            .as_str()
                            .ok_or("expected a module name".to_owned())
                            .and_then(ModuleId::parse)
                        {
                            d.error(format!("{p}.modules[{i}]"), e);
                        }
                    }
                }
                self.check_group_cycle(id, &p, d);
            }
        }
        for id in self.modules.keys() {
            if !placed.contains(id) && !self.is_builtin_block(id) {
                d.warn(
                    join("modules", &id.to_string()),
                    "configured but not placed in any layout or group",
                );
            }
        }
    }

    fn is_builtin_block(&self, id: &ModuleId) -> bool {
        // Blocks from builtin.toml stay valid even if the user's layouts no longer use them. Read
        // from builtin.toml itself, so the list can't drift.
        static NAMES: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();
        NAMES
            .get_or_init(|| {
                crate::model::BUILTIN
                    .parse::<toml::Table>()
                    .ok()
                    .and_then(|t| t.get("modules")?.as_table().cloned())
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default()
            })
            .contains(&id.to_string())
    }

    fn check_module(
        &self,
        m: &ModuleRef,
        path: &str,
        zone_width: Option<Width>,
        d: &mut Diagnostics,
    ) {
        let id = m.id();
        let block = self.module_block(m);
        let (common, rest) = Common::parse(&block, path, d);
        check_type_keys(id, &rest, path, d);
        let formats = [
            ("format", common.format.as_ref()),
            ("format-alt", common.format_alt.as_ref()),
            ("detail-format", common.detail_format.as_ref()),
        ];
        let variants = common
            .format_variants
            .iter()
            .map(|(k, v)| (k.as_str(), Some(v)));
        for (key, f) in formats.into_iter().chain(variants) {
            if let Some(Err(e)) = f.map(|f| crate::format::Template::parse(f)) {
                let key = if common.format_variants.contains_key(key) {
                    format!("format-{key}")
                } else {
                    key.to_owned()
                };
                d.error(join(path, &key), e);
            }
        }
        let width = if block.contains_key("width") {
            common.width
        } else {
            zone_width.unwrap_or(common.width)
        };
        if let Width::Fixed(px) = width {
            if common.has_actions() && px < TINY_TARGET_PX {
                d.warn(path, format!("{px} px is very hard to hit; {COMFORTABLE_TARGET_PX} px (~9 mm) is comfortable"));
            } else if common.has_actions() && px < COMFORTABLE_TARGET_PX {
                d.warn(path, format!("{px} px is below the comfortable {COMFORTABLE_TARGET_PX} px (~9 mm) touch target"));
            }
        }
        if common.on.contains_key(&Gesture::DoubleTap) && common.on.contains_key(&Gesture::Tap) {
            d.warn(
                path,
                "on-double-tap delays this module's single tap while it waits for a second one",
            );
        }
        match id.kind.as_str() {
            "custom" => {
                if common.format.is_none() && !rest.contains_key("exec") && common.icon.is_none() {
                    d.warn(
                        path,
                        "a custom module with no exec, format or icon shows nothing",
                    );
                }
                if let Some(v) = rest.get("restart-interval") {
                    match parse_duration(v) {
                        Ok(t) if t < Duration::from_secs(1) => d.warn(
                            join(path, "restart-interval"),
                            "below the 1s floor; using 1s",
                        ),
                        Ok(_) => {}
                        Err(e) => d.error(join(path, "restart-interval"), e),
                    }
                }
                if let Some(v) = rest.get("timeout") {
                    if let Err(e) = parse_duration(v) {
                        d.error(join(path, "timeout"), e);
                    }
                }
            }
            "group" => {
                let vertical = rest.get("orientation").and_then(Value::as_str) == Some("vertical");
                if vertical {
                    for child in group_members(&rest) {
                        let (cc, _) = Common::parse(
                            &self.module_block(&ModuleRef::Named(child.clone())),
                            "",
                            &mut Diagnostics::default(),
                        );
                        if cc.has_actions() {
                            d.warn(path, format!("{child} has actions, but vertical groups stack 30 px rows, too small to touch reliably"));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn check_group_cycle(&self, start: &ModuleId, path: &str, d: &mut Diagnostics) {
        fn walk(c: &Config, id: &ModuleId, stack: &mut Vec<ModuleId>) -> bool {
            if stack.contains(id) {
                return true;
            }
            let Some(block) = c.modules.get(id) else {
                return false;
            };
            stack.push(id.clone());
            let cyc = group_members(block)
                .iter()
                .filter(|m| m.kind == "group")
                .any(|m| walk(c, m, stack));
            stack.pop();
            cyc
        }
        if walk(self, start, &mut Vec::new()) {
            d.error(path, "this group contains itself");
        }
    }

    /// Keys a layout puts directly on the bar: visible top-level modules, plus children of groups
    /// that aren't drawers.
    pub fn visible_keys(&self, layout: &str, hw: Hardware) -> BTreeSet<Key> {
        let ctx = hw.context();
        let mut keys = BTreeSet::new();
        let Ok(zones) = self.layout_zones(layout) else {
            return keys;
        };
        let mut todo: Vec<ModuleRef> = zones.into_iter().flat_map(|z| z.modules).collect();
        let mut seen = 0;
        while let Some(m) = todo.pop() {
            seen += 1;
            if seen > 512 {
                break;
            }
            let block = self.module_block(&m);
            let (common, rest) = Common::parse(&block, "", &mut Diagnostics::default());
            if common.when.as_ref().is_some_and(|w| !w.eval(&ctx)) {
                continue;
            }
            if let Some(a) = common.on.get(&Gesture::Tap) {
                keys.extend(a.keys());
            }
            if m.id().kind == "group" && !rest.contains_key("drawer") {
                todo.extend(group_members(&rest).into_iter().map(ModuleRef::Named));
            }
        }
        keys
    }

    /// GOAL invariant 1 is guaranteed by sliverd's safe hatch, not by config. Here we only warn
    /// when the config itself leaves F-keys or Esc off the bar (customization.md §9.1).
    fn check_reachability(&self, hw: Hardware, d: &mut Diagnostics) {
        let candidates = |layer: &str| -> BTreeSet<&str> {
            let mut s: BTreeSet<&str> = self
                .layers
                .get(layer)
                .map(String::as_str)
                .into_iter()
                .collect();
            // Lock-screen layouts don't count: F-keys aren't for typing a password, and the safe
            // hatch still works there.
            s.extend(
                self.rules
                    .iter()
                    .filter(|r| r.layer == layer && !r.when.requires_flag("locked"))
                    .map(|r| r.layout.as_str()),
            );
            s
        };
        let (bases, fns) = (candidates("base"), candidates("fn"));
        let keys: BTreeMap<&str, BTreeSet<Key>> = bases
            .union(&fns)
            .map(|l| (*l, self.visible_keys(l, hw)))
            .collect();
        let fkeys: Vec<Key> = (1..=12).filter_map(Key::function).collect();
        for &base in &bases {
            for &fnl in &fns {
                let on_bar: BTreeSet<Key> = keys[base].union(&keys[fnl]).copied().collect();
                let missing: Vec<&str> = fkeys
                    .iter()
                    .filter(|k| !on_bar.contains(k))
                    .map(|k| k.name())
                    .collect();
                if !missing.is_empty() {
                    d.warn(
                        "layers",
                        format!(
                            "with base = {base:?} and fn = {fnl:?}, {} aren't on the bar; they're only reachable through the safe hatch (double-press Fn and hold)",
                            missing.join(", ")
                        ),
                    );
                }
                if !hw.physical_esc && !on_bar.contains(&Key::esc()) {
                    d.warn(
                        "layers",
                        format!("this Mac has no physical Esc and neither {base:?} nor {fnl:?} shows one; Esc is only reachable through the safe hatch"),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::deep_merge;

    fn check(user: &str, hw: Hardware) -> Diagnostics {
        let mut t: toml::Table = crate::model::BUILTIN.parse().unwrap();
        deep_merge(&mut t, &user.parse().unwrap());
        let (c, mut d) = Config::from_table(&t);
        d.extend(c.validate(hw));
        d
    }

    const HERE: Hardware = Hardware { physical_esc: true };
    const OLD_MAC: Hardware = Hardware {
        physical_esc: false,
    };

    #[test]
    fn builtin_is_clean_on_both_kinds_of_mac() {
        for hw in [HERE, OLD_MAC] {
            let d = check("", hw);
            assert!(d.ok() && d.warnings.is_empty(), "{hw:?}: {d:#?}");
        }
    }

    #[test]
    fn removing_fkeys_is_a_warning_not_an_error() {
        // The default base layout shows F-keys too (group/fkeys), so use the media layer as base.
        let d = check(
            r#"
            [layers]
            base = "media"
            [[layouts.fkeys.zones]]
            name = "main"
            modules = ["clock"]
            "#,
            OLD_MAC,
        );
        assert!(d.ok(), "{d:#?}");
        let msgs: Vec<&str> = d.warnings.iter().map(|w| w.message.as_str()).collect();
        assert!(
            msgs.iter()
                .any(|m| m.contains("F1, F2") && m.contains("safe hatch")),
            "{msgs:?}"
        );
        assert!(
            !msgs.iter().any(|m| m.contains("no physical Esc")),
            "the esc zone still has Esc: {msgs:?}"
        );
    }

    #[test]
    fn tiny_dfr_default_split_is_fine() {
        // F-keys on base and media on Fn: covered by base ∪ fn.
        let d = check("[layers]\nbase = \"fkeys\"\nfn = \"media\"", HERE);
        assert!(d.ok() && d.warnings.is_empty(), "{d:#?}");
    }

    #[test]
    fn structural_errors() {
        let d = check(
            r#"
            [layers]
            media = "nope"
            [[rules]]
            layer = "ghost"
            layout = "missing"
            [modules."group/a"]
            modules = ["group/b"]
            [modules."group/b"]
            modules = ["group/a"]
            [[layouts.default.zones]]
            name = "main"
            modules = ["group/a"]
            "#,
            HERE,
        );
        let paths: Vec<&str> = d.errors.iter().map(|e| e.path.as_str()).collect();
        for p in [
            "layers.media",
            "rules[0].layer",
            "rules[0].layout",
            "modules.\"group/a\"",
        ] {
            assert!(paths.contains(&p), "{p} in {paths:?}");
        }
    }

    #[test]
    fn bad_format_strings_are_errors() {
        let d = check(
            "[modules.battery]\nformat-charging = \"{capacity\"\n[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"battery\"]",
            HERE,
        );
        assert_eq!(d.errors.len(), 1, "{d:#?}");
        assert_eq!(d.errors[0].path, "modules.battery.format-charging");
    }

    #[test]
    fn module_advice() {
        let d = check(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["custom/a", "custom/empty", "custom/b"]
            [modules."custom/a"]
            format = "a"
            width = 50
            on-tap = "true"
            [modules."custom/empty"]
            [modules."custom/b"]
            format = "b"
            bogus = 1
            restart-interval = "10ms"
            [modules."custom/unused"]
            format = "x"
            "#,
            HERE,
        );
        assert!(d.ok(), "{d:#?}");
        let w: Vec<String> = d.warnings.iter().map(ToString::to_string).collect();
        let has = |frag: &str| w.iter().any(|x| x.contains(frag));
        assert!(
            has("modules.\"custom/a\": 50 px is very hard to hit"),
            "{w:#?}"
        );
        assert!(
            has("modules.\"custom/empty\": a custom module with no exec"),
            "{w:#?}"
        );
        assert!(has("modules.\"custom/b\".bogus"), "{w:#?}");
        assert!(has("restart-interval: below the 1s floor"), "{w:#?}");
        assert!(
            has("modules.\"custom/unused\": configured but not placed"),
            "{w:#?}"
        );
    }
}
