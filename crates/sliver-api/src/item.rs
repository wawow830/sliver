//! Published items: the `aa{sv}` vocabulary of `docs/api.md` §3, parsed into typed values.
//!
//! Parsing is lenient (the forward compatibility rule): unknown keys and types become warnings and
//! wrong-typed values are ignored with a warning. Only structural problems are errors: a missing
//! or duplicate `id`, a missing `type`, nesting deeper than one level, and size limits.

use sliver_ipc::dbus::limits;
use std::collections::{BTreeMap, HashSet};

/// A D-Bus value, decoupled from zvariant so the model is testable without a bus.
#[derive(Debug, Clone, PartialEq)]
pub enum Prop {
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Prop>),
    Map(BTreeMap<String, Prop>),
}

/// One item's raw properties (`a{sv}`).
pub type Props = BTreeMap<String, Prop>;

impl Prop {
    fn kind(&self) -> &'static str {
        match self {
            Prop::Bool(_) => "bool",
            Prop::Int(_) => "int",
            Prop::Num(_) => "number",
            Prop::Str(_) => "string",
            Prop::Bytes(_) => "bytes",
            Prop::List(_) => "list",
            Prop::Map(_) => "dict",
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Prop::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn as_f64(&self) -> Option<f64> {
        match self {
            Prop::Num(n) if n.is_finite() => Some(*n),
            Prop::Int(i) => Some(*i as f64),
            _ => None,
        }
    }
    fn as_i64(&self) -> Option<i64> {
        match self {
            Prop::Int(i) => Some(*i),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_strs(&self) -> Option<Vec<String>> {
        match self {
            Prop::List(l) => l.iter().map(|p| p.as_str().map(str::to_owned)).collect(),
            _ => None,
        }
    }
}

/// A scalar usable in user `format` strings.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    Button,
    Toggle {
        state: bool,
    },
    Label,
    Slider {
        value: f64,
        min: f64,
        max: f64,
        step: f64,
    },
    Segmented {
        segments: Vec<Segment>,
        selected: i32,
    },
    Progress {
        value: f64,
        indeterminate: bool,
    },
    Spacer {
        size: SpacerSize,
    },
    Group {
        items: Vec<Item>,
    },
    Popover {
        items: Vec<Item>,
    },
}

impl ItemKind {
    pub fn type_name(&self) -> &'static str {
        match self {
            ItemKind::Button => "button",
            ItemKind::Toggle { .. } => "toggle",
            ItemKind::Label => "label",
            ItemKind::Slider { .. } => "slider",
            ItemKind::Segmented { .. } => "segmented",
            ItemKind::Progress { .. } => "progress",
            ItemKind::Spacer { .. } => "spacer",
            ItemKind::Group { .. } => "group",
            ItemKind::Popover { .. } => "popover",
        }
    }
    fn children(&self) -> Option<&Vec<Item>> {
        match self {
            ItemKind::Group { items } | ItemKind::Popover { items } => Some(items),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Segment {
    pub label: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpacerSize {
    Small,
    Large,
    Flexible,
}

/// Gestures an item can opt into (`tap` is always on for interactive items).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Gesture {
    Tap,
    Hold,
    SwipeLeft,
    SwipeRight,
    DoubleTap,
    Drag,
}

impl Gesture {
    pub fn name(self) -> &'static str {
        match self {
            Gesture::Tap => "tap",
            Gesture::Hold => "hold",
            Gesture::SwipeLeft => "swipe-left",
            Gesture::SwipeRight => "swipe-right",
            Gesture::DoubleTap => "double-tap",
            Gesture::Drag => "drag",
        }
    }
    pub fn parse(s: &str) -> Option<Gesture> {
        Some(match s {
            "tap" => Gesture::Tap,
            "hold" => Gesture::Hold,
            "swipe-left" => Gesture::SwipeLeft,
            "swipe-right" => Gesture::SwipeRight,
            "double-tap" => Gesture::DoubleTap,
            "drag" => Gesture::Drag,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub label: Option<String>,
    pub icon: Option<String>,
    pub icon_data: Option<Vec<u8>>,
    pub enabled: bool,
    pub visible: bool,
    pub classes: Vec<String>,
    pub values: BTreeMap<String, Scalar>,
    pub fg: Option<String>,
    pub bg: Option<String>,
    pub min_width: Option<u32>,
    pub max_width: Option<u32>,
    pub priority: i32,
    pub gestures: Vec<Gesture>,
    pub a11y_label: Option<String>,
    pub customizable: bool,
}

impl Item {
    fn new(id: String, kind: ItemKind) -> Item {
        Item {
            id,
            kind,
            label: None,
            icon: None,
            icon_data: None,
            enabled: true,
            visible: true,
            classes: Vec::new(),
            values: BTreeMap::new(),
            fg: None,
            bg: None,
            min_width: None,
            max_width: None,
            priority: 0,
            gestures: Vec::new(),
            a11y_label: None,
            customizable: true,
        }
    }

    /// This item and its children, depth first.
    pub fn walk(&self) -> Vec<&Item> {
        let mut v = vec![self];
        if let Some(c) = self.kind.children() {
            v.extend(c.iter());
        }
        v
    }

    pub fn icon_bytes(&self) -> usize {
        self.walk()
            .iter()
            .map(|i| i.icon_data.as_ref().map_or(0, Vec::len))
            .sum()
    }

    pub fn wants(&self, g: Gesture) -> bool {
        g == Gesture::Tap || self.gestures.contains(&g)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Maps to `NS.Error.InvalidArgs`.
    Invalid(String),
    /// Maps to `NS.Error.LimitExceeded`.
    Limit(String),
}

pub type Warnings = Vec<String>;

/// Parse a full `SetItems` list.
pub fn parse_items(list: &[Props], w: &mut Warnings) -> Result<Vec<Item>, ParseError> {
    let items = parse_list(list, 0, w)?;
    check_tree(&items)?;
    Ok(items)
}

fn parse_list(list: &[Props], depth: u8, w: &mut Warnings) -> Result<Vec<Item>, ParseError> {
    let mut out = Vec::with_capacity(list.len());
    for p in list {
        if let Some(item) = parse_item(p, depth, w)? {
            out.push(item);
        }
    }
    Ok(out)
}

/// Total count, unique ids and icon size over the whole tree.
pub fn check_tree(items: &[Item]) -> Result<(), ParseError> {
    let all: Vec<&Item> = items.iter().flat_map(Item::walk).collect();
    if all.len() > limits::ITEMS_PER_CONTEXT {
        return Err(ParseError::Limit(format!(
            "{} items > {}",
            all.len(),
            limits::ITEMS_PER_CONTEXT
        )));
    }
    let mut ids = HashSet::new();
    for i in &all {
        if !ids.insert(i.id.as_str()) {
            return Err(ParseError::Invalid(format!("duplicate item id {:?}", i.id)));
        }
    }
    let icons: usize = items.iter().map(Item::icon_bytes).sum();
    if icons > limits::ICON_BYTES_PER_CONTEXT {
        return Err(ParseError::Limit(format!(
            "{icons} bytes of icon data > {}",
            limits::ICON_BYTES_PER_CONTEXT
        )));
    }
    Ok(())
}

const COMMON_KEYS: &[&str] = &[
    "id",
    "type",
    "label",
    "icon",
    "icon-data",
    "enabled",
    "visible",
    "classes",
    "values",
    "fg",
    "bg",
    "width",
    "priority",
    "gestures",
    "a11y-label",
    "customizable",
];

fn type_keys(t: &str) -> &'static [&'static str] {
    match t {
        "toggle" => &["state"],
        "slider" => &["value", "min", "max", "step"],
        "segmented" => &["segments", "selected"],
        "progress" => &["value", "indeterminate"],
        "spacer" => &["size"],
        "group" | "popover" => &["items"],
        _ => &[],
    }
}

fn parse_item(p: &Props, depth: u8, w: &mut Warnings) -> Result<Option<Item>, ParseError> {
    let id = match p.get("id") {
        Some(Prop::Str(s)) if !s.is_empty() && s.len() <= 128 => s.clone(),
        _ => {
            return Err(ParseError::Invalid(
                "every item needs a string \"id\" (1-128 bytes)".into(),
            ));
        }
    };
    let ty = match p.get("type") {
        Some(Prop::Str(s)) => s.clone(),
        _ => {
            return Err(ParseError::Invalid(format!(
                "item {id:?} needs a string \"type\""
            )));
        }
    };
    let kind = match ty.as_str() {
        "button" => ItemKind::Button,
        "toggle" => ItemKind::Toggle { state: false },
        "label" => ItemKind::Label,
        "slider" => ItemKind::Slider {
            value: 0.0,
            min: 0.0,
            max: 1.0,
            step: 0.0,
        },
        "segmented" => ItemKind::Segmented {
            segments: Vec::new(),
            selected: -1,
        },
        "progress" => ItemKind::Progress {
            value: 0.0,
            indeterminate: false,
        },
        "spacer" => ItemKind::Spacer {
            size: SpacerSize::Small,
        },
        "group" | "popover" => {
            if depth > 0 {
                return Err(ParseError::Invalid(format!(
                    "item {id:?}: {ty} can't be nested inside a group or popover"
                )));
            }
            let children = match p.get("items") {
                Some(Prop::List(l)) => {
                    let maps: Option<Vec<Props>> = l
                        .iter()
                        .map(|x| match x {
                            Prop::Map(m) => Some(m.clone()),
                            _ => None,
                        })
                        .collect();
                    let maps = maps.ok_or_else(|| {
                        ParseError::Invalid(format!("item {id:?}: \"items\" must be aa{{sv}}"))
                    })?;
                    parse_list(&maps, depth + 1, w)?
                }
                None => Vec::new(),
                Some(_) => {
                    return Err(ParseError::Invalid(format!(
                        "item {id:?}: \"items\" must be aa{{sv}}"
                    )));
                }
            };
            if ty == "group" {
                ItemKind::Group { items: children }
            } else {
                ItemKind::Popover { items: children }
            }
        }
        other => {
            w.push(format!("item {id:?}: unknown type {other:?}, skipped"));
            return Ok(None);
        }
    };
    let mut item = Item::new(id, kind);
    for (k, v) in p {
        if COMMON_KEYS.contains(&k.as_str()) || type_keys(&ty).contains(&k.as_str()) {
            if k != "id" && k != "type" && k != "items" {
                apply_prop(&mut item, k, v, w);
            }
        } else {
            w.push(format!("item {:?}: unknown key {k:?} ignored", item.id));
        }
    }
    Ok(Some(item))
}

/// Apply partial updates (`Context1.Update`). The type and children can't change this way.
pub fn apply_update(item: &mut Item, props: &Props, w: &mut Warnings) -> Result<(), ParseError> {
    for (k, v) in props {
        match k.as_str() {
            "id" | "type" | "items" => w.push(format!(
                "item {:?}: {k:?} can't be updated (use SetItems)",
                item.id
            )),
            key if COMMON_KEYS.contains(&key)
                || type_keys(item.kind.type_name()).contains(&key) =>
            {
                apply_prop(item, key, v, w)
            }
            _ => w.push(format!("item {:?}: unknown key {k:?} ignored", item.id)),
        }
    }
    if let Some(data) = &item.icon_data
        && data.len() > limits::ICON_BYTES
    {
        return Err(ParseError::Limit(format!(
            "item {:?}: icon-data over {} bytes",
            item.id,
            limits::ICON_BYTES
        )));
    }
    Ok(())
}

fn sanitize(s: &str, max_chars: usize, what: &str, id: &str, w: &mut Warnings) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > max_chars {
        w.push(format!(
            "item {id:?}: {what} truncated to {max_chars} chars"
        ));
        return clean.chars().take(max_chars).collect();
    }
    clean
}

fn color(s: &str) -> bool {
    let hex = s.strip_prefix('#').unwrap_or("");
    matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn apply_prop(item: &mut Item, k: &str, v: &Prop, w: &mut Warnings) {
    let id = item.id.clone();
    let bad = |w: &mut Warnings| {
        w.push(format!(
            "item {id:?}: {k:?} has the wrong type ({}), ignored",
            v.kind()
        ))
    };
    match k {
        "label" => match v.as_str() {
            Some(s) => item.label = Some(sanitize(s, limits::LABEL_CHARS, "label", &item.id, w)),
            None => bad(w),
        },
        "icon" => match v.as_str() {
            Some(s) => item.icon = Some(s.to_owned()),
            None => bad(w),
        },
        "icon-data" => match v {
            Prop::Bytes(b) if b.len() <= limits::ICON_BYTES => item.icon_data = Some(b.clone()),
            Prop::Bytes(_) => w.push(format!(
                "item {:?}: icon-data over {} bytes, ignored",
                item.id,
                limits::ICON_BYTES
            )),
            _ => bad(w),
        },
        "enabled" => match v.as_bool() {
            Some(b) => item.enabled = b,
            None => bad(w),
        },
        "visible" => match v.as_bool() {
            Some(b) => item.visible = b,
            None => bad(w),
        },
        "customizable" => match v.as_bool() {
            Some(b) => item.customizable = b,
            None => bad(w),
        },
        "classes" => match v.as_strs() {
            Some(c) => {
                item.classes = c
                    .into_iter()
                    .filter(|c| {
                        !c.is_empty()
                            && c.chars()
                                .all(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_')
                    })
                    .take(16)
                    .collect()
            }
            None => bad(w),
        },
        "values" => match v {
            Prop::Map(m) => {
                item.values = m
                    .iter()
                    .filter_map(|(k, v)| {
                        let s = match v {
                            Prop::Bool(b) => Scalar::Bool(*b),
                            Prop::Int(i) => Scalar::Int(*i),
                            Prop::Num(n) if n.is_finite() => Scalar::Num(*n),
                            Prop::Str(s) => Scalar::Str(
                                s.chars().filter(|c| !c.is_control()).take(256).collect(),
                            ),
                            _ => {
                                w.push(format!(
                                    "item {:?}: value {k:?} isn't a scalar, ignored",
                                    item.id
                                ));
                                return None;
                            }
                        };
                        Some((k.clone(), s))
                    })
                    .take(32)
                    .collect()
            }
            _ => bad(w),
        },
        "fg" | "bg" => match v.as_str() {
            Some(s) if color(s) => {
                let c = Some(s.to_owned());
                if k == "fg" { item.fg = c } else { item.bg = c }
            }
            _ => w.push(format!(
                "item {:?}: {k:?} must be #rrggbb or #rrggbbaa, ignored",
                item.id
            )),
        },
        "width" => match v {
            Prop::Map(m) => {
                let px = |key: &str| {
                    m.get(key)
                        .and_then(Prop::as_f64)
                        .map(|f| f.clamp(0.0, 4096.0) as u32)
                };
                item.min_width = px("min");
                item.max_width = px("max");
            }
            _ => bad(w),
        },
        "priority" => match v.as_i64() {
            Some(i) => item.priority = i.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            None => bad(w),
        },
        "gestures" => match v.as_strs() {
            Some(gs) => {
                item.gestures = gs
                    .iter()
                    .filter_map(|g| {
                        let p = Gesture::parse(g);
                        if p.is_none() {
                            w.push(format!("item {:?}: unknown gesture {g:?} ignored", item.id));
                        }
                        p
                    })
                    .collect();
                item.gestures.sort();
                item.gestures.dedup();
            }
            None => bad(w),
        },
        "a11y-label" => match v.as_str() {
            Some(s) => item.a11y_label = Some(sanitize(s, 256, "a11y-label", &item.id, w)),
            None => bad(w),
        },
        _ => apply_kind_prop(item, k, v, w),
    }
    if let ItemKind::Slider {
        value, min, max, ..
    } = &mut item.kind
    {
        if *min >= *max {
            w.push(format!(
                "item {:?}: slider min >= max, reset to 0..1",
                item.id
            ));
            (*min, *max) = (0.0, 1.0);
        }
        *value = value.clamp(*min, *max);
    }
}

fn apply_kind_prop(item: &mut Item, k: &str, v: &Prop, w: &mut Warnings) {
    let id = item.id.clone();
    let bad = |w: &mut Warnings| {
        w.push(format!(
            "item {id:?}: {k:?} has the wrong type ({}), ignored",
            v.kind()
        ))
    };
    match (&mut item.kind, k) {
        (ItemKind::Toggle { state }, "state") => match v.as_bool() {
            Some(b) => *state = b,
            None => bad(w),
        },
        (
            ItemKind::Slider {
                value,
                min,
                max,
                step,
            },
            key,
        ) => match v.as_f64() {
            Some(f) => match key {
                "value" => *value = f,
                "min" => *min = f,
                "max" => *max = f,
                _ => *step = f.max(0.0),
            },
            None => bad(w),
        },
        (ItemKind::Segmented { segments, .. }, "segments") => match v {
            Prop::List(l) if l.len() <= 32 => {
                *segments =
                    l.iter()
                        .map(|s| match s {
                            Prop::Map(m) => Segment {
                                label: m.get("label").and_then(Prop::as_str).map(|s| {
                                    sanitize(s, limits::LABEL_CHARS, "segment label", &id, w)
                                }),
                                icon: m.get("icon").and_then(Prop::as_str).map(str::to_owned),
                            },
                            _ => Segment::default(),
                        })
                        .collect()
            }
            _ => bad(w),
        },
        (ItemKind::Segmented { selected, .. }, "selected") => match v.as_i64() {
            Some(i) => *selected = i.clamp(-1, 31) as i32,
            None => bad(w),
        },
        (ItemKind::Progress { value, .. }, "value") => match v.as_f64() {
            Some(f) => *value = f.clamp(0.0, 1.0),
            None => bad(w),
        },
        (ItemKind::Progress { indeterminate, .. }, "indeterminate") => match v.as_bool() {
            Some(b) => *indeterminate = b,
            None => bad(w),
        },
        (ItemKind::Spacer { size }, "size") => match v.as_str() {
            Some("small") => *size = SpacerSize::Small,
            Some("large") => *size = SpacerSize::Large,
            Some("flexible") => *size = SpacerSize::Flexible,
            _ => w.push(format!(
                "item {id:?}: size must be small/large/flexible, ignored"
            )),
        },
        _ => w.push(format!("item {id:?}: {k:?} ignored")),
    }
    if let ItemKind::Segmented { segments, selected } = &mut item.kind
        && *selected >= segments.len() as i32
    {
        *selected = -1;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn props(kv: &[(&str, Prop)]) -> Props {
        kv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    pub fn s(x: &str) -> Prop {
        Prop::Str(x.into())
    }

    pub fn button(id: &str) -> Props {
        props(&[("id", s(id)), ("type", s("button")), ("label", s(id))])
    }

    #[test]
    fn parses_common_and_typed_props() {
        let mut w = vec![];
        let items = parse_items(
            &[
                props(&[
                    ("id", s("vol")),
                    ("type", s("slider")),
                    ("value", Prop::Num(5.0)),
                    ("max", Prop::Int(10)),
                    ("classes", Prop::List(vec![s("loud"), s("bad class!")])),
                    (
                        "values",
                        Prop::Map(props(&[
                            ("percentage", Prop::Int(50)),
                            ("x", Prop::List(vec![])),
                        ])),
                    ),
                    (
                        "gestures",
                        Prop::List(vec![s("hold"), s("swipe-left"), s("wiggle")]),
                    ),
                    ("fg", s("#ff0000")),
                    ("bg", s("red")),
                ]),
                props(&[
                    ("id", s("t")),
                    ("type", s("toggle")),
                    ("state", Prop::Bool(true)),
                    ("frobnicate", Prop::Int(1)),
                ]),
                props(&[("id", s("future")), ("type", s("hologram"))]),
            ],
            &mut w,
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].kind,
            ItemKind::Slider {
                value: 5.0,
                min: 0.0,
                max: 10.0,
                step: 0.0
            }
        );
        assert_eq!(items[0].classes, vec!["loud"]);
        assert_eq!(items[0].values.get("percentage"), Some(&Scalar::Int(50)));
        assert_eq!(items[0].gestures, vec![Gesture::Hold, Gesture::SwipeLeft]);
        assert_eq!(items[0].fg.as_deref(), Some("#ff0000"));
        assert_eq!(items[0].bg, None);
        assert_eq!(items[1].kind, ItemKind::Toggle { state: true });
        let joined = w.join("\n");
        for needle in ["wiggle", "\"bg\"", "frobnicate", "hologram", "\"x\""] {
            assert!(
                joined.contains(needle),
                "missing warning for {needle}: {joined}"
            );
        }
    }

    #[test]
    fn structural_errors() {
        let mut w = vec![];
        let e = |l: Vec<Props>| parse_items(&l, &mut vec![]).unwrap_err();
        assert!(matches!(
            e(vec![props(&[("type", s("button"))])]),
            ParseError::Invalid(_)
        ));
        assert!(matches!(
            e(vec![props(&[("id", s("a"))])]),
            ParseError::Invalid(_)
        ));
        assert!(matches!(
            e(vec![button("a"), button("a")]),
            ParseError::Invalid(_)
        ));
        let nested = props(&[
            ("id", s("g")),
            ("type", s("group")),
            (
                "items",
                Prop::List(vec![Prop::Map(props(&[
                    ("id", s("p")),
                    ("type", s("popover")),
                ]))]),
            ),
        ]);
        assert!(matches!(e(vec![nested]), ParseError::Invalid(_)));
        // ids are unique across the whole tree
        let dup = props(&[
            ("id", s("g")),
            ("type", s("group")),
            ("items", Prop::List(vec![Prop::Map(button("a"))])),
        ]);
        assert!(matches!(e(vec![dup, button("a")]), ParseError::Invalid(_)));
        let many: Vec<Props> = (0..=limits::ITEMS_PER_CONTEXT)
            .map(|i| button(&i.to_string()))
            .collect();
        assert!(matches!(e(many), ParseError::Limit(_)));
        let icon = |id: &str| {
            props(&[
                ("id", s(id)),
                ("type", s("button")),
                ("icon-data", Prop::Bytes(vec![0; limits::ICON_BYTES])),
            ])
        };
        assert!(matches!(
            e(vec![icon("a"), icon("b"), icon("c")]),
            ParseError::Limit(_)
        ));
        parse_items(&[icon("a")], &mut w).unwrap();
    }

    #[test]
    fn sanitizes_text() {
        let mut w = vec![];
        let long = "x".repeat(100);
        let items = parse_items(
            &[props(&[
                ("id", s("a")),
                ("type", s("label")),
                ("label", s(&format!("a\u{1b}[31mb\n{long}"))),
            ])],
            &mut w,
        )
        .unwrap();
        let l = items[0].label.as_ref().unwrap();
        assert!(!l.contains('\u{1b}') && !l.contains('\n'));
        assert_eq!(l.chars().count(), limits::LABEL_CHARS);
        assert!(w[0].contains("truncated"));
    }

    #[test]
    fn updates_are_partial_and_clamped() {
        let mut w = vec![];
        let mut it = parse_items(&[props(&[("id", s("v")), ("type", s("slider"))])], &mut w)
            .unwrap()
            .remove(0);
        apply_update(
            &mut it,
            &props(&[
                ("value", Prop::Num(7.0)),
                ("type", s("button")),
                ("label", s("Vol")),
            ]),
            &mut w,
        )
        .unwrap();
        assert_eq!(
            it.kind,
            ItemKind::Slider {
                value: 1.0,
                min: 0.0,
                max: 1.0,
                step: 0.0
            }
        );
        assert_eq!(it.label.as_deref(), Some("Vol"));
        assert!(w.iter().any(|x| x.contains("can't be updated")));
        apply_update(&mut it, &props(&[("min", Prop::Num(5.0))]), &mut w).unwrap();
        assert!(w.iter().any(|x| x.contains("min >= max")));
    }
}
