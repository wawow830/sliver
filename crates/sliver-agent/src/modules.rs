//! The module runtime: turns resolved [`ModuleInstance`]s into drawable [`Cell`]s
//! (customization.md §3–4).
//!
//! Every module goes through the same path: gather values → `format_module` (waybar's
//! format/format-icons/states engine) → content + classes. Built-ins differ only in where their
//! values come from, so user modules get the same powers.

use chrono::format::{Item as FmtItem, StrftimeItems};
use chrono::{DateTime, Local, Timelike};
use sliver_config::{
    Action, FireOn, FmtValue, Gesture, ModuleInstance, State, ValueMap, Values, format_module,
};
use sliver_desktop::DesktopState;
use sliver_render::{Content, Width};
use sliver_session::{BatteryState, SessionState};

use crate::icons::Icons;

/// Everything modules can read.
#[derive(Debug, Clone)]
pub struct World {
    pub now: DateTime<Local>,
    pub session: SessionState,
    pub desktop: DesktopState,
}

impl World {
    /// A world with no session or desktop data (headless previews, tests).
    pub fn at(now: DateTime<Local>) -> World {
        World {
            now,
            session: SessionState::default(),
            desktop: DesktopState::default(),
        }
    }
}

/// Per-module UI state that survives re-resolution, keyed by [`Cell::key`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiState {
    /// `format-alt` toggled on.
    pub show_alt: bool,
    /// A drawer group is open.
    pub expanded: bool,
}

/// What a touch on a cell does.
#[derive(Debug, Clone, PartialEq)]
pub enum Touch {
    /// Not touchable.
    None,
    /// sliverd handles it alone: press/release these keys (key modules with only a key on tap).
    Keys(Vec<u16>),
    /// Touches come to the agent for gesture recognition.
    Agent,
}

/// One drawable module.
#[derive(Debug, Clone)]
pub struct Cell {
    /// Stable identity across frames: the module id plus its position in groups.
    pub key: String,
    /// The module type (`clock`, `audio/slider`, …), for module actions.
    pub kind: String,
    /// `audio` modules with `node = "source"`: the microphone.
    pub mic: bool,
    pub css_id: String,
    pub classes: Vec<String>,
    pub width: Width,
    pub content: Content,
    pub touch: Touch,
    /// Gesture bindings, for the agent's recognizer.
    pub on: Vec<(Gesture, Action)>,
    /// When this cell's content changes next on its own (clock ticks), if ever.
    pub next_change: Option<DateTime<Local>>,
    /// Being touched (agent regions; sliverd draws key regions' feedback itself).
    pub pressed: bool,
    /// A toggle that is on (`:checked`).
    pub checked: bool,
    /// This cell is the handle of a drawer group: tapping it opens/closes the group.
    pub drawer: Option<Drawer>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drawer {
    /// The group's key (for [`UiState::expanded`]).
    pub group: String,
    /// Close again this long after opening (`drawer.collapse-after`).
    pub collapse_after: Option<std::time::Duration>,
}

/// Builds the cells for one zone's modules. Hidden modules produce nothing; groups flatten.
pub fn cells(
    modules: &[ModuleInstance],
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &dyn Fn(&str) -> UiState,
    out: &mut Vec<Cell>,
) {
    cells_in(modules, "", world, icons, icon_size, ui, out);
}

fn cells_in(
    modules: &[ModuleInstance],
    prefix: &str,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &dyn Fn(&str) -> UiState,
    out: &mut Vec<Cell>,
) {
    for m in modules {
        let key = format!("{prefix}{}", m.id);
        if m.id.kind == "group" {
            let drawer = m.config.contains_key("drawer");
            let open = ui(&key).expanded;
            // A closed drawer shows only its first module (the handle).
            let shown = if drawer && !open { 1 } else { m.children.len() };
            let start = out.len();
            let children = &m.children[..shown.min(m.children.len())];
            cells_in(
                children,
                &format!("{key}/"),
                world,
                icons,
                icon_size,
                ui,
                out,
            );
            if drawer && let Some(handle) = out.get_mut(start) {
                let collapse_after = m
                    .config
                    .get("drawer")
                    .and_then(|d| d.get("collapse-after"))
                    .and_then(|v| sliver_config::parse_duration(v).ok());
                handle.drawer = Some(Drawer {
                    group: key.clone(),
                    collapse_after,
                });
                handle.checked = open;
                if handle.touch == Touch::None {
                    handle.touch = Touch::Agent;
                }
            }
            for c in &mut out[start..] {
                c.classes
                    .push(format!("group-{}", m.id.name.as_deref().unwrap_or("")));
                if drawer {
                    c.classes.push("drawer-child".into());
                }
            }
            continue;
        }
        let state = ui(&key);
        if let Some(c) = cell(m, key, world, icons, icon_size, &state) {
            out.push(c);
        }
    }
}

/// What a module contributes before the shared formatting path.
struct Spec {
    values: Box<dyn Values>,
    default_format: &'static str,
    percentage: Option<f64>,
    status: Option<&'static str>,
    /// Battery-style: lower percentages are worse.
    lower_is_worse: bool,
    default_icon: Option<String>,
    /// Draw as a slider at this value (0..1).
    slider: Option<f32>,
    checked: bool,
    /// Bindings used for gestures the user didn't bind (`ModuleAction`s the agent runs).
    defaults: Vec<(Gesture, Action)>,
}

impl Spec {
    fn new(values: impl Values + 'static, default_format: &'static str) -> Spec {
        Spec {
            values: Box::new(values),
            default_format,
            percentage: None,
            status: None,
            lower_is_worse: false,
            default_icon: None,
            slider: None,
            checked: false,
            defaults: Vec::new(),
        }
    }

    fn icon(mut self, name: impl Into<String>) -> Spec {
        self.default_icon = Some(name.into());
        self
    }

    fn on(mut self, g: Gesture, action: &str) -> Spec {
        self.defaults.push((g, Action::ModuleAction(action.into())));
        self
    }
}

/// Text from outside (titles, names) is escaped so it can't inject markup.
fn esc(s: &str) -> FmtValue {
    FmtValue::Text(sliver_render::markup::escape(s))
}

/// The module's spec for this world, or `None` to hide it (no data, nothing to show).
fn spec(m: &ModuleInstance, world: &World, ui: &UiState) -> Option<Spec> {
    let s = &world.session;
    let kind = m.id.kind.as_str();
    let pct = |v: Option<u8>| v.map(f64::from);
    Some(match kind {
        "key" | "custom" | "spacer" => Spec::new(ValueMap::new(), ""),
        "clock" => Spec::new(Clock(world.now), CLOCK_DEFAULT),
        "battery" => {
            let p = &s.power;
            let level = p.battery_pct?;
            let charging = matches!(
                p.battery_state,
                BatteryState::Charging | BatteryState::PendingCharge
            );
            let mut sp = Spec::new(
                ValueMap::new().with("capacity", level as f64),
                "{capacity}%",
            )
            .icon(battery_icon(level, charging));
            sp.percentage = Some(level as f64);
            sp.lower_is_worse = true;
            sp.status = match p.battery_state {
                BatteryState::Charging | BatteryState::PendingCharge => Some("charging"),
                BatteryState::FullyCharged => Some("full"),
                BatteryState::Discharging | BatteryState::PendingDischarge => Some("discharging"),
                _ => None,
            };
            sp
        }
        "backlight" | "kbd-backlight" | "backlight/slider" | "kbd-backlight/slider" => {
            let kbd = kind.starts_with("kbd");
            let level = if kbd {
                s.brightness.keyboard_pct
            } else {
                s.brightness.screen_pct
            }?;
            let icon = if kbd {
                "keyboard-brightness-symbolic"
            } else {
                "display-brightness-symbolic"
            };
            let mut sp = Spec::new(
                ValueMap::new().with("percent", level as f64),
                if kind.ends_with("/slider") {
                    ""
                } else {
                    "{percent}%"
                },
            )
            .icon(icon);
            sp.percentage = pct(Some(level));
            if kind.ends_with("/slider") {
                sp = slider(sp, m, ui, level);
            }
            sp
        }
        "audio" | "audio/slider" => {
            let a = &s.audio;
            let mic = m.config.get("node").and_then(|v| v.as_str()) == Some("source");
            let (vol, muted) = if mic {
                (a.mic_volume_pct?, a.mic_muted.unwrap_or(false))
            } else {
                (a.volume_pct?, a.muted.unwrap_or(false))
            };
            let icon = match (mic, muted, vol) {
                (true, true, _) => "microphone-disabled-symbolic",
                (true, false, _) => "audio-input-microphone-symbolic",
                (false, true, _) => "audio-volume-muted-symbolic",
                (false, false, 0..=33) => "audio-volume-low-symbolic",
                (false, false, 34..=66) => "audio-volume-medium-symbolic",
                _ => "audio-volume-high-symbolic",
            };
            let mut sp = Spec::new(
                ValueMap::new().with("volume", vol as f64),
                if kind.ends_with("/slider") {
                    ""
                } else {
                    "{volume}%"
                },
            )
            .icon(icon);
            sp.percentage = pct(Some(vol));
            sp.status = muted.then_some("muted");
            sp = sp.on(Gesture::Tap, "toggle-mute");
            if kind == "audio/slider" {
                sp.defaults.clear();
                sp = slider(sp, m, ui, vol.min(100));
            }
            sp
        }
        "mpris" => {
            let media = s.media.as_ref()?;
            let status = crate::context::media_status(media.status);
            let glyph = if status == "playing" { "⏸" } else { "▶" };
            let v = ValueMap(
                [
                    ("title", esc(&media.title)),
                    ("artist", esc(&media.artist)),
                    ("album", esc(&media.album)),
                    ("player", esc(&media.identity)),
                    ("status", FmtValue::Text(status.into())),
                    ("status_icon", FmtValue::Text(glyph.into())),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            );
            let mut sp = Spec::new(v, "{title}")
                .on(Gesture::Tap, "play-pause")
                .on(Gesture::SwipeLeft, "next")
                .on(Gesture::SwipeRight, "previous");
            sp.status = Some(status);
            sp
        }
        "idle-inhibitor" => {
            let on = s.caffeine;
            let mut sp = Spec::new(
                ValueMap::new().with("status", if on { "activated" } else { "deactivated" }),
                "☕",
            )
            .on(Gesture::Tap, "toggle");
            sp.status = Some(if on { "activated" } else { "deactivated" });
            sp.checked = on;
            sp
        }
        "power-profiles" => {
            let p = s.power.profile.as_deref()?;
            Spec::new(ValueMap::new().with("profile", p).with("", p), "{profile}")
        }
        "language" => {
            let l = world.desktop.keyboard_layout.as_deref()?;
            Spec::new(ValueMap::new().with("", esc(l)).with("long", esc(l)), "{}")
        }
        "window" => {
            let w = world.desktop.window.as_ref()?;
            Spec::new(
                ValueMap::new()
                    .with("title", esc(&w.title))
                    .with("app_id", esc(&w.class))
                    .with("", esc(&w.title)),
                "{title}",
            )
        }
        "mode" => {
            let m = world.desktop.submap.as_deref()?;
            Spec::new(ValueMap::new().with("", esc(m)), "{}")
        }
        // No data source wired yet: show nothing rather than a broken placeholder.
        _ => return None,
    })
}

/// Sliders: collapsed ones show just their icon until tapped open (like macOS's control strip).
fn slider(mut sp: Spec, m: &ModuleInstance, ui: &UiState, level: u8) -> Spec {
    let collapsed = m
        .config
        .get("collapsed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if collapsed && !ui.expanded {
        return sp;
    }
    sp.slider = Some(level as f32 / 100.0);
    sp.defaults.retain(|(g, _)| *g != Gesture::Tap);
    sp.on(Gesture::Drag, "set")
}

/// tiny-dfr's battery icon thresholds, on the built-in icon set.
fn battery_icon(level: u8, charging: bool) -> &'static str {
    if charging {
        match level {
            0..=20 => "battery_charging_20",
            21..=30 => "battery_charging_30",
            31..=50 => "battery_charging_50",
            51..=60 => "battery_charging_60",
            61..=80 => "battery_charging_80",
            81..=99 => "battery_charging_90",
            _ => "battery_charging_full",
        }
    } else {
        match level {
            0 => "battery_0_bar",
            1..=20 => "battery_1_bar",
            21..=30 => "battery_2_bar",
            31..=50 => "battery_3_bar",
            51..=60 => "battery_4_bar",
            61..=80 => "battery_5_bar",
            81..=99 => "battery_6_bar",
            _ => "battery_full",
        }
    }
}

fn cell(
    m: &ModuleInstance,
    key: String,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &UiState,
) -> Option<Cell> {
    let c = &m.common;
    let sp = spec(m, world, ui)?;
    let next_change = (m.id.kind == "clock")
        .then(|| next_clock_tick(world.now, c.format.as_deref().unwrap_or(CLOCK_DEFAULT)));
    let st = State {
        percentage: sp.percentage,
        status: sp.status,
        show_alt: ui.show_alt,
        lower_is_worse: sp.lower_is_worse,
        ..State::default()
    };
    let mut classes = m.classes.clone();
    let content = if m.id.kind == "spacer" {
        Content::Spacer
    } else {
        let f = format_module(c, sp.default_format, &st, sp.values.as_ref());
        if f.hidden {
            return None;
        }
        classes.extend(f.classes);
        let icon_name = f
            .icon
            .map(|i| i.0)
            .or_else(|| c.icon.clone())
            .or(sp.default_icon.clone());
        let size = c.icon_size.unwrap_or(icon_size);
        let icon = icon_name.and_then(|n| icons.get(&n, c.icon_theme.as_deref(), size));
        // Format strings are Pango-style markup, like waybar (values from outside are escaped).
        match (sp.slider, icon, f.text.is_empty()) {
            (Some(value), icon, _) => Content::Slider { icon, value },
            (None, Some(i), true) => Content::Icon(i),
            (None, Some(i), false) => Content::IconMarkup(i, f.text),
            (None, None, false) => Content::Markup(f.text),
            (None, None, true) => Content::Blank,
        }
    };
    // User bindings first, then `actions = { on-x = "name" }`, then the module's defaults.
    let mut on: Vec<(Gesture, Action)> = c.on.iter().map(|(g, a)| (*g, a.clone())).collect();
    for (g, name) in &c.actions {
        if !on.iter().any(|(x, _)| x == g) {
            on.push((*g, Action::ModuleAction(name.clone())));
        }
    }
    for (g, a) in sp.defaults {
        if !on.iter().any(|(x, _)| *x == g) {
            on.push((g, a));
        }
    }
    let collapsed_slider = m.id.kind.ends_with("/slider") && sp.slider.is_none();
    let mut touch = touch_kind(m, &on);
    if collapsed_slider {
        touch = Touch::Agent;
    }
    Some(Cell {
        key: key.clone(),
        kind: m.id.kind.clone(),
        mic: m.config.get("node").and_then(|v| v.as_str()) == Some("source"),
        css_id: m.css_id.clone(),
        classes,
        width: match c.width {
            sliver_config::Width::Auto => Width::Auto,
            sliver_config::Width::Fixed(px) => Width::Fixed(px as f32),
            sliver_config::Width::Fill(n) => Width::Fill(n),
        },
        content,
        touch,
        on,
        next_change,
        pressed: false,
        checked: sp.checked,
        // A collapsed slider opens like a drawer handle and closes on its own.
        drawer: (collapsed_slider
            || m.id.kind.ends_with("/slider") && ui.expanded && is_collapsible(m))
        .then(|| Drawer {
            group: key,
            collapse_after: Some(std::time::Duration::from_secs(5)),
        }),
    })
}

fn is_collapsible(m: &ModuleInstance) -> bool {
    m.config
        .get("collapsed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Key regions are the fast path: sliverd presses and releases the keys itself, so they work
/// even if the agent hangs. Only plain "tap = key, fire on press" bindings qualify (repeat is
/// the kernel's job once the key is down).
fn touch_kind(m: &ModuleInstance, on: &[(Gesture, Action)]) -> Touch {
    let press = m.common.fire_on == Some(FireOn::Press);
    match on {
        [(Gesture::Tap, Action::Key(k))] if press => Touch::Keys(vec![k.code()]),
        [(Gesture::Tap, Action::Keys(ks))] if press => {
            Touch::Keys(ks.iter().map(|k| k.code()).collect())
        }
        [] if m.common.actions.is_empty() && m.common.format_alt.is_none() => Touch::None,
        _ => Touch::Agent,
    }
}

const CLOCK_DEFAULT: &str = "{:%H:%M}";

/// Clock placeholders: `{}`/`{time}` with a strftime spec, like waybar (`{:%a %d %b}`).
struct Clock(DateTime<Local>);

impl Values for Clock {
    fn format(&self, name: &str, spec: Option<&str>) -> Option<String> {
        match name {
            "" | "time" => {
                let spec = spec.unwrap_or("%H:%M");
                let items: Vec<FmtItem> = StrftimeItems::new(spec).collect();
                if items.iter().any(|i| matches!(i, FmtItem::Error)) {
                    return Some(format!("⚠ bad time format {spec:?}"));
                }
                Some(self.0.format_with_items(items.into_iter()).to_string())
            }
            _ => None,
        }
    }
}

/// The next second if the format shows seconds, else the next minute.
fn next_clock_tick(now: DateTime<Local>, fmt: &str) -> DateTime<Local> {
    let seconds = ["%S", "%T", "%s", "%X", "%r", "%c", "%+"]
        .iter()
        .any(|s| fmt.contains(s));
    let base = now.with_nanosecond(0).unwrap_or(now);
    if seconds {
        base + chrono::Duration::seconds(1)
    } else {
        base.with_second(0).unwrap_or(base) + chrono::Duration::minutes(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use sliver_config::{Context, load_str};

    fn world() -> World {
        World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap())
    }

    fn build(toml: &str, layer: &str) -> Vec<Cell> {
        let loaded = load_str(toml, std::path::Path::new("."));
        assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
        let ctx = Context::new().with("physical_esc", true);
        let r = loaded.config.resolve(layer, &ctx).unwrap();
        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        for z in &r.zones {
            cells(
                &z.modules,
                &world(),
                &mut icons,
                36,
                &|_| UiState::default(),
                &mut out,
            );
        }
        out
    }

    #[test]
    fn fkeys_are_key_regions() {
        let cells = build("", "fn");
        assert_eq!(cells.len(), 12, "Esc is hidden with a physical Esc");
        assert_eq!(cells[0].touch, Touch::Keys(vec![59]));
        assert!(matches!(&cells[0].content, Content::Markup(t) if t == "F1"));
        assert_eq!(cells[0].css_id, "key-f1");
        assert_eq!(cells[0].width, Width::Fill(1));
    }

    #[test]
    fn clock_formats_and_ticks() {
        let cells = build(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["clock", "clock#s", "custom/hi"]
            [modules."clock#s"]
            format = "{:%H:%M:%S}"
            [modules."custom/hi"]
            format = "hi"
            on-tap = "notify-send hi"
            "#,
            "base",
        );
        assert!(matches!(&cells[0].content, Content::Markup(t) if t == "09:41"));
        assert_eq!(
            cells[0].next_change.unwrap().format("%H:%M:%S").to_string(),
            "09:42:00"
        );
        assert_eq!(
            cells[1].next_change.unwrap().format("%H:%M:%S").to_string(),
            "09:41:08"
        );
        assert_eq!(cells[1].classes, ["s"]);
        assert_eq!(cells[2].touch, Touch::Agent);
        assert_eq!(cells[0].touch, Touch::None);
    }

    #[test]
    fn closed_drawer_shows_its_handle() {
        let cells = build("", "base");
        let keys: Vec<&str> = cells.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["group/control-strip/custom/expand", "key/Mute"]);
        assert!(cells[0].classes.contains(&"drawer-child".to_string()));
        let d = cells[0].drawer.as_ref().unwrap();
        assert_eq!(
            (d.group.as_str(), d.collapse_after),
            (
                "group/control-strip",
                Some(std::time::Duration::from_secs(5))
            )
        );
        assert_eq!(cells[0].touch, Touch::Agent, "the handle opens the drawer");
        assert!(matches!(cells[1].content, Content::Icon(_)));
    }
}
