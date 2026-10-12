//! The parsed config (customization.md §2, §6).

use std::collections::BTreeMap;
use std::time::Duration;

use toml::{Table, Value};

use crate::action::Action;
use crate::condition::Condition;
use crate::diag::{Diagnostics, Reader, join};
use crate::module::ModuleId;
use crate::values::{Align, FireOn, Width, parse_duration};

/// Current config schema version.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FnMode {
    /// The Fn layer shows while Fn is held (tiny-dfr behavior).
    #[default]
    Hold,
    /// Each Fn press flips between base and fn.
    Toggle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub name: Option<String>,
    pub spacing: u32,
    pub margin: u32,
    pub fn_mode: FnMode,
    pub fire_on: FireOn,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            name: None,
            spacing: 8,
            margin: 0,
            fn_mode: FnMode::Hold,
            fire_on: FireOn::Release,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// Follow the main display's brightness (tiny-dfr `AdaptiveBrightness`).
    pub adaptive: bool,
    /// 0–255 (tiny-dfr `ActiveBrightness`).
    pub max_brightness: u8,
    pub dim_after: Option<Duration>,
    pub off_after: Option<Duration>,
    pub pixel_shift: bool,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            adaptive: true,
            max_brightness: 128,
            dim_after: Some(Duration::from_secs(30)),
            off_after: Some(Duration::from_secs(60)),
            pixel_shift: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LayerOptions {
    pub timeout: Option<Duration>,
    pub close_button: Option<bool>,
}

/// One entry of a zone's `modules` list.
#[derive(Debug, Clone, PartialEq)]
pub enum ModuleRef {
    /// `"battery#bat2"`: configured by `[modules."battery#bat2"]`.
    Named(ModuleId),
    /// `{ type = "custom", format = "TTY1", … }`. The id gets a generated instance name.
    Inline(ModuleId, Table),
}

impl ModuleRef {
    pub fn id(&self) -> &ModuleId {
        match self {
            ModuleRef::Named(id) | ModuleRef::Inline(id, _) => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    pub name: String,
    pub modules: Vec<ModuleRef>,
    pub width: Width,
    pub align: Align,
    /// Default width for modules in this zone that don't set one (tiny-dfr import uses `fill`).
    pub module_width: Option<Width>,
    pub class: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub extends: Option<String>,
    pub zones: Vec<Zone>,
    pub zone_order: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub name: Option<String>,
    pub when: Condition,
    pub layer: String,
    pub layout: String,
}

/// Bar-wide gestures (customization.md §5).
pub const BAR_GESTURES: &[&str] = &[
    "two-finger-swipe-left",
    "two-finger-swipe-right",
    "edge-swipe-from-left",
    "edge-swipe-from-right",
];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub version: u32,
    pub stylesheet: Option<String>,
    /// The built-in base theme (`sliver`, `classic`) under the user's stylesheet. `None` = `sliver`.
    pub theme: Option<String>,
    pub bar: Bar,
    pub display: Display,
    /// Layer name → its default layout. `base` and `fn` are reserved.
    pub layers: BTreeMap<String, String>,
    pub layer_options: BTreeMap<String, LayerOptions>,
    pub layouts: BTreeMap<String, Layout>,
    pub rules: Vec<Rule>,
    /// Module blocks, keyed by the canonical id (so `[modules.pulseaudio]` configures `audio`).
    pub modules: BTreeMap<ModuleId, Table>,
    pub gestures: BTreeMap<String, Action>,
    /// Per-app API preferences, keyed by app id (`"*"` = defaults). api.md §4.
    pub apps: BTreeMap<String, AppPrefs>,
    /// Per-provider preferences, keyed by D-Bus provider id (a Flatpak/Snap id when sandboxed).
    pub providers: BTreeMap<String, ProviderPrefs>,
    pub api: ApiPrefs,
}

/// Always merged *underneath* every config, so built-in layers and layouts always exist and any
/// of them can be redefined (customization.md §6).
pub const BUILTIN: &str = include_str!("builtin.toml");

impl Config {
    /// Parses a merged config table (includes already resolved; see [`crate::load`]).
    pub fn from_table(t: &Table) -> (Config, Diagnostics) {
        let mut d = Diagnostics::default();
        let mut r = Reader::new(t, "");
        let mut c = Config {
            version: SCHEMA_VERSION,
            ..Config::default()
        };
        r.skip("include");
        match r.uint("version", u32::MAX as u64, &mut d) {
            Some(v) if v as u32 == SCHEMA_VERSION => {}
            Some(v) => d.error("version", format!("config version {v} isn't supported (this sliver reads version {SCHEMA_VERSION})")),
            None => {}
        }
        c.stylesheet = r.string("stylesheet", &mut d);
        c.theme = r.string("theme", &mut d);
        if let Some(b) = r.table("bar", &mut d) {
            c.bar = parse_bar(b, &mut d);
        }
        if let Some(t) = r.table("display", &mut d) {
            c.display = parse_display(t, &mut d);
        }
        if let Some(layers) = r.table("layers", &mut d) {
            for (k, v) in layers {
                match v.as_str() {
                    Some(l) => {
                        c.layers.insert(k.clone(), l.to_owned());
                    }
                    None => d.error(join("layers", k), "a layer maps to a layout name"),
                }
            }
        }
        if let Some(opts) = r.table("layer-options", &mut d) {
            for (k, v) in opts {
                let p = join("layer-options", k);
                if let Some(t) = v.as_table() {
                    c.layer_options
                        .insert(k.clone(), parse_layer_options(t, &p, &mut d));
                } else {
                    d.error(p, "expected a table");
                }
            }
        }
        if let Some(layouts) = r.table("layouts", &mut d) {
            for (name, v) in layouts {
                let p = join("layouts", name);
                match v.as_table() {
                    Some(t) => {
                        c.layouts
                            .insert(name.clone(), parse_layout(name, t, &p, &mut d));
                    }
                    None => d.error(p, "expected a table"),
                }
            }
        }
        match r.get("rules") {
            None => {}
            Some(Value::Array(rules)) => {
                for (i, v) in rules.iter().enumerate() {
                    let p = format!("rules[{i}]");
                    match v.as_table() {
                        Some(t) => c.rules.extend(parse_rule(t, &p, &mut d)),
                        None => d.error(p, "expected a table ([[rules]])"),
                    }
                }
            }
            Some(_) => d.error("rules", "rules is an array of tables: [[rules]]"),
        }
        if let Some(modules) = r.table("modules", &mut d) {
            for (k, v) in modules {
                let p = join("modules", k);
                let (Ok(id), Some(block)) = (ModuleId::parse(k), v.as_table()) else {
                    d.error(p, "expected [modules.\"<type>[#instance]\"] with a table");
                    continue;
                };
                if !id.is_known_type() {
                    d.warn(&p, format!("unknown module type {:?}", id.kind));
                }
                if c.modules.insert(id.clone(), block.clone()).is_some() {
                    d.error(
                        p,
                        format!("{id} is configured twice (check waybar aliases)"),
                    );
                }
            }
        }
        if let Some(t) = r.table("apps", &mut d) {
            for (id, v) in t {
                let p = join("apps", id);
                match v.as_table() {
                    Some(t) => {
                        c.apps.insert(id.clone(), parse_app_prefs(t, &p, &mut d));
                    }
                    None => d.error(p, "expected a table"),
                }
            }
        }
        if let Some(t) = r.table("providers", &mut d) {
            for (id, v) in t {
                let p = join("providers", id);
                match v.as_table() {
                    Some(t) => {
                        c.providers
                            .insert(id.clone(), parse_provider_prefs(t, &p, &mut d));
                    }
                    None => d.error(p, "expected a table"),
                }
            }
        }
        if let Some(t) = r.table("api", &mut d) {
            let mut ar = Reader::new(t, "api");
            if let Some(b) = ar.bool("target", &mut d) {
                c.api.target = b;
            }
            ar.finish(&mut d);
        }
        if let Some(g) = r.table("gestures", &mut d) {
            for (k, v) in g {
                let p = join("gestures", k);
                if !BAR_GESTURES.contains(&k.as_str()) {
                    d.warn(
                        &p,
                        format!("unknown bar gesture; known: {}", BAR_GESTURES.join(", ")),
                    );
                }
                if let Some(a) = Action::parse(v, &p, &mut d) {
                    c.gestures.insert(k.clone(), a);
                }
            }
        }
        r.finish(&mut d);
        (c, d)
    }
}

/// What one app may do with the bar (`[apps."<app-id>"]`). `None` = not set here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppPrefs {
    /// May the app (or a provider for it) fill App controls? Default true.
    pub controls: Option<bool>,
    /// May its modal contexts take the bar beyond the first zone? Default true.
    pub whole_bar: Option<bool>,
    /// Only this provider fills App controls for the app.
    pub pin: Option<String>,
}

/// One provider's preferences (`[providers."<id>"]`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderPrefs {
    /// `false` hides it everywhere (Status items and App controls).
    pub show: bool,
    /// Lets a *sandboxed* provider publish status items (ignored for unsandboxed ones).
    pub allow: bool,
    /// Status items order: lower first, unlisted after (by id).
    pub order: Option<i64>,
}

impl Default for ProviderPrefs {
    fn default() -> Self {
        Self {
            show: true,
            allow: false,
            order: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApiPrefs {
    /// `target`-scope contexts (scripts targeting other apps) allowed at all. Default true.
    pub target: bool,
}

impl Default for ApiPrefs {
    fn default() -> Self {
        Self { target: true }
    }
}

/// Effective per-app preferences after the `"*"` defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct AppPolicy {
    pub controls: bool,
    pub whole_bar: bool,
    pub pin: Option<String>,
}

impl Config {
    /// The preferences for `app_id`: its own `[apps."…"]` (case-insensitive, `.desktop` suffix
    /// ignored, like conditions), then `[apps."*"]`, then the defaults.
    pub fn app_policy(&self, app_id: &str) -> AppPolicy {
        let norm = |s: &str| s.trim_end_matches(".desktop").to_ascii_lowercase();
        let want = norm(app_id);
        let own = self
            .apps
            .iter()
            .find(|(k, _)| *k != "*" && norm(k) == want)
            .map(|(_, v)| v);
        let any = self.apps.get("*");
        let pick = |f: fn(&AppPrefs) -> Option<bool>, default: bool| {
            own.and_then(f)
                .or_else(|| any.and_then(f))
                .unwrap_or(default)
        };
        AppPolicy {
            controls: pick(|p| p.controls, true),
            whole_bar: pick(|p| p.whole_bar, true),
            pin: own
                .and_then(|p| p.pin.clone())
                .or_else(|| any.and_then(|p| p.pin.clone())),
        }
    }

    pub fn provider_prefs(&self, provider: &str) -> ProviderPrefs {
        self.providers.get(provider).cloned().unwrap_or_default()
    }
}

fn parse_app_prefs(t: &Table, path: &str, d: &mut Diagnostics) -> AppPrefs {
    let mut r = Reader::new(t, path);
    let p = AppPrefs {
        controls: r.bool("controls", d),
        whole_bar: r.bool("whole-bar", d),
        pin: r.string("pin", d),
    };
    r.finish(d);
    p
}

fn parse_provider_prefs(t: &Table, path: &str, d: &mut Diagnostics) -> ProviderPrefs {
    let mut r = Reader::new(t, path);
    let mut p = ProviderPrefs::default();
    if let Some(b) = r.bool("show", d) {
        p.show = b;
    }
    if let Some(b) = r.bool("allow", d) {
        p.allow = b;
    }
    if let Some(v) = r.get("order") {
        match v.as_integer() {
            Some(n) => p.order = Some(n),
            None => d.error(join(path, "order"), "order is a whole number"),
        }
    }
    r.finish(d);
    p
}

fn parse_bar(t: &Table, d: &mut Diagnostics) -> Bar {
    let mut r = Reader::new(t, "bar");
    let mut b = Bar {
        name: r.string("name", d),
        ..Bar::default()
    };
    if let Some(n) = r.uint("spacing", 500, d) {
        b.spacing = n as u32;
    }
    if let Some(n) = r.uint("margin", 500, d) {
        b.margin = n as u32;
    }
    match r.string("fn-mode", d).as_deref() {
        None | Some("hold") => {}
        Some("toggle") => b.fn_mode = FnMode::Toggle,
        Some(m) => d.error("bar.fn-mode", format!("bad fn-mode {m:?} (hold or toggle)")),
    }
    if let Some(s) = r.string("fire-on", d) {
        match FireOn::parse(&s) {
            Ok(f) => b.fire_on = f,
            Err(e) => d.error("bar.fire-on", e),
        }
    }
    r.finish(d);
    b
}

fn optional_duration(v: &Value, path: &str, d: &mut Diagnostics) -> Option<Duration> {
    match v {
        Value::Boolean(false) => None,
        Value::String(s) if s == "never" => None,
        v => parse_duration(v).map_err(|e| d.error(path, e)).ok(),
    }
}

fn parse_display(t: &Table, d: &mut Diagnostics) -> Display {
    let mut r = Reader::new(t, "display");
    let mut x = Display::default();
    match r.string("brightness", d).as_deref() {
        None | Some("adaptive") => {}
        Some("fixed") => x.adaptive = false,
        Some(m) => d.error(
            "display.brightness",
            format!("bad brightness mode {m:?} (adaptive or fixed)"),
        ),
    }
    if let Some(n) = r.uint("max-brightness", 255, d) {
        x.max_brightness = n as u8;
    }
    if let Some(v) = r.get("dim-after") {
        x.dim_after = optional_duration(v, "display.dim-after", d);
    }
    if let Some(v) = r.get("off-after") {
        x.off_after = optional_duration(v, "display.off-after", d);
    }
    if let Some(b) = r.bool("pixel-shift", d) {
        x.pixel_shift = b;
    }
    r.finish(d);
    x
}

fn parse_layer_options(t: &Table, path: &str, d: &mut Diagnostics) -> LayerOptions {
    let mut r = Reader::new(t, path);
    let timeout = r
        .get("timeout")
        .and_then(|v| optional_duration(v, &join(path, "timeout"), d));
    let o = LayerOptions {
        timeout,
        close_button: r.bool("close-button", d),
    };
    r.finish(d);
    o
}

fn parse_layout(name: &str, t: &Table, path: &str, d: &mut Diagnostics) -> Layout {
    let mut r = Reader::new(t, path);
    let mut l = Layout {
        extends: r.string("extends", d),
        ..Layout::default()
    };
    if t.contains_key("zone-order") {
        l.zone_order = Some(r.strings("zone-order", d));
    }
    match r.get("zones") {
        None => {}
        Some(Value::Array(zones)) => {
            for (i, z) in zones.iter().enumerate() {
                let p = format!("{path}.zones[{i}]");
                match z.as_table() {
                    Some(zt) => {
                        if let Some(z) = parse_zone(name, zt, &p, d) {
                            if l.zones.iter().any(|o| o.name == z.name) {
                                d.error(&p, format!("zone {:?} appears twice", z.name));
                            }
                            l.zones.push(z);
                        }
                    }
                    None => d.error(p, "expected a table ([[layouts.<name>.zones]])"),
                }
            }
        }
        Some(_) => d.error(r.path("zones"), "zones is an array of tables"),
    }
    r.finish(d);
    l
}

fn parse_zone(layout: &str, t: &Table, path: &str, d: &mut Diagnostics) -> Option<Zone> {
    let mut r = Reader::new(t, path);
    let name = r.string("name", d);
    if name.is_none() {
        d.error(path, "a zone needs a name");
    }
    let mut z = Zone {
        name: name.clone().unwrap_or_default(),
        modules: Vec::new(),
        width: Width::Auto,
        align: Align::Start,
        module_width: None,
        class: r.strings("class", d),
    };
    if let Some(v) = r.get("width") {
        z.width = Width::parse(v)
            .map_err(|e| d.error(join(path, "width"), e))
            .unwrap_or_default();
    }
    if let Some(v) = r.get("module-width") {
        z.module_width = Width::parse(v)
            .map_err(|e| d.error(join(path, "module-width"), e))
            .ok();
    }
    if let Some(s) = r.string("align", d) {
        z.align = Align::parse(&s)
            .map_err(|e| d.error(join(path, "align"), e))
            .unwrap_or_default();
    }
    match r.get("modules") {
        None => {}
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                let p = format!("{path}.modules[{i}]");
                let parsed = match item {
                    Value::String(s) => ModuleId::parse(s).map(ModuleRef::Named),
                    Value::Table(t) => parse_inline(layout, &z.name, i, t),
                    other => Err(format!(
                        "expected a module name or inline table, got {}",
                        other.type_str()
                    )),
                };
                match parsed {
                    Ok(m) => {
                        if !m.id().is_known_type() {
                            d.warn(&p, format!("unknown module type {:?}", m.id().kind));
                        }
                        if m.id().kind == "key" && m.id().key().is_none() {
                            d.error(&p, format!("unknown key name in {}", m.id()));
                        }
                        z.modules.push(m);
                    }
                    Err(e) => d.error(p, e),
                }
            }
        }
        Some(_) => d.error(join(path, "modules"), "modules is an array"),
    }
    r.finish(d);
    name.map(|_| z)
}

fn parse_inline(layout: &str, zone: &str, i: usize, t: &Table) -> Result<ModuleRef, String> {
    let ty = t
        .get("type")
        .and_then(Value::as_str)
        .ok_or("an inline module needs type = \"…\"")?;
    let mut id = ModuleId::parse(ty)?;
    if id.instance.is_none() {
        id.instance = Some(format!("{layout}-{zone}-{i}"));
    }
    let mut block = t.clone();
    block.remove("type");
    Ok(ModuleRef::Inline(id, block))
}

fn parse_rule(t: &Table, path: &str, d: &mut Diagnostics) -> Option<Rule> {
    let mut r = Reader::new(t, path);
    let name = r.string("name", d);
    let when = match r.get("when") {
        Some(v) => Condition::parse(v, &join(path, "when"), d),
        None => Some(Condition::always()),
    };
    let layer = r.string("layer", d).unwrap_or_else(|| "base".into());
    let layout = r.string("layout", d);
    if layout.is_none() {
        d.error(path, "a rule needs layout = \"…\"");
    }
    r.finish(d);
    Some(Rule {
        name,
        when: when?,
        layer,
        layout: layout?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses_clean() {
        let t: Table = BUILTIN.parse().expect("builtin.toml is valid TOML");
        let (c, d) = Config::from_table(&t);
        assert!(d.ok() && d.warnings.is_empty(), "{d:#?}");
        assert_eq!(c.layers.get("base").map(String::as_str), Some("default"));
        assert_eq!(c.layers.get("fn").map(String::as_str), Some("fkeys"));
        for l in ["default", "fkeys", "media"] {
            assert!(c.layouts.contains_key(l), "{l}");
        }
    }

    #[test]
    fn app_and_provider_prefs() {
        let t: Table = r#"
            [apps."*"]
            whole-bar = false
            [apps."org.mozilla.firefox"]
            controls = false
            pin = "org.example.Tabs"
            [providers."org.example.Pomodoro"]
            show = false
            order = 2
            [providers."com.spotify.Client"]
            allow = true
            [api]
            target = false
        "#
        .parse()
        .unwrap();
        let (c, d) = Config::from_table(&t);
        assert!(d.ok() && d.warnings.is_empty(), "{d:#?}");
        let ff = c.app_policy("Org.Mozilla.Firefox.desktop");
        assert_eq!(
            (ff.controls, ff.whole_bar, ff.pin.as_deref()),
            (false, false, Some("org.example.Tabs"))
        );
        let other = c.app_policy("foot");
        assert_eq!(
            (other.controls, other.whole_bar, other.pin),
            (true, false, None),
            "'*' defaults"
        );
        assert!(!c.provider_prefs("org.example.Pomodoro").show);
        assert_eq!(c.provider_prefs("org.example.Pomodoro").order, Some(2));
        assert!(c.provider_prefs("com.spotify.Client").allow);
        assert_eq!(c.provider_prefs("unknown"), ProviderPrefs::default());
        assert!(!c.api.target);
        let (_, d) = Config::from_table(&"[apps.x]\ncontrol = true\n".parse().unwrap());
        assert_eq!(d.warnings.len(), 1, "typo warned: {d:?}");
    }

    #[test]
    fn diagnostics_have_paths() {
        let t: Table = r#"
            version = 2
            typo = 1
            [bar]
            fn-mode = "sideways"
            [[layouts.x.zones]]
            modules = ["key/NoSuchKey", 5]
            [[rules]]
            when = { app-id = "foot" }
        "#
        .parse()
        .unwrap();
        let (_, d) = Config::from_table(&t);
        let errs: Vec<String> = d.errors.iter().map(|i| i.path.clone()).collect();
        assert!(errs.contains(&"version".into()), "{errs:?}");
        assert!(errs.contains(&"bar.fn-mode".into()), "{errs:?}");
        assert!(
            errs.contains(&"layouts.x.zones[0]".into()),
            "zone without name: {errs:?}"
        );
        assert!(
            errs.contains(&"layouts.x.zones[0].modules[0]".into()),
            "{errs:?}"
        );
        assert!(
            errs.contains(&"layouts.x.zones[0].modules[1]".into()),
            "{errs:?}"
        );
        assert!(errs.contains(&"rules[0]".into()), "{errs:?}");
        assert_eq!(
            d.warnings
                .iter()
                .map(|i| i.path.as_str())
                .collect::<Vec<_>>(),
            vec!["typo"]
        );
    }
}
