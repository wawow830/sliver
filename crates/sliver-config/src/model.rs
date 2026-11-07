//! The parsed config (sliver-config(5)).

use std::collections::BTreeMap;
use std::time::Duration;

use toml::{Table, Value};

use crate::action::Action;
use crate::condition::Condition;
use crate::diag::{Diagnostics, Reader, join};
use crate::module::ModuleId;
use crate::resolve::Parsed;
use crate::values::{Align, FireOn, Width, parse_duration};

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
    /// Adaptive brightness curve: `(screen, bar)` in percent (0–100) of the main display's
    /// brightness and of `max_brightness`, screens strictly increasing, 2–16 points. Empty =
    /// tiny-dfr's `sqrt` (the default). Math: `sliver_ipc::curve` (per-mille = percent × 10).
    pub curve: Vec<(f32, f32)>,
    /// `[display.on-battery]`: replaces the values above while running on battery.
    pub on_battery: DisplayOverride,
}

/// Values that replace [`Display`]'s while on battery. `None` = keep the normal value. The
/// timeouts are doubly optional: `Some(None)` is an explicit "never".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisplayOverride {
    pub max_brightness: Option<u8>,
    pub dim_after: Option<Option<Duration>>,
    pub off_after: Option<Option<Duration>>,
}

impl Display {
    /// The values in effect now: on battery, `[display.on-battery]` replaces what it sets.
    pub fn effective(&self, on_battery: bool) -> Display {
        let mut x = self.clone();
        if on_battery {
            let o = &self.on_battery;
            x.max_brightness = o.max_brightness.unwrap_or(x.max_brightness);
            x.dim_after = o.dim_after.unwrap_or(x.dim_after);
            x.off_after = o.off_after.unwrap_or(x.off_after);
        }
        x.on_battery = DisplayOverride::default();
        x
    }
}

impl Default for Display {
    fn default() -> Self {
        Self {
            adaptive: true,
            max_brightness: 128,
            dim_after: Some(Duration::from_secs(30)),
            off_after: Some(Duration::from_secs(60)),
            pixel_shift: false,
            curve: Vec::new(),
            on_battery: DisplayOverride::default(),
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
    /// `{ type = "custom", format = "TTY1", … }`. The id gets a generated instance name. The
    /// block is parsed when the config loads.
    Inline(ModuleId, Table, std::sync::Arc<Parsed>),
}

impl ModuleRef {
    pub fn id(&self) -> &ModuleId {
        match self {
            ModuleRef::Named(id) | ModuleRef::Inline(id, ..) => id,
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

/// Bar-wide gestures (sliver-config(5)).
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
    /// The built-in base theme (`sliver`, `classic`) under the user's stylesheet. `None` =
    /// `sliver`.
    pub theme: Option<String>,
    pub bar: Bar,
    pub display: Display,
    /// Layer name → its default layout. `base` and `fn` are reserved.
    pub layers: BTreeMap<String, String>,
    pub layer_options: BTreeMap<String, LayerOptions>,
    /// Only loading sets it, so [`Config::module`]'s parsed modules agree with it.
    pub(crate) layouts: BTreeMap<String, Layout>,
    pub rules: Vec<Rule>,
    /// Module blocks, keyed by the canonical id (so `[modules.pulseaudio]` configures `audio`).
    /// Only loading sets it (see `layouts`).
    pub(crate) modules: BTreeMap<ModuleId, Table>,
    /// Every named module the config refers to (blocks, zones, groups), parsed when it loads.
    pub(crate) parsed: BTreeMap<ModuleId, std::sync::Arc<Parsed>>,
    pub gestures: BTreeMap<String, Action>,
}

impl Config {
    pub fn layouts(&self) -> &BTreeMap<String, Layout> {
        &self.layouts
    }

    /// The `[modules]` blocks, keyed by canonical id.
    pub fn modules(&self) -> &BTreeMap<ModuleId, Table> {
        &self.modules
    }
}

/// Merged underneath every config (sliver-config(5)).
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
        // Before layouts: an inline module is parsed with its named block.
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
        if let Some(layouts) = r.table("layouts", &mut d) {
            for (name, v) in layouts {
                let p = join("layouts", name);
                match v.as_table() {
                    Some(t) => {
                        c.layouts
                            .insert(name.clone(), parse_layout(name, t, &p, &c.modules, &mut d));
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
        c.parsed = c.parse_named();
        (c, d)
    }
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
    if let Some(v) = r.get("curve") {
        x.curve = parse_curve(v, "display.curve", d);
    }
    if let Some(t) = r.table("on-battery", d) {
        let mut b = Reader::new(t, "display.on-battery");
        x.on_battery.max_brightness = b.uint("max-brightness", 255, d).map(|n| n as u8);
        if let Some(v) = b.get("dim-after") {
            x.on_battery.dim_after = Some(optional_duration(v, "display.on-battery.dim-after", d));
        }
        if let Some(v) = b.get("off-after") {
            x.on_battery.off_after = Some(optional_duration(v, "display.on-battery.off-after", d));
        }
        b.finish(d);
    }
    r.finish(d);
    x
}

/// `curve = [[screen, bar], ...]`: percent 0–100, screens strictly increasing, 2–16 points. Any
/// problem is an error and leaves the default (sqrt).
fn parse_curve(v: &Value, path: &str, d: &mut Diagnostics) -> Vec<(f32, f32)> {
    let num = |v: &Value| match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    };
    let Value::Array(points) = v else {
        d.error(path, "expected [[screen, bar], ...]");
        return Vec::new();
    };
    if !(2..=16).contains(&points.len()) {
        d.error(path, format!("needs 2 to 16 points, got {}", points.len()));
        return Vec::new();
    }
    let mut out = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let pair = match p {
            Value::Array(a) if a.len() == 2 => num(&a[0]).zip(num(&a[1])),
            _ => None,
        };
        let Some((sx, bx)) = pair else {
            d.error(
                format!("{path}[{i}]"),
                "expected [screen, bar] (two numbers)",
            );
            return Vec::new();
        };
        if !(0.0..=100.0).contains(&sx) || !(0.0..=100.0).contains(&bx) {
            d.error(
                format!("{path}[{i}]"),
                "screen and bar are percentages from 0 to 100",
            );
            return Vec::new();
        }
        if out
            .last()
            .is_some_and(|&(prev, _): &(f32, f32)| sx as f32 <= prev)
        {
            d.error(
                format!("{path}[{i}]"),
                "screen values must be strictly increasing",
            );
            return Vec::new();
        }
        out.push((sx as f32, bx as f32));
    }
    out
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

fn parse_layout(
    name: &str,
    t: &Table,
    path: &str,
    blocks: &BTreeMap<ModuleId, Table>,
    d: &mut Diagnostics,
) -> Layout {
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
                        if let Some(z) = parse_zone(name, zt, &p, blocks, d) {
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

fn parse_zone(
    layout: &str,
    t: &Table,
    path: &str,
    blocks: &BTreeMap<ModuleId, Table>,
    d: &mut Diagnostics,
) -> Option<Zone> {
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
                    Value::Table(t) => parse_inline(layout, &z.name, i, t, &p, blocks),
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

fn parse_inline(
    layout: &str,
    zone: &str,
    i: usize,
    t: &Table,
    path: &str,
    blocks: &BTreeMap<ModuleId, Table>,
) -> Result<ModuleRef, String> {
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
    let parsed = Parsed::new(
        &id,
        &crate::resolve::merged_block(&id, blocks.get(&id), &block),
        path,
    );
    Ok(ModuleRef::Inline(id, block, std::sync::Arc::new(parsed)))
}

fn parse_rule(t: &Table, path: &str, d: &mut Diagnostics) -> Option<Rule> {
    let mut r = Reader::new(t, path);
    let name = r.string("name", d);
    let when = match r.get("when") {
        Some(v) => Condition::parse(v, &join(path, "when"), d),
        None => Some(Condition::always()),
    };
    if when.as_ref().is_some_and(|w| w.mentions("layout")) {
        d.warn(
            join(path, "when"),
            "rules choose the layout, so `layout` is unknown here and never matches",
        );
    }
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
    fn on_battery_display_override() {
        let t: Table = r#"
            [display]
            max-brightness = 200
            dim-after = "30s"
            [display.on-battery]
            max-brightness = 60
            off-after = "never"
        "#
        .parse()
        .unwrap();
        let (c, d) = Config::from_table(&t);
        assert!(d.ok() && d.warnings.is_empty(), "{d:#?}");
        let ac = c.display.effective(false);
        assert_eq!(ac.max_brightness, 200);
        assert_eq!(ac.off_after, Some(Duration::from_secs(60)));
        let bat = c.display.effective(true);
        assert_eq!(bat.max_brightness, 60);
        assert_eq!(
            bat.dim_after,
            Some(Duration::from_secs(30)),
            "unset keys keep [display]"
        );
        assert_eq!(bat.off_after, None, "explicit never");
        // No table: battery = AC.
        assert_eq!(Display::default().effective(true), Display::default());

        let t: Table = "[display.on-battery]\nmax-brightness = 300\nbogus = 1\n"
            .parse()
            .unwrap();
        let (_, d) = Config::from_table(&t);
        assert!(!d.ok(), "out of range is an error");
        assert!(
            d.warnings
                .iter()
                .any(|w| w.path == "display.on-battery.bogus"),
            "{d:#?}"
        );
    }

    #[test]
    fn display_curve() {
        let t: Table = "[display]\ncurve = [[0, 5], [50, 40.5], [100, 100]]\n"
            .parse()
            .unwrap();
        let (c, d) = Config::from_table(&t);
        assert!(d.ok() && d.warnings.is_empty(), "{d:#?}");
        assert_eq!(
            c.display.curve,
            vec![(0.0, 5.0), (50.0, 40.5), (100.0, 100.0)]
        );
        assert!(
            Display::default().curve.is_empty(),
            "default: tiny-dfr's sqrt"
        );
        for (bad, path) in [
            ("curve = [[0, 0]]", "display.curve"),
            ("curve = 3", "display.curve"),
            ("curve = [[0, 0], [1]]", "display.curve[1]"),
            ("curve = [[0, 0], [101, 1]]", "display.curve[1]"),
            ("curve = [[0, 0], [50, -1]]", "display.curve[1]"),
            ("curve = [[50, 0], [50, 100]]", "display.curve[1]"),
            ("curve = [[0, 0], [\"x\", 1]]", "display.curve[1]"),
        ] {
            let t: Table = format!("[display]\n{bad}\n").parse().unwrap();
            let (c, d) = Config::from_table(&t);
            assert!(!d.ok(), "{bad}");
            assert!(d.errors.iter().any(|e| e.path == path), "{bad}: {d:#?}");
            assert!(c.display.curve.is_empty(), "{bad}: falls back to sqrt");
        }
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
