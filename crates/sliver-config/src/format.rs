//! The format engine (customization.md §3, G5/T25): waybar format strings, `states`,
//! `format-<status>-<state>` selection, `format-icons`, and `max-length`. Every module uses it,
//! built-in or `custom/*` (no special powers).
//!
//! Placeholders: `{}` (the module's main value), `{name}`, `{name:spec}`, `{:spec}`. `{{` and `}}`
//! are literal braces. A spec goes to the [`Values`] provider first (the clock uses strftime),
//! and otherwise is the standard subset `[[fill]align][0][width][.precision]`, like fmt/Rust.

use std::collections::BTreeMap;

use toml::Value;

use crate::module::Common;

/// A parsed format string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit(String),
    Field { name: String, spec: Option<String> },
}

impl Template {
    pub fn parse(s: &str) -> Result<Template, String> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    lit.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    lit.push('}');
                }
                '{' => {
                    let mut inner = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some('{') => return Err(format!("nested '{{' in {s:?}")),
                            Some(c) => inner.push(c),
                            None => return Err(format!("unclosed '{{' in {s:?}")),
                        }
                    }
                    if !lit.is_empty() {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                    }
                    let (name, spec) = match inner.split_once(':') {
                        Some((n, sp)) => (n.trim().to_owned(), Some(sp.to_owned())),
                        None => (inner.trim().to_owned(), None),
                    };
                    parts.push(Part::Field { name, spec });
                }
                '}' => {
                    return Err(format!(
                        "unmatched '}}' in {s:?} (write '}}}}' for a literal brace)"
                    ));
                }
                c => lit.push(c),
            }
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(Template { parts })
    }

    /// Placeholder names used (`""` for `{}`), e.g. to know which values a module must compute.
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.parts.iter().filter_map(|p| match p {
            Part::Field { name, .. } => Some(name.as_str()),
            Part::Lit(_) => None,
        })
    }

    /// Unknown placeholders render literally (`{nope}`), so mistakes show up on the bar.
    pub fn render(&self, values: &dyn Values) -> String {
        let mut out = String::new();
        for p in &self.parts {
            match p {
                Part::Lit(s) => out.push_str(s),
                Part::Field { name, spec } => match values.format(name, spec.as_deref()) {
                    Some(s) => out.push_str(&s),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        if let Some(sp) = spec {
                            out.push(':');
                            out.push_str(sp);
                        }
                        out.push('}');
                    }
                },
            }
        }
        out
    }
}

/// A module's current values. Implement it directly for special specs (the clock's strftime),
/// or use [`ValueMap`].
pub trait Values {
    /// Formats placeholder `name` with an optional spec. `None` means "no such value".
    fn format(&self, name: &str, spec: Option<&str>) -> Option<String>;
}

/// One plain value.
#[derive(Debug, Clone, PartialEq)]
pub enum FmtValue {
    Text(String),
    Num(f64),
}

impl From<&str> for FmtValue {
    fn from(s: &str) -> Self {
        FmtValue::Text(s.to_owned())
    }
}
impl From<String> for FmtValue {
    fn from(s: String) -> Self {
        FmtValue::Text(s)
    }
}
impl From<f64> for FmtValue {
    fn from(n: f64) -> Self {
        FmtValue::Num(n)
    }
}
impl From<i64> for FmtValue {
    fn from(n: i64) -> Self {
        FmtValue::Num(n as f64)
    }
}

/// Values in a map, formatted with the standard spec. `""` is the `{}` value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ValueMap(pub BTreeMap<String, FmtValue>);

impl ValueMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, name: &str, v: impl Into<FmtValue>) -> Self {
        self.0.insert(name.to_owned(), v.into());
        self
    }
}

impl Values for ValueMap {
    fn format(&self, name: &str, spec: Option<&str>) -> Option<String> {
        self.0
            .get(name)
            .map(|v| standard_spec(v, spec.unwrap_or("")))
    }
}

/// `[[fill]align][0][width][.precision]`: `>4`, `^9`, `*<6`, `03`, `.1`, `>6.2`.
/// An unparseable spec is ignored (the value prints plainly).
pub fn standard_spec(v: &FmtValue, spec: &str) -> String {
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let (mut fill, mut align) = (' ', None);
    let is_align = |c: char| matches!(c, '<' | '>' | '^');
    if chars.len() >= 2 && is_align(chars[1]) {
        fill = chars[0];
        align = Some(chars[1]);
        i = 2;
    } else if chars.first().copied().is_some_and(is_align) {
        align = chars.first().copied();
        i = 1;
    }
    let mut zero = false;
    if chars.get(i) == Some(&'0') {
        zero = true;
        i += 1;
    }
    let width_digits: String = chars[i..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    i += width_digits.len();
    let width: usize = width_digits.parse().unwrap_or(0);
    let precision: Option<usize> = match chars.get(i) {
        Some('.') => {
            let p: String = chars[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            i += 1 + p.len();
            p.parse().ok()
        }
        _ => None,
    };
    if i != chars.len() {
        return plain(v, None);
    }
    let body = plain(v, precision);
    let len = body.chars().count();
    if len >= width {
        return body;
    }
    let pad = width - len;
    if zero && align.is_none() && matches!(v, FmtValue::Num(_)) {
        let (sign, digits) = body
            .strip_prefix('-')
            .map(|d| ("-", d))
            .unwrap_or(("", &body));
        return format!("{sign}{}{digits}", "0".repeat(pad));
    }
    let fill_s = |n: usize| fill.to_string().repeat(n);
    // Like Rust and fmt: numbers align right and text aligns left by default.
    match align.unwrap_or(if matches!(v, FmtValue::Num(_)) {
        '>'
    } else {
        '<'
    }) {
        '<' => format!("{body}{}", fill_s(pad)),
        '^' => format!("{}{body}{}", fill_s(pad / 2), fill_s(pad - pad / 2)),
        _ => format!("{}{body}", fill_s(pad)),
    }
}

fn plain(v: &FmtValue, precision: Option<usize>) -> String {
    match (v, precision) {
        (FmtValue::Text(s), Some(p)) => s.chars().take(p).collect(),
        (FmtValue::Text(s), None) => s.clone(),
        (FmtValue::Num(n), Some(p)) => format!("{n:.p$}"),
        (FmtValue::Num(n), None) if n.fract() == 0.0 && n.abs() < 1e15 => format!("{}", *n as i64),
        (FmtValue::Num(n), None) => format!("{n}"),
    }
}

/// What the module knows right now, besides its placeholder values.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct State<'a> {
    /// 0–100. Drives `states`, array `format-icons`, and `{percentage}`.
    pub percentage: Option<f64>,
    /// Module status: `charging`, `muted`, `playing`… Selects `format-<status>` and adds a class.
    pub status: Option<&'a str>,
    /// waybar's `alt` (from JSON output or the module). Keys table `format-icons`.
    pub alt: Option<&'a str>,
    /// The user toggled `format-alt`.
    pub show_alt: bool,
    /// Battery-style states, where *lower* is worse (`warning = 30` means ≤ 30%). cpu, memory and
    /// temperature use the default, where higher is worse.
    pub lower_is_worse: bool,
}

/// A themed/SVG icon picked by `{icon}` from `format-icons = [{ icon = "…" }, …]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconRef(pub String);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Formatted {
    pub text: String,
    /// Set when `{icon}` chose an image instead of a text glyph. The renderer draws it next to `text`.
    pub icon: Option<IconRef>,
    /// The active `states` name, if any.
    pub state: Option<String>,
    /// Classes to add (state, status) for the stylesheet.
    pub classes: Vec<String>,
    /// `hide-empty-text` and nothing to show.
    pub hidden: bool,
}

/// The active state name for a percentage (waybar `getState`).
pub fn active_state(states: &[(String, f64)], pct: f64, lower_is_worse: bool) -> Option<&str> {
    // `states` is sorted ascending by threshold.
    if lower_is_worse {
        states
            .iter()
            .find(|(_, t)| pct <= *t)
            .map(|(n, _)| n.as_str())
    } else {
        states
            .iter()
            .rev()
            .find(|(_, t)| pct >= *t)
            .map(|(n, _)| n.as_str())
    }
}

/// Picks the format string, as waybar does: `format-alt` (if toggled), then
/// `format-<status>-<state>`, `format-<status>`, `format-<state>`, `format`, then the module's
/// default.
pub fn select_format<'c>(
    c: &'c Common,
    status: Option<&str>,
    state: Option<&str>,
    show_alt: bool,
    default: &'c str,
) -> &'c str {
    if show_alt {
        if let Some(f) = &c.format_alt {
            return f;
        }
    }
    let v = |k: String| c.format_variants.get(&k).map(String::as_str);
    let combo = status.zip(state).and_then(|(s, st)| v(format!("{s}-{st}")));
    combo
        .or_else(|| status.and_then(|s| v(s.to_owned())))
        .or_else(|| state.and_then(|st| v(st.to_owned())))
        .or(c.format.as_deref())
        .unwrap_or(default)
}

enum Icon<'v> {
    Text(&'v str),
    Image(&'v str),
}

fn icon_entry(v: &Value) -> Option<Icon<'_>> {
    match v {
        Value::String(s) => Some(Icon::Text(s)),
        Value::Table(t) => t.get("icon").and_then(Value::as_str).map(Icon::Image),
        _ => None,
    }
}

/// waybar `getIcon`: a string as-is, an array indexed by percentage, a table keyed by alt
/// (we also try status and state), then `default`, possibly holding a nested array.
fn pick_icon<'v>(icons: &'v Value, st: &State, state: Option<&str>) -> Option<Icon<'v>> {
    let by_pct = |a: &'v [Value]| -> Option<Icon<'v>> {
        if a.is_empty() {
            return None;
        }
        let pct = st.percentage.unwrap_or(0.0).clamp(0.0, 100.0);
        let idx = ((pct / (100.0 / a.len() as f64)) as usize).min(a.len() - 1);
        icon_entry(&a[idx])
    };
    match icons {
        Value::Array(a) => by_pct(a),
        Value::Table(t) => {
            let keys = [st.alt, st.status, state, Some("default")];
            let hit = keys.iter().flatten().find_map(|k| t.get(*k))?;
            match hit {
                Value::Array(a) => by_pct(a),
                other => icon_entry(other),
            }
        }
        other => icon_entry(other),
    }
}

/// Wraps a module's values with the engine-provided `{icon}` and `{percentage}`.
struct WithEngine<'a> {
    inner: &'a dyn Values,
    icon_text: Option<&'a str>,
    percentage: Option<f64>,
}

impl Values for WithEngine<'_> {
    fn format(&self, name: &str, spec: Option<&str>) -> Option<String> {
        if let Some(v) = self.inner.format(name, spec) {
            return Some(v);
        }
        match name {
            "icon" => Some(self.icon_text.unwrap_or("").to_owned()),
            "percentage" => self
                .percentage
                .map(|p| standard_spec(&FmtValue::Num(p.round()), spec.unwrap_or(""))),
            _ => None,
        }
    }
}

/// Formats one module: selects the format, renders it, applies max/min-length and hide-empty-text.
pub fn format_module(
    c: &Common,
    default_format: &str,
    st: &State,
    values: &dyn Values,
) -> Formatted {
    let state = st
        .percentage
        .and_then(|p| active_state(&c.states, p, st.lower_is_worse));
    let fmt = select_format(c, st.status, state, st.show_alt, default_format);
    let (icon_text, icon_image) = match c
        .format_icons
        .as_ref()
        .and_then(|i| pick_icon(i, st, state))
    {
        Some(Icon::Text(t)) => (Some(t), None),
        Some(Icon::Image(i)) => (None, Some(IconRef(i.to_owned()))),
        None => (None, None),
    };
    let text = match Template::parse(fmt) {
        Ok(t) => t.render(&WithEngine {
            inner: values,
            icon_text,
            percentage: st.percentage,
        }),
        Err(e) => format!("⚠ {e}"),
    };
    let text = text.trim().to_owned();
    let text = apply_lengths(text, c.min_length, c.max_length);
    let mut classes = Vec::new();
    classes.extend(state.map(str::to_owned));
    classes.extend(st.status.map(str::to_owned));
    let hidden = c.hide_empty_text && text.is_empty() && icon_image.is_none();
    Formatted {
        text,
        icon: icon_image,
        state: state.map(str::to_owned),
        classes,
        hidden,
    }
}

fn apply_lengths(text: String, min: Option<u32>, max: Option<u32>) -> String {
    let n = text.chars().count();
    match (min, max) {
        (_, Some(max)) if n > max as usize => {
            let keep = (max as usize).saturating_sub(1);
            let mut s: String = text.chars().take(keep).collect();
            s.push('…');
            s
        }
        (Some(min), _) if n < min as usize => format!("{text}{}", " ".repeat(min as usize - n)),
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Diagnostics;

    fn common(src: &str) -> Common {
        let t: toml::Table = src.parse().unwrap();
        let mut d = Diagnostics::default();
        let (c, _) = Common::parse(&t, "m", &mut d);
        assert!(d.ok(), "{d:?}");
        c
    }

    #[test]
    fn templates() {
        let v = ValueMap::new()
            .with("", "main")
            .with("capacity", 42i64)
            .with("load", 0.4567)
            .with("title", "Hello");
        let r = |s: &str| Template::parse(s).unwrap().render(&v);
        assert_eq!(r("{} | {capacity}%"), "main | 42%");
        assert_eq!(
            r("{capacity:>4}|{capacity:03}|{load:.2}|{title:^9}|{title:.3}"),
            "  42|042|0.46|  Hello  |Hel"
        );
        assert_eq!(r("{title:*<7}"), "Hello**");
        assert_eq!(r("{{literal}} {nope}"), "{literal} {nope}");
        assert!(Template::parse("{unclosed").is_err());
        assert!(Template::parse("a } b").is_err());
        assert_eq!(
            Template::parse("{icon} {capacity}%")
                .unwrap()
                .fields()
                .collect::<Vec<_>>(),
            ["icon", "capacity"]
        );
    }

    struct Clock;
    impl Values for Clock {
        fn format(&self, name: &str, spec: Option<&str>) -> Option<String> {
            (name.is_empty()).then(|| format!("strftime({})", spec.unwrap_or("%H:%M")))
        }
    }

    #[test]
    fn provider_specs() {
        assert_eq!(
            Template::parse("{:%a %d}").unwrap().render(&Clock),
            "strftime(%a %d)"
        );
    }

    #[test]
    fn battery_like_waybar() {
        let c = common(
            r#"
            format = "{icon} {capacity}%"
            format-charging = "C {capacity}%"
            format-critical = "LOW {capacity}%"
            format-discharging-critical = "!! {capacity}%"
            format-icons = ["0", "1", "2", "3", "4"]
            states = { warning = 30, critical = 15 }
            "#,
        );
        let f = |pct: f64, status: Option<&str>| {
            let st = State {
                percentage: Some(pct),
                status,
                lower_is_worse: true,
                ..State::default()
            };
            format_module(&c, "{}", &st, &ValueMap::new().with("capacity", pct))
        };
        assert_eq!(f(95.0, None).text, "4 95%");
        assert_eq!(f(50.0, None).text, "2 50%");
        assert_eq!(
            f(100.0, None).text,
            "4 100%",
            "100% clamps to the last icon"
        );
        let warn = f(25.0, None);
        assert_eq!(
            (warn.text.as_str(), warn.state.as_deref()),
            ("1 25%", Some("warning"))
        );
        assert_eq!(warn.classes, ["warning"]);
        assert_eq!(f(10.0, None).text, "LOW 10%");
        assert_eq!(
            f(10.0, Some("discharging")).text,
            "!! 10%",
            "status-state combo wins"
        );
        assert_eq!(f(60.0, Some("charging")).text, "C 60%");
        assert_eq!(f(60.0, Some("charging")).classes, ["charging"]);
    }

    #[test]
    fn higher_is_worse_states() {
        let c = common("format = \"{percentage}%\"\nstates = { warning = 70, critical = 90 }");
        let st = |p| State {
            percentage: Some(p),
            ..State::default()
        };
        assert_eq!(
            format_module(&c, "", &st(95.0), &ValueMap::new())
                .state
                .as_deref(),
            Some("critical")
        );
        assert_eq!(
            format_module(&c, "", &st(75.0), &ValueMap::new())
                .state
                .as_deref(),
            Some("warning")
        );
        assert_eq!(
            format_module(&c, "", &st(10.0), &ValueMap::new()).state,
            None
        );
        assert_eq!(
            format_module(&c, "", &st(10.4), &ValueMap::new()).text,
            "10%"
        );
    }

    #[test]
    fn icon_tables_images_alt_and_lengths() {
        let c = common(
            r#"
            format = "{icon} {text}"
            format-alt = "ALT"
            format-icons = { connected = { icon = "network-vpn-symbolic" }, default = ["a", "b"] }
            max-length = 8
            hide-empty-text = true
            "#,
        );
        let v = ValueMap::new().with("text", "a long title");
        let on = format_module(
            &c,
            "{}",
            &State {
                alt: Some("connected"),
                ..State::default()
            },
            &v,
        );
        assert_eq!(on.icon, Some(IconRef("network-vpn-symbolic".into())));
        assert_eq!(
            on.text, "a long …",
            "{{icon}} renders empty when it's an image; then max-length"
        );
        let off = format_module(
            &c,
            "{}",
            &State {
                percentage: Some(80.0),
                ..State::default()
            },
            &v,
        );
        assert_eq!((off.icon, off.text.as_str()), (None, "b a lon…"));
        let alt = format_module(
            &c,
            "{}",
            &State {
                show_alt: true,
                ..State::default()
            },
            &v,
        );
        assert_eq!(alt.text, "ALT");
        let empty = format_module(
            &c,
            "{}",
            &State::default(),
            &ValueMap::new().with("text", ""),
        );
        assert!(!empty.hidden, "the default icon glyph is still text");
        let bare = common("format = \"{text}\"\nhide-empty-text = true");
        assert!(
            format_module(
                &bare,
                "{}",
                &State::default(),
                &ValueMap::new().with("text", "")
            )
            .hidden
        );
    }
}
