//! Type-specific module keys: the schema ([`MODULE_TYPES`]), and the one parse of a block's
//! keys by it, when the config loads.

use std::collections::BTreeMap;
use std::time::Duration;

use toml::{Table, Value};

use crate::diag::{Diagnostics, Reader, join};
use crate::module::ModuleId;
use crate::rewrite::Rewrite;
use crate::values::{Width, parse_duration};

/// What a type-specific key holds. Loading reads each value by its kind, once, reporting what
/// doesn't fit, into an [`Opt`]: the runtime reads only those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Flag,
    Text,
    /// One of these strings.
    OneOf(&'static [&'static str]),
    /// A list of strings.
    Texts,
    /// An integer in this range; others are errors.
    Int(i64, i64),
    /// An integer, clamped into this range with a warning.
    Clamped(i64, i64),
    /// An integer or float.
    Number,
    /// A duration (`"5s"`, `"200ms"`, or seconds), raised to this floor (ms) with a warning.
    Duration(u64),
    Width,
    /// A module reference.
    Module,
    /// A list of module references.
    Modules,
    Rewrite,
    /// A group's `drawer` table.
    Drawer,
    /// Environment variables: string (or integer) values.
    Env,
}

/// Highest `signal` offset (glibc has 30 real-time signals available to applications).
pub const MAX_SIGNAL: i64 = 30;

/// A group of keys and what each holds.
pub type Keys = &'static [(&'static str, Kind)];

/// What a script behind a module takes (`custom`, and `image` and `graph` fed by one).
const SCRIPT: Keys = &[
    ("exec", Kind::Text),
    ("exec-if", Kind::Text),
    ("restart-interval", Kind::Duration(1000)),
    ("signal", Kind::Int(1, MAX_SIGNAL)),
    ("exec-on-event", Kind::Flag),
    ("return-type", Kind::OneOf(&["", "json"])),
    ("escape", Kind::Flag),
    ("timeout", Kind::Duration(50)),
    ("env", Kind::Env),
];
/// A module with a value drawn as a slider.
const SLIDER: Keys = &[("slider", Kind::Flag), ("collapsed", Kind::Flag)];
/// Every built-in module type (sliver-config(5)) and the keys it takes besides
/// [`crate::COMMON_KEYS`]. Other keys are warned about and dropped. Unknown types are a warning, because a newer agent
/// may know them.
pub const MODULE_TYPES: &[(&str, &[Keys])] = &[
    ("key", &[]),
    ("spacer", &[]),
    (
        "clock",
        &[&[
            ("locale", Kind::Text),
            ("timezone", Kind::Text),
            ("timezones", Kind::Texts),
        ]],
    ),
    ("battery", &[]),
    ("backlight", &[SLIDER]),
    ("kbd-backlight", &[SLIDER]),
    (
        "audio",
        &[&[("node", Kind::OneOf(&["sink", "source"]))], SLIDER],
    ),
    ("mpris", &[&[("art", Kind::Flag), ("scrub", Kind::Flag)]]),
    ("workspaces", &[&[("all-outputs", Kind::Flag)]]),
    ("window", &[&[("rewrite", Kind::Rewrite)]]),
    ("mode", &[]),
    ("language", &[]),
    ("idle-inhibitor", &[]),
    ("dnd", &[]),
    ("dark-mode", &[]),
    ("screenshot", &[]),
    ("lock", &[]),
    ("power", &[]),
    ("power-profiles", &[]),
    ("cpu", &[]),
    ("memory", &[]),
    (
        "temperature",
        &[&[
            ("thermal-zone", Kind::Int(0, 1023)),
            ("hwmon-path", Kind::Text),
            ("critical-threshold", Kind::Number),
        ]],
    ),
    ("disk", &[&[("path", Kind::Text)]]),
    ("load", &[]),
    ("network", &[]),
    ("bluetooth", &[]),
    (
        "graph",
        &[
            &[
                ("source", Kind::Module),
                ("samples", Kind::Clamped(2, 600)),
                ("min", Kind::Number),
                ("max", Kind::Number),
            ],
            SCRIPT,
        ],
    ),
    (
        "systemd-failed-units",
        &[&[
            ("hide-on-ok", Kind::Flag),
            ("system", Kind::Flag),
            ("user", Kind::Flag),
        ]],
    ),
    (
        "taskbar",
        &[&[("all-workspaces", Kind::Flag), ("pinned", Kind::Texts)]],
    ),
    ("notifications", &[]),
    ("timer", &[&[("duration", Kind::Duration(1000))]]),
    ("image", &[&[("path", Kind::Text)], SCRIPT]),
    (
        "group",
        &[&[
            ("modules", Kind::Modules),
            ("orientation", Kind::OneOf(&["horizontal", "vertical"])),
            ("drawer", Kind::Drawer),
            ("module-width", Kind::Width),
        ]],
    ),
    ("custom", &[SLIDER, SCRIPT]),
];

/// The keys type `kind` declares for itself in [`MODULE_TYPES`] (none for an unknown type).
pub fn own_keys(kind: &str) -> impl Iterator<Item = &'static str> {
    own(kind).map(|(k, _)| k)
}

fn own(kind: &str) -> impl Iterator<Item = (&'static str, Kind)> {
    let groups = MODULE_TYPES
        .iter()
        .find(|(t, _)| *t == kind)
        .map(|(_, g)| *g);
    groups.into_iter().flatten().flat_map(|g| g.iter().copied())
}

/// What `key` holds on a module of type `kind`: `None` for an unknown type, `Some(None)` for a
/// key the type doesn't take.
pub fn key_kind(kind: &str, key: &str) -> Option<Option<Kind>> {
    declared(kind, key).map(|o| o.map(|(_, k)| k))
}

fn declared(kind: &str, key: &str) -> Option<Option<(&'static str, Kind)>> {
    MODULE_TYPES.iter().find(|(t, _)| *t == kind)?;
    Some(own(kind).find(|(k, _)| *k == key))
}

/// Does a module of type `kind` take `key` (besides the common keys)? `None`: an unknown type.
pub fn takes_key(kind: &str, key: &str) -> Option<bool> {
    key_kind(kind, key).map(|k| k.is_some())
}

/// A type-specific key's value, read by its [`Kind`].
#[derive(Debug, Clone, PartialEq)]
pub enum Opt {
    Flag(bool),
    /// [`Kind::Text`] and [`Kind::OneOf`].
    Text(String),
    Texts(Vec<String>),
    /// [`Kind::Int`] and [`Kind::Clamped`].
    Int(i64),
    Number(f64),
    Duration(Duration),
    Width(Width),
    Module(ModuleId),
    Modules(Vec<ModuleId>),
    Rewrite(Rewrite),
    Drawer(Drawer),
    Env(Vec<(String, String)>),
}

/// A group's `drawer` (sliver-config(5)).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Drawer {
    /// `mode = "bar"`: the rest opens as a sub-bar, not in place.
    pub bar: bool,
    pub start_expanded: bool,
    pub collapse_after: Option<Duration>,
    pub children_class: Option<String>,
    /// `transition-left-to-right = false`: the leader comes after the rest.
    pub leader_last: bool,
}

/// The type-specific keys of a module block (what [`crate::Common`] left), by the schema.
/// Keys of an unknown type are left out unread.
pub(crate) fn parse(
    id: &ModuleId,
    rest: &Table,
    path: &str,
    d: &mut Diagnostics,
) -> BTreeMap<&'static str, Opt> {
    let mut out = BTreeMap::new();
    for (k, v) in rest {
        let p = join(path, k);
        match declared(&id.kind, k) {
            None => {}
            Some(None) => d.warn(p, format!("unknown key for {} modules (ignored)", id.kind)),
            Some(Some((name, kind))) => {
                if let Some(o) = read(kind, v, &p, d) {
                    out.insert(name, o);
                }
            }
        }
    }
    out
}

fn read(kind: Kind, v: &Value, p: &str, d: &mut Diagnostics) -> Option<Opt> {
    let int = |d: &mut Diagnostics| {
        let n = v.as_integer();
        if n.is_none() {
            d.error(p, "expected an integer");
        }
        n
    };
    let fail = |d: &mut Diagnostics, what: &str| {
        d.error(p, what.to_owned());
        None
    };
    Some(match kind {
        Kind::Flag => match v.as_bool() {
            Some(b) => Opt::Flag(b),
            None => return fail(d, "expected true or false"),
        },
        Kind::Text => match v.as_str() {
            Some(s) => Opt::Text(s.to_owned()),
            None => return fail(d, "expected a string"),
        },
        Kind::OneOf(choices) => match v.as_str() {
            Some(s) if choices.contains(&s) => Opt::Text(s.to_owned()),
            _ => {
                let list: Vec<String> = choices.iter().map(|c| format!("{c:?}")).collect();
                return fail(d, &format!("expected one of {}", list.join(", ")));
            }
        },
        Kind::Texts => match v {
            Value::String(s) => Opt::Texts(vec![s.clone()]),
            Value::Array(a) => match a.iter().map(Value::as_str).collect::<Option<Vec<_>>>() {
                Some(list) => Opt::Texts(list.into_iter().map(str::to_owned).collect()),
                None => return fail(d, "expected a list of strings"),
            },
            _ => return fail(d, "expected a list of strings"),
        },
        Kind::Int(lo, hi) => match int(d)? {
            n if (lo..=hi).contains(&n) => Opt::Int(n),
            _ => return fail(d, &format!("expected an integer from {lo} to {hi}")),
        },
        Kind::Clamped(lo, hi) => {
            let n = int(d)?;
            if !(lo..=hi).contains(&n) {
                d.warn(p, format!("outside {lo}–{hi}; using {}", n.clamp(lo, hi)));
            }
            Opt::Int(n.clamp(lo, hi))
        }
        Kind::Number => match v.as_float().or(v.as_integer().map(|i| i as f64)) {
            Some(n) => Opt::Number(n),
            None => return fail(d, "expected a number"),
        },
        Kind::Duration(floor_ms) => {
            let t = parse_duration(v).map_err(|e| d.error(p, e)).ok()?;
            let floor = Duration::from_millis(floor_ms);
            if t < floor {
                d.warn(p, format!("below the {floor:?} floor; using {floor:?}"));
            }
            Opt::Duration(t.max(floor))
        }
        Kind::Width => Opt::Width(Width::parse(v).map_err(|e| d.error(p, e)).ok()?),
        Kind::Module => match v.as_str().map(ModuleId::parse) {
            Some(Ok(id)) => Opt::Module(id),
            Some(Err(e)) => return fail(d, &e),
            None => return fail(d, "expected a module name"),
        },
        Kind::Modules => {
            let Some(list) = v.as_array() else {
                return fail(d, "expected a list of module names");
            };
            let mut ids = Vec::new();
            for (i, v) in list.iter().enumerate() {
                match v
                    .as_str()
                    .ok_or("expected a module name".to_owned())
                    .and_then(ModuleId::parse)
                {
                    Ok(id) => ids.push(id),
                    Err(e) => d.error(format!("{p}[{i}]"), e),
                }
            }
            Opt::Modules(ids)
        }
        Kind::Rewrite => Opt::Rewrite(Rewrite::parse(v, p, d)),
        Kind::Drawer => match v.as_table() {
            Some(t) => Opt::Drawer(drawer(t, p, d)),
            None => return fail(d, "expected a table"),
        },
        Kind::Env => {
            let Some(t) = v.as_table() else {
                return fail(d, "expected a table of strings");
            };
            let mut env = Vec::new();
            for (k, v) in t {
                match v {
                    Value::String(s) => env.push((k.clone(), s.clone())),
                    Value::Integer(n) => env.push((k.clone(), n.to_string())),
                    _ => d.error(join(p, k), "expected a string"),
                }
            }
            Opt::Env(env)
        }
    })
}

fn drawer(t: &Table, path: &str, d: &mut Diagnostics) -> Drawer {
    let mut r = Reader::new(t, path);
    // waybar's; a tap always opens a drawer.
    let _ = r.string("reveal", d);
    let mode = r.string("mode", d);
    if mode
        .as_deref()
        .is_some_and(|m| !matches!(m, "inline" | "bar"))
    {
        d.error(r.path("mode"), "expected \"inline\" or \"bar\"");
    }
    let collapse_after = r.get("collapse-after").and_then(|v| {
        parse_duration(v)
            .map_err(|e| d.error(join(path, "collapse-after"), e))
            .ok()
    });
    let drawer = Drawer {
        bar: mode.as_deref() == Some("bar"),
        start_expanded: r.bool("start-expanded", d).unwrap_or(false),
        collapse_after,
        children_class: r.string("children-class", d),
        leader_last: r.bool("transition-left-to-right", d) == Some(false),
    };
    r.finish(d);
    drawer
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_block(id: &str, block: &str) -> (BTreeMap<&'static str, Opt>, Diagnostics) {
        let mut d = Diagnostics::default();
        let o = parse(
            &ModuleId::parse(id).unwrap(),
            &block.parse().unwrap(),
            "m",
            &mut d,
        );
        (o, d)
    }

    #[test]
    fn values_are_read_by_their_kind() {
        let (o, d) = parse_block(
            "group/g",
            r#"
            modules = ["clock", "custom/a#1"]
            module-width = 90
            orientation = "vertical"
            drawer = { mode = "bar", collapse-after = "3s", transition-left-to-right = false }
            "#,
        );
        assert!(d.ok() && d.warnings.is_empty(), "{d:?}");
        assert_eq!(
            o["modules"],
            Opt::Modules(vec![
                ModuleId::parse("clock").unwrap(),
                ModuleId::parse("custom/a#1").unwrap()
            ])
        );
        assert_eq!(o["module-width"], Opt::Width(Width::Fixed(90)));
        assert_eq!(
            o["drawer"],
            Opt::Drawer(Drawer {
                bar: true,
                collapse_after: Some(Duration::from_secs(3)),
                leader_last: true,
                ..Drawer::default()
            })
        );
        let (o, _) = parse_block("clock", "timezones = \"UTC\"");
        assert_eq!(o["timezones"], Opt::Texts(vec!["UTC".into()]));
        let (o, _) = parse_block("graph/g", "samples = 1000");
        assert_eq!(o["samples"], Opt::Int(600));
    }

    #[test]
    fn what_does_not_fit_is_reported_and_left_out() {
        let (o, d) = parse_block(
            "custom/x",
            r#"
            exec = 3
            signal = 99
            return-type = "xml"
            env = { A = "1", B = [] }
            restart-interval = "10ms"
            timeout = "soon"
            slider = "yes"
            bogus = 1
            "#,
        );
        let errors: Vec<&str> = d.errors.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            errors,
            [
                "m.env.B",
                "m.exec",
                "m.return-type",
                "m.signal",
                "m.slider",
                "m.timeout"
            ]
        );
        let warnings: Vec<&str> = d.warnings.iter().map(|w| w.path.as_str()).collect();
        assert_eq!(warnings, ["m.bogus", "m.restart-interval"]);
        assert_eq!(
            o.keys().copied().collect::<Vec<_>>(),
            ["env", "restart-interval"]
        );
        assert_eq!(o["restart-interval"], Opt::Duration(Duration::from_secs(1)));
        // Unknown types aren't read.
        let (o, d) = parse_block("from-the-future", "anything = 1");
        assert!(o.is_empty() && d.ok() && d.warnings.is_empty());
    }
}
