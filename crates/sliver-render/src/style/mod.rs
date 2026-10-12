//! Waybar-style stylesheets for the bar (ADR-0004, reference in docs/styling.md).
//!
//! The style tree is `bar > zone > module`. Selectors match by tag, `#id`, `.class`, `:active`
//! (pressed), `:first-child`/`:last-child` and `:not()`, with descendant and child combinators.
//! Values support CSS colors plus the GTK-isms waybar users paste: `@define-color`, `@name`,
//! `alpha()`, `shade()`, `mix()`, `lighter()`, `darker()`. `@import "file.css"` and
//! `@import "sliver:NAME"` (built-in themes) compose sheets.
//!
//! Loading never fails on content: problems become [`Diagnostic`]s with file:line:column and the
//! offending rule or declaration is skipped, so a typo can't blank the bar.

mod parse;
mod selector;
mod value;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Color;
use crate::text::EMBEDDED_FONT_FAMILY;
pub use selector::Element;
use selector::Selector;
use value::Length;

/// A problem found while loading a stylesheet. Never fatal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// File path, or `sliver:NAME` for built-in sheets.
    pub source: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.source, self.line, self.column, self.message
        )
    }
}

/// `@accent` until the agent defines the desktop's (GNOME's default blue).
pub const DEFAULT_ACCENT: Color = Color::rgb(0x35, 0x84, 0xe4);

/// Built-in themes, importable as `@import "sliver:NAME";`.
pub const BUILTIN_THEMES: &[(&str, &str)] = &[
    ("classic", include_str!("../../assets/themes/classic.css")),
    ("sliver", include_str!("../../assets/themes/sliver.css")),
];

/// The computed style of one element.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    // Inherited.
    pub color: Color,
    pub font_family: Vec<String>,
    pub font_size: f32,
    pub font_weight: u16,
    /// `-sliver-icon-size` (alias `-gtk-icon-size`).
    pub icon_size: f32,
    // Not inherited.
    pub background: Color,
    /// `None` means `currentColor`.
    pub border_color: Option<Color>,
    pub border_width: f32,
    pub radius: f32,
    /// Top, right, bottom, left.
    pub padding: [f32; 4],
    pub margin: [f32; 4],
    pub opacity: f32,
    pub min_width: f32,
    /// Slider troughs: thickness. Slider knobs: height.
    pub min_height: f32,
    /// Space between children: zones in the bar, modules in a zone, icon and label in a module.
    pub gap: f32,
}

impl Style {
    /// Initial values, for the root (`bar`).
    pub fn initial() -> Style {
        Style {
            color: Color::WHITE,
            font_family: vec![EMBEDDED_FONT_FAMILY.to_string()],
            font_size: 24.0,
            font_weight: 400,
            icon_size: 32.0,
            background: Color::TRANSPARENT,
            border_color: None,
            border_width: 0.0,
            radius: 0.0,
            padding: [0.0; 4],
            margin: [0.0; 4],
            opacity: 1.0,
            min_width: 0.0,
            min_height: 0.0,
            gap: 0.0,
        }
    }

    pub fn border_color(&self) -> Color {
        self.border_color.unwrap_or(self.color)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Prop {
    Color,
    FontFamily,
    FontSize,
    FontWeight,
    IconSize,
    Background,
    BorderColor,
    BorderWidth,
    Radius,
    Padding(usize),
    Margin(usize),
    Opacity,
    MinWidth,
    MinHeight,
    Gap,
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Color(Color),
    CurrentColor,
    Len(Length),
    Family(Vec<String>),
    Weight(u16),
    Number(f32),
    Inherit,
    Initial,
}

#[derive(Clone, Debug)]
struct Decl {
    prop: Prop,
    value: Value,
    important: bool,
}

#[derive(Clone, Debug)]
struct Rule {
    selector: Selector,
    decls: std::sync::Arc<[Decl]>,
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// A parsed stylesheet (possibly several, via imports), ready to compute styles.
#[derive(Clone, Debug)]
pub struct Stylesheet {
    rules: Vec<Rule>,
    colors: HashMap<String, Color>,
    files: Vec<PathBuf>,
    generation: u64,
}

impl Default for Stylesheet {
    /// Empty: no rules, so everything gets [`Style::initial`] values.
    fn default() -> Stylesheet {
        Stylesheet::empty()
    }
}

const MAX_IMPORT_DEPTH: u32 = 8;

impl Stylesheet {
    /// An empty sheet. Build one up with [`Self::define_color`], [`Self::add_str`] and
    /// [`Self::add_file`], e.g. to predefine `@accent` from the desktop before the user's sheet.
    pub fn new() -> Stylesheet {
        Stylesheet::empty()
    }

    /// Predefines `@name`, as if by `@define-color` (later definitions in sheets win).
    pub fn define_color(&mut self, name: &str, color: Color) {
        self.colors.insert(name.to_string(), color);
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    /// Appends rules from stylesheet text. `base_dir` resolves relative `@import`s.
    pub fn add_str(
        &mut self,
        name: &str,
        text: &str,
        base_dir: Option<&Path>,
        diags: &mut Vec<Diagnostic>,
    ) {
        self.add_text(name, text, base_dir, 0, diags);
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    /// Appends rules from a file and its imports. Fails only if `path` itself can't be read.
    pub fn add_file(&mut self, path: &Path, diags: &mut Vec<Diagnostic>) -> std::io::Result<()> {
        let text = std::fs::read_to_string(path)?;
        self.files.push(path.to_path_buf());
        self.add_str(&path.display().to_string(), &text, path.parent(), diags);
        Ok(())
    }

    fn empty() -> Stylesheet {
        Stylesheet {
            rules: Vec::new(),
            // The agent overrides `@accent` with the desktop's accent color.
            colors: HashMap::from([("accent".to_string(), DEFAULT_ACCENT)]),
            files: Vec::new(),
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The built-in tiny-dfr look. Always valid. The safe layer uses it.
    pub fn classic() -> Stylesheet {
        Stylesheet::builtin("classic").expect("classic theme exists")
    }

    /// The default theme: macOS Touch Bar look with the `@accent` color (H10).
    pub fn sliver() -> Stylesheet {
        Stylesheet::builtin("sliver").expect("sliver theme exists")
    }

    /// A built-in theme by name (see [`BUILTIN_THEMES`]).
    pub fn builtin(name: &str) -> Option<Stylesheet> {
        let (_, text) = BUILTIN_THEMES.iter().find(|(n, _)| *n == name)?;
        let (sheet, diags) = Stylesheet::parse(&format!("sliver:{name}"), text, None);
        debug_assert!(diags.is_empty(), "built-in theme {name}: {diags:?}");
        Some(sheet)
    }

    /// Parses stylesheet text. `base_dir` resolves relative `@import`s (none if `None`).
    pub fn parse(name: &str, text: &str, base_dir: Option<&Path>) -> (Stylesheet, Vec<Diagnostic>) {
        let mut sheet = Stylesheet::empty();
        let mut diags = Vec::new();
        sheet.add_text(name, text, base_dir, 0, &mut diags);
        (sheet, diags)
    }

    /// Loads a stylesheet file and its imports. Fails only if `path` itself can't be read.
    pub fn load(path: &Path) -> std::io::Result<(Stylesheet, Vec<Diagnostic>)> {
        let text = std::fs::read_to_string(path)?;
        let mut sheet = Stylesheet::empty();
        let mut diags = Vec::new();
        sheet.files.push(path.to_path_buf());
        sheet.add_text(
            &path.display().to_string(),
            &text,
            path.parent(),
            0,
            &mut diags,
        );
        Ok((sheet, diags))
    }

    /// Files this sheet was loaded from, imports included. Watch these for hot reload.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Changes whenever the sheet does, so renderers can cache computed styles.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Appends `other`'s rules and colors after this sheet's (so `other` wins ties).
    pub fn extend(&mut self, other: &Stylesheet) {
        self.rules.extend(other.rules.iter().cloned());
        self.colors
            .extend(other.colors.iter().map(|(k, v)| (k.clone(), *v)));
        self.files.extend(other.files.iter().cloned());
        self.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    fn add_text(
        &mut self,
        name: &str,
        text: &str,
        base_dir: Option<&Path>,
        depth: u32,
        diags: &mut Vec<Diagnostic>,
    ) {
        let src = parse::Source::new(name, text);
        let clean = parse::clean(text);
        for item in parse::parse_sheet(&src, &clean, diags) {
            match item {
                parse::Item::AtStatement {
                    name: "define-color",
                    prelude,
                    pos,
                } => {
                    let (cname, val) = prelude
                        .split_once(char::is_whitespace)
                        .unwrap_or((prelude, ""));
                    match value::color(val, &self.colors) {
                        Ok(c) => {
                            self.colors.insert(cname.to_string(), c);
                        }
                        Err(e) => diags.push(src.diag(pos, format!("@define-color {cname}: {e}"))),
                    }
                }
                parse::Item::AtStatement {
                    name: "import",
                    prelude,
                    pos,
                } => self.import(&src, pos, prelude, base_dir, depth, diags),
                parse::Item::AtStatement { name, pos, .. } | parse::Item::AtBlock { name, pos } => {
                    diags.push(src.diag(pos, format!("'@{name}' is not supported (ignored)")));
                }
                parse::Item::Rule {
                    selectors,
                    selectors_pos,
                    decls,
                } => {
                    let decls: std::sync::Arc<[Decl]> = decls
                        .iter()
                        .flat_map(|d| match self.parse_decl(d.name, d.value) {
                            Ok(v) => v
                                .into_iter()
                                .map(|(prop, value)| Decl {
                                    prop,
                                    value,
                                    important: d.important,
                                })
                                .collect(),
                            Err(e) => {
                                diags.push(src.diag(d.pos, e));
                                Vec::new()
                            }
                        })
                        .collect();
                    for sel in value::commas(selectors) {
                        let mut notes = Vec::new();
                        match Selector::parse(sel, &mut notes) {
                            Ok(selector) => self.rules.push(Rule {
                                selector,
                                decls: decls.clone(),
                            }),
                            Err(e) => diags
                                .push(src.diag(selectors_pos, format!("selector '{sel}': {e}"))),
                        }
                        diags.extend(notes.into_iter().map(|n| src.diag(selectors_pos, n)));
                    }
                }
            }
        }
    }

    fn import(
        &mut self,
        src: &parse::Source<'_>,
        pos: usize,
        prelude: &str,
        base_dir: Option<&Path>,
        depth: u32,
        diags: &mut Vec<Diagnostic>,
    ) {
        let target = prelude
            .strip_prefix("url(")
            .and_then(|p| p.strip_suffix(')'))
            .unwrap_or(prelude)
            .trim()
            .trim_matches(|c| c == '"' || c == '\'');
        if depth >= MAX_IMPORT_DEPTH {
            diags.push(src.diag(pos, format!("@import '{target}': imports nest too deeply")));
            return;
        }
        if let Some(theme) = target.strip_prefix("sliver:") {
            match BUILTIN_THEMES.iter().find(|(n, _)| *n == theme) {
                Some((n, text)) => {
                    self.add_text(&format!("sliver:{n}"), text, None, depth + 1, diags)
                }
                None => {
                    let names: Vec<&str> = BUILTIN_THEMES.iter().map(|t| t.0).collect();
                    diags.push(src.diag(
                        pos,
                        format!("no built-in theme '{theme}' (have: {})", names.join(", ")),
                    ));
                }
            }
            return;
        }
        let path = match base_dir {
            Some(dir) => dir.join(target),
            None => PathBuf::from(target),
        };
        if base_dir.is_none() && path.is_relative() {
            diags.push(src.diag(
                pos,
                format!("@import '{target}': relative import without a base directory"),
            ));
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.files.push(path.clone());
                self.add_text(
                    &path.display().to_string(),
                    &text,
                    path.parent(),
                    depth + 1,
                    diags,
                );
            }
            Err(e) => diags.push(src.diag(pos, format!("@import '{}': {e}", path.display()))),
        }
    }

    /// Parses one declaration into one or more longhand properties.
    fn parse_decl(&self, name: &str, raw: &str) -> Result<Vec<(Prop, Value)>, String> {
        let name = name.to_ascii_lowercase();
        let ctx = |e: String| format!("{name}: {e}");
        let kw = raw.trim().to_ascii_lowercase();
        let global = match kw.as_str() {
            "inherit" => Some(Value::Inherit),
            "initial" | "unset" => Some(Value::Initial),
            _ => None,
        };
        let props: Vec<Prop> = match name.as_str() {
            "color" => vec![Prop::Color],
            "font-family" => vec![Prop::FontFamily],
            "font-size" => vec![Prop::FontSize],
            "font-weight" => vec![Prop::FontWeight],
            "-sliver-icon-size" | "-gtk-icon-size" => vec![Prop::IconSize],
            "background-color" | "background" => vec![Prop::Background],
            "border-color" => vec![Prop::BorderColor],
            "border-width" => vec![Prop::BorderWidth],
            "border-radius" => vec![Prop::Radius],
            "border" | "border-style" => vec![Prop::BorderWidth, Prop::BorderColor],
            "padding" => (0..4).map(Prop::Padding).collect(),
            "margin" => (0..4).map(Prop::Margin).collect(),
            "opacity" => vec![Prop::Opacity],
            "min-width" => vec![Prop::MinWidth],
            "min-height" => vec![Prop::MinHeight],
            "gap" | "column-gap" => vec![Prop::Gap],
            // Parsed for forward compatibility (customization.md §7); animations aren't drawn yet.
            "transition" => return Ok(Vec::new()),
            _ => {
                let side = ["top", "right", "bottom", "left"];
                let sided = |prefix: &str| {
                    name.strip_prefix(prefix)
                        .and_then(|s| side.iter().position(|x| *x == s))
                };
                if let Some(i) = sided("padding-") {
                    vec![Prop::Padding(i)]
                } else if let Some(i) = sided("margin-") {
                    vec![Prop::Margin(i)]
                } else {
                    return Err(format!("unsupported property '{name}' (ignored)"));
                }
            }
        };
        if let Some(g) = global {
            return Ok(props.into_iter().map(|p| (p, g.clone())).collect());
        }
        let len = |s: &str| {
            value::length(s)
                .and_then(value::non_negative)
                .map(Value::Len)
        };
        let color = |s: &str| {
            if s.trim().eq_ignore_ascii_case("currentcolor") {
                Ok(Value::CurrentColor)
            } else {
                value::color(s, &self.colors).map(Value::Color)
            }
        };
        let one = |p: Prop, v: Result<Value, String>| v.map(|v| vec![(p, v)]).map_err(ctx);
        match name.as_str() {
            "color" | "background-color" | "border-color" => one(props[0], color(raw)),
            "background" => match color(raw) {
                Ok(v) => Ok(vec![(Prop::Background, v)]),
                Err(_) => Err(ctx(format!("only colors are supported, got '{raw}'"))),
            },
            "font-family" => one(Prop::FontFamily, value::font_family(raw).map(Value::Family)),
            "font-weight" => one(Prop::FontWeight, value::font_weight(raw).map(Value::Weight)),
            "opacity" => one(
                Prop::Opacity,
                value::number_or_percent(raw).map(|v| Value::Number(v.clamp(0.0, 1.0))),
            ),
            "border-style" => match kw.as_str() {
                "none" | "hidden" => Ok(vec![(Prop::BorderWidth, Value::Len(Length::Px(0.0)))]),
                "solid" => Ok(Vec::new()),
                _ => Err(ctx(format!(
                    "only 'solid' and 'none' are supported, got '{raw}'"
                ))),
            },
            "border" => {
                let mut out = Vec::new();
                for t in value::tokens(raw) {
                    let tl = t.to_ascii_lowercase();
                    if tl == "none" || tl == "hidden" {
                        out.push((Prop::BorderWidth, Value::Len(Length::Px(0.0))));
                    } else if matches!(tl.as_str(), "solid" | "dashed" | "dotted" | "double") {
                    } else if let Ok(l) = len(t) {
                        out.push((Prop::BorderWidth, l));
                    } else {
                        out.push((Prop::BorderColor, color(t).map_err(ctx)?));
                    }
                }
                Ok(out)
            }
            "border-radius" => {
                let first = value::tokens(raw).first().copied().unwrap_or("");
                one(Prop::Radius, len(first))
            }
            "padding" | "margin" => {
                let s = value::sides(raw).map_err(ctx)?;
                if name == "padding" && s.iter().any(|l| l.px(1.0) < 0.0) {
                    return Err(ctx("must not be negative".into()));
                }
                Ok(props
                    .into_iter()
                    .zip(s)
                    .map(|(p, l)| (p, Value::Len(l)))
                    .collect())
            }
            n if n.starts_with("margin-") => one(props[0], value::length(raw).map(Value::Len)),
            _ => one(props[0], len(raw)),
        }
    }

    /// Computes the style of the last element of `path`, given its parent's computed style.
    pub fn compute(&self, path: &[Element<'_>], parent: Option<&Style>) -> Style {
        // Cascade order: !important, then specificity, then source order. Later wins.
        type Precedence = (bool, (u16, u16, u16), usize);
        let mut matched: Vec<(Precedence, &Decl)> = Vec::new();
        for (order, rule) in self.rules.iter().enumerate() {
            if rule.selector.matches(path) {
                for d in rule.decls.iter() {
                    matched.push(((d.important, rule.selector.specificity, order), d));
                }
            }
        }
        matched.sort_by_key(|m| m.0);
        let mut specified: HashMap<Prop, &Value> = HashMap::new();
        for (_, d) in matched {
            specified.insert(d.prop, &d.value);
        }

        let initial = Style::initial();
        let parent = parent.unwrap_or(&initial);
        let mut s = Style::initial();
        // Inherited properties start from the parent.
        s.color = parent.color;
        s.font_family = parent.font_family.clone();
        s.font_size = parent.font_size;
        s.font_weight = parent.font_weight;
        s.icon_size = parent.icon_size;

        let get = |p: Prop| -> Option<&Value> {
            match specified.get(&p).copied() {
                Some(Value::Inherit) => None,
                v => v,
            }
        };
        // font-size first: em lengths depend on it.
        match get(Prop::FontSize) {
            Some(Value::Len(l)) => s.font_size = l.px(parent.font_size),
            Some(Value::Initial) => s.font_size = initial.font_size,
            _ => {}
        }
        let fs = s.font_size;
        // A length property: specified value, `initial`, or `fallback` (inherited or initial).
        let len = |v: Option<&Value>, init: f32, fallback: f32| match v {
            Some(Value::Len(l)) => l.px(fs),
            Some(Value::Initial) => init,
            _ => fallback,
        };
        let inherit_if = |p: Prop, own: f32, from_parent: f32| {
            if specified.get(&p) == Some(&&Value::Inherit) {
                from_parent
            } else {
                own
            }
        };
        match get(Prop::Color) {
            Some(Value::Color(c)) => s.color = *c,
            Some(Value::Initial) => s.color = initial.color,
            _ => {}
        }
        match get(Prop::FontFamily) {
            Some(Value::Family(f)) => s.font_family = f.clone(),
            Some(Value::Initial) => s.font_family = initial.font_family.clone(),
            _ => {}
        }
        match get(Prop::FontWeight) {
            Some(Value::Weight(w)) => s.font_weight = *w,
            Some(Value::Initial) => s.font_weight = initial.font_weight,
            _ => {}
        }
        s.icon_size = len(get(Prop::IconSize), initial.icon_size, s.icon_size);

        s.background = match get(Prop::Background) {
            Some(Value::Color(c)) => *c,
            Some(Value::CurrentColor) => s.color,
            _ if specified.get(&Prop::Background) == Some(&&Value::Inherit) => parent.background,
            _ => initial.background,
        };
        s.border_color = match get(Prop::BorderColor) {
            Some(Value::Color(c)) => Some(*c),
            _ if specified.get(&Prop::BorderColor) == Some(&&Value::Inherit) => parent.border_color,
            _ => None,
        };
        s.border_width = inherit_if(
            Prop::BorderWidth,
            len(get(Prop::BorderWidth), 0.0, 0.0),
            parent.border_width,
        );
        s.radius = inherit_if(
            Prop::Radius,
            len(get(Prop::Radius), 0.0, 0.0),
            parent.radius,
        );
        for i in 0..4 {
            s.padding[i] = inherit_if(
                Prop::Padding(i),
                len(get(Prop::Padding(i)), 0.0, 0.0),
                parent.padding[i],
            );
            s.margin[i] = inherit_if(
                Prop::Margin(i),
                len(get(Prop::Margin(i)), 0.0, 0.0),
                parent.margin[i],
            );
        }
        s.opacity = match get(Prop::Opacity) {
            Some(Value::Number(o)) => *o,
            _ => inherit_if(Prop::Opacity, 1.0, parent.opacity),
        };
        s.min_width = inherit_if(
            Prop::MinWidth,
            len(get(Prop::MinWidth), 0.0, 0.0),
            parent.min_width,
        );
        s.min_height = inherit_if(
            Prop::MinHeight,
            len(get(Prop::MinHeight), 0.0, 0.0),
            parent.min_height,
        );
        s.gap = inherit_if(Prop::Gap, len(get(Prop::Gap), 0.0, 0.0), parent.gap);
        s
    }
}

#[cfg(test)]
mod tests;
