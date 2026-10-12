//! `when = { … }` conditions over the agent's flat context (customization.md §6).

use std::collections::BTreeMap;

use toml::{Table, Value};

use crate::diag::Diagnostics;

/// A value the agent reports about the current state.
#[derive(Debug, Clone, PartialEq)]
pub enum CtxValue {
    Bool(bool),
    Num(f64),
    Str(String),
}

impl From<bool> for CtxValue {
    fn from(b: bool) -> Self {
        CtxValue::Bool(b)
    }
}
impl From<f64> for CtxValue {
    fn from(n: f64) -> Self {
        CtxValue::Num(n)
    }
}
impl From<i64> for CtxValue {
    fn from(n: i64) -> Self {
        CtxValue::Num(n as f64)
    }
}
impl From<u32> for CtxValue {
    fn from(n: u32) -> Self {
        CtxValue::Num(n.into())
    }
}
impl From<&str> for CtxValue {
    fn from(s: &str) -> Self {
        CtxValue::Str(s.to_owned())
    }
}
impl From<String> for CtxValue {
    fn from(s: String) -> Self {
        CtxValue::Str(s)
    }
}

/// The agent's flat state: compositor + session + agent keys (integration.md §1).
/// Missing keys mean "unknown", which never matches.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Context {
    values: BTreeMap<String, CtxValue>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: &str, value: impl Into<CtxValue>) -> &mut Self {
        self.values.insert(canonical_key(key), value.into());
        self
    }

    pub fn with(mut self, key: &str, value: impl Into<CtxValue>) -> Self {
        self.set(key, value);
        self
    }

    pub fn get(&self, key: &str) -> Option<&CtxValue> {
        self.values.get(&canonical_key(key))
    }

    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some(CtxValue::Bool(true)))
    }
}

/// Context keys sliver knows about. Others are allowed but get an "unknown key" warning.
pub const CONTEXT_KEYS: &[&str] = &[
    // compositor (sliver-desktop)
    "app_id",
    "desktop_id",
    "title",
    "pid",
    "fullscreen",
    "floating",
    "xwayland",
    "workspace",
    "workspace_name",
    "special_workspace",
    "monitor",
    "monitors",
    "external_display",
    "keyboard_layout",
    "submap",
    "screencast",
    // session (sliver-session)
    "locked",
    "idle",
    "sleeping",
    "on_battery",
    "battery_pct",
    "battery_state",
    "lid_closed",
    "power_profile",
    "media_status",
    "media_player",
    "media_title",
    "media_artist",
    // audio/brightness/notifications (T19)
    "mic_muted",
    "output_muted",
    "volume",
    "dnd",
    "caffeine",
    "brightness",
    // agent
    "time",
    "weekday",
    "physical_esc",
    "layer",
    "layout",
];

/// `app-id`, `App_ID` and `power.on_battery` all become their canonical snake_case key.
pub fn canonical_key(k: &str) -> String {
    let k = k.trim().to_ascii_lowercase().replace('-', "_");
    match k.as_str() {
        "power.on_battery" => "on_battery".into(),
        "power.battery_pct" => "battery_pct".into(),
        "power.battery_state" => "battery_state".into(),
        "power.lid_closed" => "lid_closed".into(),
        "power.profile" => "power_profile".into(),
        "media.status" => "media_status".into(),
        "media.player" => "media_player".into(),
        "media.title" => "media_title".into(),
        "media.artist" => "media_artist".into(),
        "workspace.id" => "workspace".into(),
        "workspace.name" => "workspace_name".into(),
        _ => k.replace('.', "_"),
    }
}

#[derive(Debug, Clone)]
pub struct Regex(regex_lite::Regex);

impl PartialEq for Regex {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Matcher {
    /// Equality. Strings compare case-insensitively.
    Is(CtxValue),
    OneOf(Vec<CtxValue>),
    Regex(Regex),
    Contains(String),
    Below(f64),
    Above(f64),
    /// Inclusive range. For strings like `"22:00".."06:00"` it wraps around when from > to.
    Between(CtxValue, CtxValue),
}

/// All `all` keys match, at least one `any` branch matches (if there are any), and `not` doesn't.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Condition {
    pub all: Vec<(String, Matcher)>,
    pub any: Vec<Condition>,
    pub not: Option<Box<Condition>>,
}

const OPS: &[&str] = &["regex", "contains", "below", "above", "between", "is"];

impl Condition {
    /// The always-true condition.
    pub fn always() -> Self {
        Self::default()
    }

    pub fn parse(v: &Value, path: &str, d: &mut Diagnostics) -> Option<Condition> {
        match v {
            Value::Table(t) => Some(Self::parse_table(t, path, "", d)),
            other => {
                d.error(
                    path,
                    format!(
                        "a condition is a table like {{ app-id = \"firefox\" }}, got {}",
                        other.type_str()
                    ),
                );
                None
            }
        }
    }

    fn parse_table(t: &Table, path: &str, prefix: &str, d: &mut Diagnostics) -> Condition {
        let mut c = Condition::default();
        for (k, v) in t {
            let p = crate::diag::join(path, k);
            match (prefix.is_empty(), k.as_str(), v) {
                (true, "any", Value::Array(branches)) => {
                    for (i, b) in branches.iter().enumerate() {
                        if let Some(b) = Self::parse(b, &format!("{p}[{i}]"), d) {
                            c.any.push(b);
                        }
                    }
                }
                (true, "not", v) => c.not = Self::parse(v, &p, d).map(Box::new),
                // `{ power = { on_battery = true } }`, i.e. TOML dotted keys: flatten.
                (_, _, Value::Table(inner)) if !inner.keys().all(|k| OPS.contains(&k.as_str())) => {
                    let sub = Self::parse_table(inner, &p, &format!("{prefix}{k}."), d);
                    c.all.extend(sub.all);
                }
                _ => {
                    let key = canonical_key(&format!("{prefix}{k}"));
                    if !CONTEXT_KEYS.contains(&key.as_str()) {
                        d.warn(&p, format!("unknown context key {key:?}; it never matches unless something provides it"));
                    }
                    if let Some(m) = parse_matcher(v, &p, d) {
                        c.all.push((key, m));
                    }
                }
            }
        }
        c
    }

    pub fn eval(&self, ctx: &Context) -> bool {
        self.all.iter().all(|(k, m)| m.eval(k, ctx))
            && (self.any.is_empty() || self.any.iter().any(|c| c.eval(ctx)))
            && !self.not.as_ref().is_some_and(|n| n.eval(ctx))
    }

    pub fn is_always(&self) -> bool {
        self.all.is_empty() && self.any.is_empty() && self.not.is_none()
    }
}

fn literal(v: &Value) -> Option<CtxValue> {
    Some(match v {
        Value::Boolean(b) => CtxValue::Bool(*b),
        Value::Integer(i) => CtxValue::Num(*i as f64),
        Value::Float(f) => CtxValue::Num(*f),
        Value::String(s) => CtxValue::Str(s.clone()),
        _ => return None,
    })
}

fn parse_matcher(v: &Value, path: &str, d: &mut Diagnostics) -> Option<Matcher> {
    if let Some(l) = literal(v) {
        return Some(Matcher::Is(l));
    }
    match v {
        Value::Array(items) => {
            let lits: Option<Vec<CtxValue>> = items.iter().map(literal).collect();
            lits.map(Matcher::OneOf).or_else(|| {
                d.error(
                    path,
                    "a one-of list may only contain strings, numbers or booleans",
                );
                None
            })
        }
        Value::Table(t) if t.len() == 1 => {
            let (op, arg) = t.iter().next().expect("len 1");
            let num = |d: &mut Diagnostics| match arg {
                Value::Integer(i) => Some(*i as f64),
                Value::Float(f) => Some(*f),
                _ => {
                    d.error(path, format!("{op} needs a number"));
                    None
                }
            };
            match op.as_str() {
                "is" => literal(arg).map(Matcher::Is),
                "regex" => match arg.as_str().map(regex_lite::Regex::new) {
                    Some(Ok(re)) => Some(Matcher::Regex(Regex(re))),
                    Some(Err(e)) => {
                        d.error(path, format!("bad regex: {e}"));
                        None
                    }
                    None => {
                        d.error(path, "regex needs a string");
                        None
                    }
                },
                "contains" => arg.as_str().map(|s| Matcher::Contains(s.to_lowercase())),
                "below" => num(d).map(Matcher::Below),
                "above" => num(d).map(Matcher::Above),
                "between" => match arg.as_array().map(|a| a.as_slice()) {
                    Some([a, b]) => literal(a)
                        .zip(literal(b))
                        .map(|(a, b)| Matcher::Between(a, b)),
                    _ => {
                        d.error(path, "between needs [from, to]");
                        None
                    }
                },
                _ => unreachable!("filtered by OPS"),
            }
        }
        _ => {
            d.error(path, format!("can't match on {}; use a value, a list, or {{ regex|contains|below|above|between = … }}", v.type_str()));
            None
        }
    }
}

fn eq(a: &CtxValue, b: &CtxValue) -> bool {
    match (a, b) {
        (CtxValue::Str(a), CtxValue::Str(b)) => a.eq_ignore_ascii_case(b),
        (CtxValue::Num(a), CtxValue::Num(b)) => a == b,
        (CtxValue::Bool(a), CtxValue::Bool(b)) => a == b,
        // `workspace = "3"` vs a numeric workspace id.
        (CtxValue::Str(s), CtxValue::Num(n)) | (CtxValue::Num(n), CtxValue::Str(s)) => {
            s.parse::<f64>() == Ok(*n)
        }
        _ => false,
    }
}

fn le(a: &CtxValue, b: &CtxValue) -> Option<bool> {
    match (a, b) {
        (CtxValue::Num(a), CtxValue::Num(b)) => Some(a <= b),
        (CtxValue::Str(a), CtxValue::Str(b)) => Some(a <= b),
        _ => None,
    }
}

fn as_text(v: &CtxValue) -> String {
    match v {
        CtxValue::Str(s) => s.clone(),
        CtxValue::Num(n) => n.to_string(),
        CtxValue::Bool(b) => b.to_string(),
    }
}

impl Matcher {
    fn eval(&self, key: &str, ctx: &Context) -> bool {
        // app_id also matches the .desktop id, so `org.mozilla.firefox` and `firefox` both work.
        let mut candidates: Vec<&CtxValue> = ctx.get(key).into_iter().collect();
        let desktop = match (key, ctx.get("desktop_id")) {
            ("app_id", Some(CtxValue::Str(id))) => {
                Some(CtxValue::Str(id.trim_end_matches(".desktop").to_owned()))
            }
            _ => None,
        };
        candidates.extend(desktop.as_ref());
        candidates.into_iter().any(|v| self.eval_one(v))
    }

    fn eval_one(&self, v: &CtxValue) -> bool {
        match self {
            Matcher::Is(want) => eq(v, want),
            Matcher::OneOf(list) => list.iter().any(|w| eq(v, w)),
            Matcher::Regex(re) => re.0.is_match(&as_text(v)),
            Matcher::Contains(s) => as_text(v).to_lowercase().contains(s),
            Matcher::Below(n) => matches!(v, CtxValue::Num(x) if x < n),
            Matcher::Above(n) => matches!(v, CtxValue::Num(x) if x > n),
            Matcher::Between(from, to) => match (le(from, v), le(v, to), le(from, to)) {
                (Some(a), Some(b), Some(true)) => a && b,
                // wraps around, e.g. 22:00..06:00
                (Some(a), Some(b), Some(false)) => a || b,
                _ => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cond(src: &str) -> (Condition, Diagnostics) {
        let t: Table = format!("when = {src}").parse().unwrap();
        let mut d = Diagnostics::default();
        (Condition::parse(&t["when"], "when", &mut d).unwrap(), d)
    }

    #[test]
    fn app_id_and_lists() {
        let (c, d) = cond(r#"{ app-id = ["foot", "kitty"] }"#);
        assert!(d.ok() && d.warnings.is_empty(), "{d:?}");
        assert!(c.eval(&Context::new().with("app_id", "Kitty")));
        assert!(!c.eval(&Context::new().with("app_id", "firefox")));
        assert!(!c.eval(&Context::new()), "unknown never matches");
    }

    #[test]
    fn desktop_id_fallback() {
        let (c, _) = cond(r#"{ app-id = "org.mozilla.firefox" }"#);
        let ctx = Context::new()
            .with("app_id", "firefox")
            .with("desktop_id", "org.mozilla.firefox.desktop");
        assert!(c.eval(&ctx));
    }

    #[test]
    fn ops_any_not_dotted() {
        let (c, d) = cond(
            r#"{ power.on_battery = true, battery-pct = { below = 20 }, any = [{ title = { regex = "^Meet" } }, { screencast = true }], not = { locked = true } }"#,
        );
        assert!(d.ok() && d.warnings.is_empty(), "{d:?}");
        let base = Context::new()
            .with("on_battery", true)
            .with("battery_pct", 10i64);
        assert!(!c.eval(&base), "needs one any-branch");
        assert!(c.eval(&base.clone().with("title", "Meet - standup")));
        assert!(c.eval(&base.clone().with("screencast", true)));
        assert!(!c.eval(&base.clone().with("screencast", true).with("locked", true)));
        assert!(!c.eval(&base.with("battery_pct", 50i64).with("screencast", true)));
    }

    #[test]
    fn between_wraps() {
        let (c, _) = cond(r#"{ time = { between = ["22:00", "06:00"] } }"#);
        assert!(c.eval(&Context::new().with("time", "23:10")));
        assert!(c.eval(&Context::new().with("time", "05:59")));
        assert!(!c.eval(&Context::new().with("time", "12:00")));
    }

    #[test]
    fn diagnostics() {
        assert!(!cond(r#"{ title = { regex = "(" } }"#).1.ok());
        assert_eq!(cond(r#"{ nonsense = 1 }"#).1.warnings.len(), 1);
        assert!(!cond(r#"{ app-id = [{ x = 1 }] }"#).1.ok());
    }
}
