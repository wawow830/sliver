//! Module references, waybar aliases, gestures and the keys every module shares
//! (customization.md §3).

use std::collections::BTreeMap;
use std::fmt;

use toml::{Table, Value};

use crate::action::Action;
use crate::condition::Condition;
use crate::diag::{Diagnostics, Reader};
use crate::keys::Key;
use crate::values::{FireOn, Interval, Width};

/// Namespaces whose second part is a *name*, not part of the type (`custom/vpn`, `key/F1`).
const PARAMETRIC: &[&str] = &["custom", "key", "group", "app", "custom-graph"];

/// Built-in module types (customization.md §3). Unknown types are a warning, because a newer
/// agent may know them.
pub const MODULE_TYPES: &[&str] = &[
    "key",
    "spacer",
    "clock",
    "battery",
    "backlight",
    "backlight/slider",
    "kbd-backlight",
    "kbd-backlight/slider",
    "audio",
    "audio/slider",
    "mpris",
    "workspaces",
    "window",
    "mode",
    "language",
    "idle-inhibitor",
    "dnd",
    "power-profiles",
    "cpu",
    "memory",
    "temperature",
    "disk",
    "load",
    "network",
    "bluetooth",
    "graph",
    "privacy",
    "systemd-failed-units",
    "taskbar",
    "notifications",
    "emoji",
    "timer",
    "image",
    "group",
    "app-controls",
    "status-items",
    "app",
    "custom",
];

/// waybar type names → sliver type names, so ported configs work.
fn type_alias(t: &str) -> Option<&'static str> {
    Some(match t {
        "pulseaudio" | "wireplumber" => "audio",
        "pulseaudio/slider" => "audio/slider",
        "hyprland/workspaces"
        | "sway/workspaces"
        | "niri/workspaces"
        | "ext/workspaces"
        | "river/tags" => "workspaces",
        "hyprland/window" | "sway/window" | "niri/window" => "window",
        "hyprland/submap" | "sway/mode" => "mode",
        "hyprland/language" | "sway/language" | "niri/language" => "language",
        "idle_inhibitor" => "idle-inhibitor",
        "power-profiles-daemon" => "power-profiles",
        "upower" => "battery",
        "tray" => "status-items",
        "wlr/taskbar" => "taskbar",
        "custom-graph" => "graph",
        _ => return None,
    })
}

/// A parsed module reference: `battery#bat2`, `custom/vpn`, `key/F1#auto`, `backlight/slider`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId {
    /// Canonical type after aliasing (`audio`, `backlight/slider`, `custom`, `key`).
    pub kind: String,
    /// The name inside a parametric namespace (`vpn` in `custom/vpn`, `F1` in `key/F1`).
    pub name: Option<String>,
    /// The `#instance` suffix.
    pub instance: Option<String>,
}

impl ModuleId {
    pub fn parse(s: &str) -> Result<ModuleId, String> {
        let s = s.trim();
        let (base, instance) = match s.split_once('#') {
            Some((b, i)) if !i.is_empty() => (b, Some(i.to_owned())),
            Some(_) => return Err(format!("empty #instance in {s:?}")),
            None => (s, None),
        };
        if base.is_empty() {
            return Err("empty module name".into());
        }
        let base = type_alias(base).unwrap_or(base);
        if let Some((ns, name)) = base.split_once('/') {
            let ns = type_alias(ns).unwrap_or(ns);
            if PARAMETRIC.contains(&ns) || ns == "graph" {
                if name.is_empty() {
                    return Err(format!("{ns}/ needs a name"));
                }
                return Ok(ModuleId {
                    kind: ns.to_owned(),
                    name: Some(name.to_owned()),
                    instance,
                });
            }
        }
        Ok(ModuleId {
            kind: base.to_owned(),
            name: None,
            instance,
        })
    }

    /// The CSS id (customization.md §7): `custom/vpn` → `custom-vpn`, `key/F1` → `key-f1`.
    pub fn css_id(&self) -> String {
        let mut s = self.kind.replace('/', "-");
        if let Some(n) = &self.name {
            s.push('-');
            s.push_str(n);
        }
        s.to_ascii_lowercase()
    }

    pub fn is_known_type(&self) -> bool {
        MODULE_TYPES.contains(&self.kind.as_str())
    }

    /// For `key/<Key>` modules, the key.
    pub fn key(&self) -> Option<Key> {
        (self.kind == "key")
            .then(|| self.name.as_deref().and_then(Key::from_name))
            .flatten()
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.kind)?;
        if let Some(n) = &self.name {
            write!(f, "/{n}")?;
        }
        if let Some(i) = &self.instance {
            write!(f, "#{i}")?;
        }
        Ok(())
    }
}

/// Touch gestures a module can bind (customization.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Gesture {
    Tap,
    DoubleTap,
    Hold,
    Release,
    SwipeLeft,
    SwipeRight,
    Drag,
    Update,
}

impl Gesture {
    pub const ALL: [Gesture; 8] = [
        Gesture::Tap,
        Gesture::DoubleTap,
        Gesture::Hold,
        Gesture::Release,
        Gesture::SwipeLeft,
        Gesture::SwipeRight,
        Gesture::Drag,
        Gesture::Update,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Gesture::Tap => "on-tap",
            Gesture::DoubleTap => "on-double-tap",
            Gesture::Hold => "on-hold",
            Gesture::Release => "on-release",
            Gesture::SwipeLeft => "on-swipe-left",
            Gesture::SwipeRight => "on-swipe-right",
            Gesture::Drag => "on-drag",
            Gesture::Update => "on-update",
        }
    }

    /// The waybar spelling that maps onto this gesture.
    pub fn waybar_alias(self) -> Option<&'static str> {
        Some(match self {
            Gesture::Tap => "on-click",
            Gesture::DoubleTap => "on-double-click",
            Gesture::Hold => "on-click-right",
            Gesture::Release => "on-click-release",
            Gesture::SwipeLeft => "on-scroll-down",
            Gesture::SwipeRight => "on-scroll-up",
            Gesture::Drag | Gesture::Update => return None,
        })
    }

    pub fn from_key(k: &str) -> Option<Gesture> {
        Gesture::ALL
            .into_iter()
            .find(|g| g.key() == k || g.waybar_alias() == Some(k))
    }
}

/// waybar gestures with no touch equivalent: warn instead of silently ignoring.
const NO_TOUCH_EQUIVALENT: &[&str] = &[
    "on-click-middle",
    "on-click-middle-release",
    "on-double-click-middle",
    "on-triple-click-middle",
    "on-triple-click",
    "on-double-click-right",
    "on-triple-click-right",
    "on-click-right-release",
    "on-click-backward",
    "on-click-forward",
    "on-click-backward-release",
    "on-click-forward-release",
    "on-double-click-backward",
    "on-triple-click-backward",
    "on-double-click-forward",
    "on-triple-click-forward",
    "on-scroll-left",
    "on-scroll-right",
];

#[derive(Debug, Clone, PartialEq)]
pub struct MenuEntry {
    pub label: String,
    pub icon: Option<String>,
    pub on_tap: Action,
}

/// Keys every module understands. Type-specific keys stay in [`crate::ModuleInstance::config`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Common {
    pub format: Option<String>,
    pub format_alt: Option<String>,
    /// `format-<state>`, `format-<status>`, `format-<status>-<state>`, keyed by the suffix.
    pub format_variants: BTreeMap<String, String>,
    /// Raw `format-icons` (array, table or string). The format engine (T25) interprets it.
    pub format_icons: Option<Value>,
    /// `states`, sorted by threshold.
    pub states: Vec<(String, f64)>,
    pub icon: Option<String>,
    pub icon_theme: Option<String>,
    pub icon_size: Option<u32>,
    pub width: Width,
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
    pub align: Option<f64>,
    pub justify: Option<String>,
    pub class: Vec<String>,
    pub when: Option<Condition>,
    pub hide_empty_text: bool,
    pub detail: bool,
    pub detail_format: Option<String>,
    pub fire_on: Option<FireOn>,
    pub repeat: Option<bool>,
    pub on: BTreeMap<Gesture, Action>,
    /// Gestures bound to the module's built-in actions (`actions = { on-hold = "mode" }`).
    pub actions: BTreeMap<Gesture, String>,
    pub menu: Vec<MenuEntry>,
    pub menu_trigger: Option<Gesture>,
    pub swipe_threshold: Option<u32>,
    pub interval: Option<Interval>,
    /// For app-published modules: also deliver the gesture to the app when a user `on-*`
    /// binding exists (api.md §5). Default false, so the user binding replaces the app event.
    pub pass_through: bool,
}

impl Common {
    /// Reads the common keys from a module block and returns the remaining (type-specific) keys.
    pub(crate) fn parse(t: &Table, path: &str, d: &mut Diagnostics) -> (Common, Table) {
        let mut r = Reader::new(t, path);
        let mut c = Common {
            format: r.string("format", d),
            format_alt: r.string("format-alt", d),
            format_icons: r.get("format-icons").cloned(),
            icon: r.string("icon", d).filter(|s| !s.is_empty()),
            icon_theme: r.string("icon-theme", d),
            icon_size: r.uint("icon-size", 512, d).map(|n| n as u32),
            min_length: r.uint("min-length", 1000, d).map(|n| n as u32),
            max_length: r.uint("max-length", 1000, d).map(|n| n as u32),
            align: r.number("align", d),
            justify: r.string("justify", d),
            class: r.strings("class", d),
            hide_empty_text: r.bool("hide-empty-text", d).unwrap_or(false),
            detail_format: r
                .string("detail-format", d)
                .or_else(|| r.string("tooltip-format", d)),
            detail: r
                .bool("detail", d)
                .or_else(|| r.bool("tooltip", d))
                .unwrap_or(true),
            repeat: r.bool("repeat", d),
            swipe_threshold: r
                .uint("swipe-threshold", 2008, d)
                .or_else(|| {
                    r.number("smooth-scrolling-threshold", d)
                        .map(|f| f.max(0.0) as u64)
                })
                .map(|n| n as u32),
            ..Common::default()
        };
        if let Some(v) = r.get("width") {
            match Width::parse(v) {
                Ok(w) => c.width = w,
                Err(e) => d.error(r.path("width"), e),
            }
        } else if r.bool("expand", d) == Some(true) {
            c.width = Width::Fill(1);
        }
        if let Some(s) = r.string("fire-on", d) {
            c.fire_on = FireOn::parse(&s)
                .map_err(|e| d.error(r.path("fire-on"), e))
                .ok();
        }
        if let Some(v) = r.get("when") {
            c.when = Condition::parse(v, &r.path("when"), d);
        }
        if let Some(v) = r.get("interval") {
            c.interval = Interval::parse(v, &r.path("interval"), d);
        }
        c.pass_through = r.bool("pass-through", d).unwrap_or(false);
        if let Some(states) = r.table("states", d) {
            for (name, v) in states {
                match v.as_float().or(v.as_integer().map(|i| i as f64)) {
                    Some(n) => c.states.push((name.clone(), n)),
                    None => d.error(
                        crate::diag::join(&r.path("states"), name),
                        "a state threshold is a number",
                    ),
                }
            }
            c.states.sort_by(|a, b| a.1.total_cmp(&b.1));
        }
        for g in Gesture::ALL {
            for key in [Some(g.key()), g.waybar_alias()].into_iter().flatten() {
                let Some(v) = r.get(key) else { continue };
                match c.on.entry(g) {
                    std::collections::btree_map::Entry::Occupied(_) => d.warn(
                        r.path(key),
                        format!("{} is already set; this alias is ignored", g.key()),
                    ),
                    std::collections::btree_map::Entry::Vacant(e) => {
                        if let Some(a) = Action::parse(v, &r.path(key), d) {
                            e.insert(a);
                        }
                    }
                }
            }
        }
        if let Some(actions) = r.table("actions", d) {
            let p = r.path("actions");
            for (k, v) in actions {
                match (Gesture::from_key(k), v.as_str()) {
                    (Some(g), Some(name)) => {
                        c.actions.insert(g, name.to_owned());
                    }
                    _ => d.error(
                        crate::diag::join(&p, k),
                        "expected on-<gesture> = \"module action name\"",
                    ),
                }
            }
        }
        if let Some(v) = r.get("menu") {
            c.menu = parse_menu(v, &r.path("menu"), d);
        }
        if let Some(s) = r.string("menu-trigger", d) {
            c.menu_trigger =
                Gesture::from_key(&s).or_else(|| Gesture::from_key(&format!("on-{s}")));
            if c.menu_trigger.is_none() {
                d.error(r.path("menu-trigger"), format!("unknown gesture {s:?}"));
            }
        }
        for key in NO_TOUCH_EQUIVALENT {
            if r.get(key).is_some() {
                d.warn(
                    r.path(key),
                    "this mouse gesture has no touch equivalent (customization.md §10)",
                );
            }
        }
        // `format-<x>` variants. Must run last so only unread `format-*` keys are collected.
        let mut rest = Table::new();
        for (k, v) in t {
            if let Some(suffix) = k.strip_prefix("format-") {
                if matches!(suffix, "alt" | "icons") {
                    continue;
                }
                r.skip(k);
                match v.as_str() {
                    Some(s) => {
                        c.format_variants.insert(suffix.to_owned(), s.to_owned());
                    }
                    None => d.error(r.path(k), "expected a format string"),
                }
            }
        }
        for (k, v) in t {
            if !COMMON_KEYS.contains(&k.as_str())
                && !k.starts_with("format-")
                && !NO_TOUCH_EQUIVALENT.contains(&k.as_str())
            {
                rest.insert(k.clone(), v.clone());
            }
        }
        // Type-specific keys are checked by `check_type_keys`, so `r` doesn't warn for them.
        (c, rest)
    }

    /// True when a touch on this module does something.
    pub fn has_actions(&self) -> bool {
        !self.on.is_empty()
            || !self.actions.is_empty()
            || !self.menu.is_empty()
            || self.format_alt.is_some()
    }
}

/// Every key [`Common::parse`] consumes (plus their waybar aliases).
pub const COMMON_KEYS: &[&str] = &[
    "format",
    "format-alt",
    "format-icons",
    "icon",
    "icon-theme",
    "icon-size",
    "width",
    "expand",
    "min-length",
    "max-length",
    "align",
    "justify",
    "class",
    "when",
    "hide-empty-text",
    "detail",
    "detail-format",
    "tooltip",
    "tooltip-format",
    "fire-on",
    "repeat",
    "swipe-threshold",
    "smooth-scrolling-threshold",
    "states",
    "actions",
    "menu",
    "menu-trigger",
    "interval",
    "on-tap",
    "on-click",
    "on-double-tap",
    "on-double-click",
    "on-hold",
    "on-click-right",
    "on-release",
    "on-click-release",
    "on-swipe-left",
    "on-scroll-down",
    "on-swipe-right",
    "on-scroll-up",
    "on-drag",
    "on-update",
    "pass-through",
];

fn parse_menu(v: &Value, path: &str, d: &mut Diagnostics) -> Vec<MenuEntry> {
    let Some(items) = v.as_array() else {
        d.error(path, "menu is an array of { label, icon, on-tap }");
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let p = format!("{path}[{i}]");
            let Some(t) = item.as_table() else {
                d.error(&p, "menu entries are tables");
                return None;
            };
            let mut r = Reader::new(t, &p);
            let label = r.string("label", d);
            let icon = r.string("icon", d);
            let on_tap = r
                .get("on-tap")
                .and_then(|v| Action::parse(v, &format!("{p}.on-tap"), d));
            if on_tap.is_none() && !t.contains_key("on-tap") {
                d.error(&p, "menu entry needs on-tap");
            }
            r.finish(d);
            Some(MenuEntry {
                label: label?,
                icon,
                on_tap: on_tap?,
            })
        })
        .collect()
}

/// Type-specific keys for the module types whose schema lives in this crate. Other types are
/// owned by their runtime (T21) and get no unknown-key warnings here.
pub(crate) fn check_type_keys(id: &ModuleId, rest: &Table, path: &str, d: &mut Diagnostics) {
    let known: &[&str] = match id.kind.as_str() {
        "key" => &["key"],
        "spacer" | "app-controls" | "status-items" => &[],
        "group" => &["modules", "orientation", "drawer"],
        "custom" => &[
            "exec",
            "exec-if",
            "restart-interval",
            "signal",
            "exec-on-event",
            "return-type",
            "escape",
            "timeout",
            "env",
            "slider",
        ],
        _ => return,
    };
    for k in rest.keys() {
        if !known.contains(&k.as_str()) {
            d.warn(
                crate::diag::join(path, k),
                format!("unknown key for {} modules (ignored)", id.kind),
            );
        }
    }
}

/// Media key icons (XDG symbolic names) for `key/*` defaults.
fn key_icon(k: Key) -> Option<&'static str> {
    Some(match k.name() {
        "BrightnessDown" => "display-brightness-low-symbolic",
        "BrightnessUp" => "display-brightness-high-symbolic",
        "IllumDown" => "keyboard-brightness-low-symbolic",
        "IllumUp" => "keyboard-brightness-high-symbolic",
        "MicMute" => "microphone-disabled-symbolic",
        "Search" => "system-search-symbolic",
        "PreviousSong" => "media-skip-backward-symbolic",
        "PlayPause" => "media-playback-start-symbolic",
        "NextSong" => "media-skip-forward-symbolic",
        "Mute" => "audio-volume-muted-symbolic",
        "VolumeDown" => "audio-volume-low-symbolic",
        "VolumeUp" => "audio-volume-high-symbolic",
        _ => return None,
    })
}

/// The block a bare `key/<Key>` reference gets before any user block is merged on top.
pub(crate) fn key_defaults(k: Key) -> Table {
    let mut t = Table::new();
    match key_icon(k) {
        Some(icon) => {
            t.insert("icon".into(), Value::String(icon.into()));
        }
        None => {
            let label = if k.name() == "Esc" {
                "esc".to_owned()
            } else {
                k.name().to_owned()
            };
            t.insert("format".into(), Value::String(label));
        }
    }
    let mut tap = Table::new();
    tap.insert("key".into(), Value::String(k.name().into()));
    t.insert("on-tap".into(), Value::Table(tap));
    t.insert("repeat".into(), Value::Boolean(true));
    t.insert("fire-on".into(), Value::String("press".into()));
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        let id = ModuleId::parse("custom/vpn#work").unwrap();
        assert_eq!(
            (id.kind.as_str(), id.name.as_deref(), id.instance.as_deref()),
            ("custom", Some("vpn"), Some("work"))
        );
        assert_eq!(id.css_id(), "custom-vpn");
        let id = ModuleId::parse("battery#bat2").unwrap();
        assert_eq!(
            (id.kind.as_str(), id.name, id.instance.as_deref()),
            ("battery", None, Some("bat2"))
        );
        assert_eq!(
            ModuleId::parse("backlight/slider").unwrap().kind,
            "backlight/slider"
        );
        assert_eq!(
            ModuleId::parse("hyprland/workspaces").unwrap().kind,
            "workspaces"
        );
        assert_eq!(
            ModuleId::parse("pulseaudio/slider").unwrap().kind,
            "audio/slider"
        );
        assert_eq!(
            ModuleId::parse("key/F1").unwrap().key().map(Key::code),
            Some(59)
        );
        assert_eq!(
            ModuleId::parse("key/F1#auto").unwrap().to_string(),
            "key/F1#auto"
        );
        assert_eq!(ModuleId::parse("custom-graph/cpu").unwrap().kind, "graph");
        assert!(ModuleId::parse("custom/").is_err());
        assert!(ModuleId::parse("clock#").is_err());
    }

    #[test]
    fn common_keys_and_aliases() {
        let t: Table = r#"
            format = "{icon} {capacity}%"
            format-charging = "C {capacity}%"
            format-icons = ["a", "b"]
            states = { warning = 30, critical = 15 }
            on-click = "pavucontrol"
            on-scroll-up = { key = "VolumeUp" }
            on-click-middle = "x"
            expand = true
            bat = "BAT1"
        "#
        .parse()
        .unwrap();
        let mut d = Diagnostics::default();
        let (c, rest) = Common::parse(&t, "modules.battery", &mut d);
        assert!(d.ok(), "{d:?}");
        assert_eq!(
            d.warnings.len(),
            1,
            "middle click has no touch equivalent: {d:?}"
        );
        assert_eq!(c.width, Width::Fill(1));
        assert_eq!(
            c.states,
            vec![("critical".into(), 15.0), ("warning".into(), 30.0)]
        );
        assert_eq!(
            c.format_variants.get("charging").map(String::as_str),
            Some("C {capacity}%")
        );
        assert_eq!(
            c.on.get(&Gesture::Tap),
            Some(&Action::Shell("pavucontrol".into()))
        );
        assert!(matches!(
            c.on.get(&Gesture::SwipeRight),
            Some(Action::Key(_))
        ));
        assert_eq!(rest.keys().collect::<Vec<_>>(), vec!["bat"]);
    }
}
