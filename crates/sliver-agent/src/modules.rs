//! The module runtime: turns resolved [`ModuleInstance`]s into drawable [`Cell`]s
//! (customization.md §3–4).
//!
//! Every module goes through the same path: gather values → `format_module` (waybar's
//! format/format-icons/states engine) → content + classes. Built-ins differ only in where their
//! values come from, so user modules get the same powers.

use chrono::format::{Item as FmtItem, StrftimeItems};
use chrono::{DateTime, Local, Timelike};
use sliver_config::{
    Action, FireOn, Gesture, ModuleInstance, State, ValueMap, Values, format_module,
};
use sliver_render::{Content, Width};

use crate::icons::Icons;

/// Everything modules can read. Grows as session/desktop sources are wired in.
#[derive(Debug, Clone)]
pub struct World {
    pub now: DateTime<Local>,
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
    pub css_id: String,
    pub classes: Vec<String>,
    pub width: Width,
    pub content: Content,
    pub touch: Touch,
    /// Gesture bindings, for the agent's recognizer.
    pub on: Vec<(Gesture, Action)>,
    /// When this cell's content changes next on its own (clock ticks), if ever.
    pub next_change: Option<DateTime<Local>>,
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

fn cell(
    m: &ModuleInstance,
    key: String,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &UiState,
) -> Option<Cell> {
    let c = &m.common;
    let mut next_change = None;
    let formatted = match m.id.kind.as_str() {
        "spacer" => None,
        "key" | "custom" => Some(format_module(c, "", &state(ui), &ValueMap::new())),
        "clock" => {
            let fmt = c.format.as_deref().unwrap_or(CLOCK_DEFAULT);
            next_change = Some(next_clock_tick(world.now, fmt));
            Some(format_module(
                c,
                CLOCK_DEFAULT,
                &state(ui),
                &Clock(world.now),
            ))
        }
        // No data source wired yet: show nothing rather than a broken placeholder.
        _ => return None,
    };
    let mut classes = m.classes.clone();
    let content = match formatted {
        None => Content::Spacer,
        Some(f) if f.hidden => return None,
        Some(f) => {
            classes.extend(f.classes);
            let icon_name = f.icon.map(|i| i.0).or_else(|| c.icon.clone());
            let size = c.icon_size.unwrap_or(icon_size);
            let icon = icon_name.and_then(|n| icons.get(&n, c.icon_theme.as_deref(), size));
            match (icon, f.text.is_empty()) {
                (Some(i), true) => Content::Icon(i),
                (Some(i), false) => Content::IconText(i, f.text),
                (None, false) => Content::Text(f.text),
                (None, true) => Content::Blank,
            }
        }
    };
    let on: Vec<(Gesture, Action)> = c.on.iter().map(|(g, a)| (*g, a.clone())).collect();
    let touch = touch_kind(m, &on);
    Some(Cell {
        key,
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
    })
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

fn state(ui: &UiState) -> State<'static> {
    State {
        show_alt: ui.show_alt,
        ..State::default()
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
        World {
            now: Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap(),
        }
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
        assert!(matches!(&cells[0].content, Content::Text(t) if t == "F1"));
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
        assert!(matches!(&cells[0].content, Content::Text(t) if t == "09:41"));
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
        assert!(matches!(cells[1].content, Content::Icon(_)));
    }
}
