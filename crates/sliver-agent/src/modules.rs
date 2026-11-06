//! The module runtime: turns resolved [`ModuleInstance`]s into drawable [`Cell`]s
//! (sliver-config(5)).
//!
//! Every module goes through the same path: gather values → `format_module` (waybar's
//! format/format-icons/states engine) → content + classes. Built-ins differ only in where their
//! values come from, so user modules get the same powers.

use chrono::format::{Item as FmtItem, StrftimeItems};
use chrono::{DateTime, Local, Timelike};
use sliver_config::{
    Action, FireOn, FmtValue, Gesture, ModuleInstance, State, ValueMap, Values, format_module,
};
use std::collections::HashMap;

use sliver_config::Context;
use sliver_desktop::DesktopState;
use sliver_modules::Output;
use sliver_render::{Content, Width};
use sliver_session::{BatteryState, SessionState};

use crate::icons::Icons;

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
    /// The module id (`custom/vpn#work`), as the user module runtime knows it.
    pub id: String,
    /// The module type (`clock`, `audio/slider`, …), for module actions.
    pub kind: String,
    /// `audio` modules with `node = "source"`: the microphone.
    pub mic: bool,
    /// Drawn as a slider (`slider = true`), open or collapsed.
    pub slider: bool,
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
    /// `:disabled`.
    pub disabled: bool,
    /// Inline colours (published items' `fg`/`bg`).
    pub foreground: Option<sliver_render::Color>,
    pub background: Option<sliver_render::Color>,
    /// A published item: touches go to the app that owns it.
    pub published: Option<crate::published::PubRef>,
    /// Published items: also send the app its event when a user binding handled the gesture.
    pub pass_through: bool,
    /// What a workspaces/taskbar cell does to the compositor (focus; hold closes a window).
    pub target: Option<sliver_desktop::control::Action>,
    /// `timer` modules: the configured total.
    pub timer_total: Option<std::time::Duration>,
    /// `menu = [...]`: the entries of the sub-bar this module opens.
    pub menu: Vec<sliver_config::MenuEntry>,
    /// Part of a sized App controls / Status items slot (`width` = px or fill), by the slot's
    /// key: its published items, and the trailing `kind = "slot"` spacer that reserves the rest.
    pub slot: Option<String>,
    /// `spectrum = true`: the live spectrum's levels, drawn under the content. Empty: none.
    pub spectrum: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drawer {
    /// The group's key (for [`UiState::expanded`]).
    pub group: String,
    /// Close again this long after opening (`drawer.collapse-after`).
    pub collapse_after: Option<std::time::Duration>,
    /// `mode = "bar"`: tapping opens the group's other modules as a sub-bar, not inline.
    pub bar: bool,
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

/// Which of a zone's modules a cell shows: `zone_ids` are the zone's top-level module ids in
/// order, `zone_keys` the keys of the zone's cells in order, and `pos` the cell's position in
/// them. Repeated modules are told apart by counting; a group's children are consecutive cells
/// of one module. (The settings preview and Mirror's `HeldPressed` both use it.)
pub fn module_index(zone_ids: &[String], zone_keys: &[String], pos: usize) -> Option<usize> {
    let top = |key: &str| {
        zone_ids
            .iter()
            .find(|id| key == id.as_str() || key.starts_with(&format!("{id}/")))
            .cloned()
    };
    let mut count: HashMap<String, usize> = HashMap::new();
    let mut prev: Option<String> = None;
    for (p, k) in zone_keys.iter().enumerate() {
        let t = top(k)?;
        let group_child = *k != t;
        if !(group_child && prev.as_ref() == Some(&t)) {
            *count.entry(t.clone()).or_default() += 1;
        }
        if p == pos {
            let nth = count[&t] - 1;
            return zone_ids
                .iter()
                .enumerate()
                .filter(|(_, id)| **id == t)
                .map(|(i, _)| i)
                .nth(nth);
        }
        prev = Some(t);
    }
    None
}

/// Is this group a `drawer = { mode = "bar" }` one?
fn is_bar_drawer(m: &ModuleInstance) -> bool {
    drawer_opt(m, "mode").and_then(toml::Value::as_str) == Some("bar")
}

/// A drawer group's `drawer.<key>` setting.
fn drawer_opt<'a>(m: &'a ModuleInstance, key: &str) -> Option<&'a toml::Value> {
    m.config.get("drawer").and_then(|d| d.get(key))
}

/// The cell keys of drawer groups with `start-expanded = true`, in `zones`. The agent opens each
/// the first time it sees it; after that, taps and `collapse-after` decide.
pub fn start_expanded_drawers(zones: &[sliver_config::ResolvedZone]) -> Vec<String> {
    fn walk(ms: &[ModuleInstance], prefix: &str, out: &mut Vec<String>) {
        for m in ms.iter().filter(|m| m.id.kind == "group") {
            let key = format!("{prefix}{}", m.id);
            if drawer_opt(m, "start-expanded").and_then(toml::Value::as_bool) == Some(true) {
                out.push(key.clone());
            }
            walk(&m.children, &format!("{key}/"), out);
        }
    }
    let mut out = Vec::new();
    for z in zones {
        walk(&z.modules, "", &mut out);
    }
    out
}

/// The cells of an open bar-mode drawer: the group's modules after its leader, then a ✕. `key`
/// is the group's cell key (as in [`Drawer::group`]); `None` when no such group is in `zones`.
pub fn subbar_cells(
    zones: &[sliver_config::ResolvedZone],
    key: &str,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &dyn Fn(&str) -> UiState,
) -> Option<Vec<Cell>> {
    fn find<'a>(ms: &'a [ModuleInstance], prefix: &str, key: &str) -> Option<&'a ModuleInstance> {
        for m in ms {
            let k = format!("{prefix}{}", m.id);
            if m.id.kind == "group" {
                if k == key {
                    return Some(m);
                }
                if let Some(f) = find(&m.children, &format!("{k}/"), key) {
                    return Some(f);
                }
            }
        }
        None
    }
    let group = zones.iter().find_map(|z| find(&z.modules, "", key))?;
    let mut out = Vec::new();
    cells_in(
        group.children.get(1..).unwrap_or_default(),
        &format!("{key}/"),
        world,
        icons,
        icon_size,
        ui,
        &mut out,
    );
    out.extend(menu_cells(&[], None, icons, icon_size));
    Some(out)
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
            // (a bar-mode drawer never grows in place: its rest shows in the sub-bar)
            let bar = drawer && is_bar_drawer(m);
            let shown = if drawer && (bar || !open) {
                1
            } else {
                m.children.len()
            };
            let start = out.len();
            let children = &m.children[..shown.min(m.children.len())];
            let child_prefix = format!("{key}/");
            // waybar's transition-left-to-right = false: the drawer opens towards the left, so
            // the leader comes after the rest.
            let rtl = drawer
                && drawer_opt(m, "transition-left-to-right").and_then(toml::Value::as_bool)
                    == Some(false);
            let mut handle_at = start;
            if rtl && children.len() > 1 {
                let (leader, rest) = children.split_at(1);
                cells_in(rest, &child_prefix, world, icons, icon_size, ui, out);
                handle_at = out.len();
                cells_in(leader, &child_prefix, world, icons, icon_size, ui, out);
            } else {
                cells_in(children, &child_prefix, world, icons, icon_size, ui, out);
            }
            if drawer && let Some(handle) = out.get_mut(handle_at) {
                let collapse_after = drawer_opt(m, "collapse-after")
                    .and_then(|v| sliver_config::parse_duration(v).ok());
                handle.drawer = Some(Drawer {
                    group: key.clone(),
                    collapse_after,
                    bar,
                });
                handle.checked = open;
                if handle.touch == Touch::None {
                    handle.touch = Touch::Agent;
                }
            }
            let child_class = drawer.then(|| {
                drawer_opt(m, "children-class")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("drawer-child")
                    .to_owned()
            });
            for c in &mut out[start..] {
                c.classes
                    .push(format!("group-{}", m.id.name.as_deref().unwrap_or("")));
                if let Some(class) = &child_class {
                    c.classes.push(class.clone());
                }
            }
            continue;
        }
        // A published item (or a whole context) placed by the layout: `app/<provider>#<item>`
        // (api.md §3.1). It's styled, bound and formatted like any module.
        if m.id.kind == "app" {
            pinned_app(m, &key, world, icons, icon_size, out);
            continue;
        }
        if m.id.kind == "app-controls" || m.id.kind == "status-items" {
            let start = out.len();
            let p = &world.published;
            if m.id.kind == "app-controls" {
                if let Some(ctx) = &p.app {
                    let skip = |id: &str| p.is_pinned(&ctx.provider, id);
                    crate::published::cells(ctx, "app", true, icons, icon_size, &skip, out);
                }
            } else {
                for ctx in &p.status {
                    let skip = |id: &str| p.is_pinned(&ctx.provider, id);
                    crate::published::cells(ctx, "status", false, icons, icon_size, &skip, out);
                }
            }
            let class = if m.id.kind == "app-controls" {
                "app"
            } else {
                "status"
            };
            let placeholder_shown = out.len() == start && world.preview;
            if placeholder_shown {
                let label = if class == "app" {
                    "App controls"
                } else {
                    "Status items"
                };
                out.push(placeholder(m, &key, label, class));
            }
            // The placeholder's own classes and user bindings apply to every item in it
            // (a user `on-*` replaces the app's event unless `pass-through`, api.md §5).
            let on: Vec<(Gesture, Action)> =
                m.common.on.iter().map(|(g, a)| (*g, a.clone())).collect();
            for c in &mut out[start..] {
                c.classes.extend(m.classes.iter().cloned());
                c.on = on.clone();
                c.pass_through = m.common.pass_through;
                c.key = format!("{prefix}{}", c.key);
            }
            // `[modules."app/<provider>#<item>"]` applies here too (api.md §3.1).
            if !world.published.configured.is_empty() {
                let cells: Vec<Cell> = out.drain(start..).collect();
                for cell in cells {
                    let conf = world.published.configured.get(&cell.id);
                    let ctx = cell
                        .id
                        .strip_prefix("app/")
                        .and_then(|r| r.split('#').next())
                        .and_then(|p| world.published.find(p))
                        .map(|(c, _)| c);
                    match (conf, ctx) {
                        (Some(conf), Some(ctx)) => {
                            out.extend(decorate_published(conf, ctx, cell, icons, icon_size))
                        }
                        _ => out.push(cell),
                    }
                }
            }
            // A sized slot (`width` = px or fill): the items lay out inside it and a trailing
            // spacer reserves the rest. The agent measures a px slot and trims the spacer, or
            // drops the lowest-priority items if they don't fit (`Agent::fit_slots`).
            if !placeholder_shown && m.common.width != sliver_config::Width::Auto {
                let slot = key.clone();
                for c in &mut out[start..] {
                    c.slot = Some(slot.clone());
                }
                let mut spacer = placeholder(m, &format!("{slot}#slot"), "", class);
                spacer.kind = "slot".into();
                spacer.classes = vec!["slot".into(), class.into()];
                spacer.content = Content::Spacer;
                spacer.slot = Some(slot);
                out.push(spacer);
            }
            continue;
        }
        if m.id.kind == "emoji" && m.config.get("mode").and_then(|v| v.as_str()) == Some("strip") {
            emoji_strip(m, &key, world, &ui(&key).emoji, out);
            continue;
        }
        if m.id.kind == "privacy" {
            // One cell per kind in use (screen, camera, microphone), nothing while idle.
            for (i, (kind, _)) in world.session.capture.kinds().into_iter().enumerate() {
                let item_key = format!("{key}/{kind}");
                let state = ui(&item_key);
                if let Some(c) = cell(m, item_key, world, icons, icon_size, &state, Some(i)) {
                    out.push(c);
                }
            }
            continue;
        }
        if m.id.kind == "workspaces" || m.id.kind == "taskbar" {
            // Pinned apps first (launchers, or the running app's window), then the other windows.
            let mut shown = std::collections::HashSet::new();
            if m.id.kind == "taskbar" {
                for (pi, pin) in pinned_ids(m).iter().enumerate() {
                    let window = pinned_window(world, pin);
                    shown.extend(window);
                    out.push(pinned_cell(
                        m, &key, world, icons, icon_size, pi, pin, window,
                    ));
                }
            }
            for i in listed(m, world).into_iter().filter(|i| !shown.contains(i)) {
                let id = match m.id.kind.as_str() {
                    "workspaces" => world.desktop.workspaces[i].id.to_string(),
                    _ => world.desktop.windows[i].address.clone(),
                };
                let item_key = format!("{key}/{id}");
                let state = ui(&item_key);
                if let Some(c) = cell(m, item_key, world, icons, icon_size, &state, Some(i)) {
                    out.push(c);
                }
            }
            continue;
        }
        let state = ui(&key);
        if let Some(c) = cell(m, key, world, icons, icon_size, &state, None) {
            out.push(c);
        }
    }
}

/// A cell's gestures: user bindings first, then `actions = { on-x = "name" }`, then the module's
/// `defaults`. With a menu, its trigger (default: tap, or hold when the module also taps) opens
/// the sub-bar instead of whatever that gesture did.
fn bindings(
    m: &ModuleInstance,
    defaults: Vec<(Gesture, Action)>,
    menu: bool,
) -> Vec<(Gesture, Action)> {
    let c = &m.common;
    let mut on: Vec<(Gesture, Action)> = c.on.iter().map(|(g, a)| (*g, a.clone())).collect();
    for (g, name) in &c.actions {
        if !on.iter().any(|(x, _)| x == g) {
            on.push((*g, Action::ModuleAction(name.clone())));
        }
    }
    for (g, a) in defaults {
        if !on.iter().any(|(x, _)| *x == g) {
            on.push((g, a));
        }
    }
    if menu {
        let has_tap = on.iter().any(|(g, _)| *g == Gesture::Tap);
        let trigger = c
            .menu_trigger
            .unwrap_or(if has_tap { Gesture::Hold } else { Gesture::Tap });
        on.retain(|(g, _)| *g != trigger);
        on.push((trigger, Action::ModuleAction("menu:open".into())));
    }
    on
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
    /// A level (0..1): what a slider shows.
    level: Option<f32>,
    /// The agent sets `level` itself (the module action `set`). User modules set theirs through
    /// their own `on-drag`.
    settable: bool,
    /// Drawn as a slider at this value (0..1); set by [`as_slider`] from `level`.
    slider: Option<f32>,
    checked: bool,
    /// Bindings used for gestures the user didn't bind (`ModuleAction`s the agent runs).
    defaults: Vec<(Gesture, Action)>,
    /// Classes from the module's own output.
    extra_classes: Vec<String>,
    /// The text is already formatted (user modules): don't apply `format` again.
    raw: bool,
    /// Polled modules: redraw after this long.
    refresh: Option<std::time::Duration>,
    /// Workspaces/taskbar items: what tapping does.
    target: Option<sliver_desktop::control::Action>,
    timer_total: Option<std::time::Duration>,
    /// A sub-bar built from live data (the `language` picker), used when the config has no `menu`.
    menu: Vec<sliver_config::MenuEntry>,
    /// Shown when there is no icon (and no text), instead of an empty cell.
    fallback_text: Option<&'static str>,
    /// The `image` module: a picture (path or icon name) that fills the cell.
    image: Option<String>,
    /// `graph`: samples (oldest first, 0..=1) and how many fit.
    graph: Option<(Vec<f32>, usize)>,
    /// `cava`: the spectrum's levels, 0..=1.
    bars: Option<Vec<f32>>,
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
            level: None,
            settable: false,
            slider: None,
            checked: false,
            defaults: Vec::new(),
            extra_classes: Vec::new(),
            raw: false,
            refresh: None,
            target: None,
            timer_total: None,
            menu: Vec::new(),
            fallback_text: None,
            image: None,
            graph: None,
            bars: None,
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

/// `image` pictures are kept at most this tall: twice the 60 px bar, so scaling down stays sharp.
pub(crate) const IMAGE_MAX_HEIGHT: u32 = 120;

mod clock;
mod desktop;
mod emoji;
mod media;
mod menus;
mod spectrum;
mod system;
mod widgets;
mod world;

use clock::*;
use desktop::*;
use emoji::*;
pub use menus::*;
pub use spectrum::*;
use system::*;
pub use world::*;

/// A 0–100 reading as a percentage value.
fn pct(v: Option<u8>) -> Option<f64> {
    v.map(f64::from)
}

/// The module's spec for this world, or `None` to hide it (no data, nothing to show).
fn spec(m: &ModuleInstance, world: &World, ui: &UiState, item: Option<usize>) -> Option<Spec> {
    let sp = source(m, world, ui, item)?;
    Some(if is_slider(m) {
        as_slider(sp, m, ui)
    } else {
        sp
    })
}

/// Does the module draw as a slider? `slider = true`, which the resolver also sets for a module
/// written as waybar's `<type>/slider`.
fn is_slider(m: &ModuleInstance) -> bool {
    m.config
        .get("slider")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// The slider presentation, for any module with a [`Spec::level`]: no text, a drag sets the level,
/// and a tap no longer does the button's thing. A `collapsed = true` slider shows just its icon
/// until it's tapped open (like macOS's control strip).
fn as_slider(mut sp: Spec, m: &ModuleInstance, ui: &UiState) -> Spec {
    let Some(level) = sp.level else {
        return sp;
    };
    sp.default_format = "";
    sp.defaults.retain(|(g, _)| *g != Gesture::Tap);
    let collapsed = m
        .config
        .get("collapsed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if collapsed && !ui.expanded {
        return sp;
    }
    sp.slider = Some(level);
    if sp.settable {
        sp = sp.on(Gesture::Drag, "set");
    }
    sp
}

/// What the module shows, before any presentation.
fn source(m: &ModuleInstance, world: &World, ui: &UiState, item: Option<usize>) -> Option<Spec> {
    match m.id.kind.as_str() {
        "custom" if world.outputs.contains_key(&m.id.to_string()) => {
            widgets::custom_output(m, world, ui)
        }
        "key" | "custom" | "spacer" => Some(Spec::new(ValueMap::new(), "")),
        "graph" | "custom-graph" => widgets::graph(m, world, ui),
        "cava" => widgets::cava(m, world),
        "image" => widgets::image(m, world),
        "clock" => widgets::clock(m, world, ui),
        "battery" => system::battery(world),
        "backlight" | "kbd-backlight" => system::backlight(m, world),
        "audio" => system::audio(m, world),
        "mpris" => media::mpris(m, world, ui),
        "idle-inhibitor" => system::idle_inhibitor(world),
        "power-profiles" => system::power_profiles(world),
        "language" if world.session.input_method.is_some() => desktop::language_input_method(world),
        "language" => desktop::language(world),
        "window" => desktop::window(m, world),
        "mode" => desktop::mode(world),
        "cpu" | "memory" | "temperature" | "disk" | "load" => stat_spec(m, world),
        "network" => system::network(m, world),
        "bluetooth" => system::bluetooth(world),
        "emoji" => widgets::emoji(m),
        "timer" => widgets::timer(m, world, ui),
        "notifications" => system::notifications(world),
        "screenshot" => system::screenshot(),
        "systemd-failed-units" => system::failed_units(m, world),
        "lock" => system::lock(),
        "power" => system::power(),
        "dnd" => system::dnd(world),
        "dark-mode" => system::dark_mode(world),
        "privacy" => system::privacy(world, item),
        "workspaces" => desktop::workspaces(world, item),
        "taskbar" => desktop::taskbar(world, item),
        // No data source wired yet: show nothing rather than a broken placeholder.
        _ => None,
    }
}

fn cell(
    m: &ModuleInstance,
    key: String,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    ui: &UiState,
    item: Option<usize>,
) -> Option<Cell> {
    let c = &m.common;
    let mut sp = spec(m, world, ui, item)?;
    let next_change = if m.id.kind == "clock" {
        Some(next_clock_tick(
            world.now,
            c.format.as_deref().unwrap_or(CLOCK_DEFAULT),
        ))
    } else {
        sp.refresh
            .and_then(|d| chrono::Duration::from_std(d).ok())
            .map(|d| world.now + d)
    };
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
    } else if let Some((values, capacity)) = sp.graph.take() {
        // `states` still apply (on the latest value): `.warning`, `.critical`.
        let f = format_module(c, "", &st, sp.values.as_ref());
        classes.extend(f.classes);
        Content::Graph { values, capacity }
    } else if let Some(levels) = sp.bars.take() {
        Content::Bars(levels)
    } else if let Some(name) = sp.image.take() {
        // A missing or unreadable picture hides the module (it may not exist yet).
        Content::Image(icons.image(&name, IMAGE_MAX_HEIGHT)?)
    } else {
        // User module output is already formatted (`sliver_modules::view`): pass it through.
        let plain = sliver_config::Common::default();
        let fc = if sp.raw { &plain } else { c };
        let f = format_module(fc, sp.default_format, &st, sp.values.as_ref());
        if f.hidden {
            return None;
        }
        classes.extend(f.classes);
        classes.extend(sp.extra_classes.iter().cloned());
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
            (None, Some(i), true) => Content::icon(i),
            (None, Some(i), false) => Content::label(Some(i), f.text),
            (None, None, false) => Content::markup(f.text),
            // A window whose app has no icon: its name's first letters.
            (None, None, true) if m.id.kind == "taskbar" => {
                let name = world
                    .desktop
                    .windows
                    .get(item.unwrap_or(usize::MAX))
                    .map_or("", |w| w.class.as_str());
                Content::markup(sliver_render::markup::escape(
                    &name.chars().take(2).collect::<String>(),
                ))
            }
            (None, None, true) => sp.fallback_text.map_or(Content::blank(), Content::markup),
        }
    };
    let menu = if m.common.menu.is_empty() {
        std::mem::take(&mut sp.menu)
    } else {
        button_menu(&m.common.menu, sp.values.as_ref())
    };
    let on = bindings(m, sp.defaults, !menu.is_empty());
    let collapsed_slider = is_slider(m) && sp.level.is_some() && sp.slider.is_none();
    let mut touch = touch_kind(m, &on);
    if collapsed_slider {
        touch = Touch::Agent;
    }
    Some(Cell {
        key: key.clone(),
        id: m.id.to_string(),
        kind: m.id.kind.clone(),
        mic: m.config.get("node").and_then(|v| v.as_str()) == Some("source"),
        slider: is_slider(m),
        css_id: m.css_id.clone(),
        classes,
        width: width(c.width),
        content,
        touch,
        on,
        next_change,
        pressed: false,
        checked: sp.checked,
        // A collapsed slider opens like a drawer handle and closes on its own.
        disabled: false,
        foreground: None,
        background: None,
        published: None,
        pass_through: false,
        target: sp.target,
        timer_total: sp.timer_total,
        menu,
        slot: None,
        spectrum: spectrum(m, world),
        drawer: (collapsed_slider || is_slider(m) && ui.expanded && is_collapsible(m)).then(|| {
            Drawer {
                group: key,
                collapse_after: Some(std::time::Duration::from_secs(5)),
                bar: false,
            }
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

/// A module's configured `width` for the renderer.
fn width(w: sliver_config::Width) -> Width {
    match w {
        sliver_config::Width::Auto => Width::Auto,
        sliver_config::Width::Fixed(px) => Width::Fixed(px as f32),
        sliver_config::Width::Fill(n) => Width::Fill(n),
    }
}

/// An empty App controls / Status items placeholder, drawn only in previews. It takes the
/// module's `width`, so the settings editor's resize handle shows what it does.
/// The cells of a pinned `app/<provider>[#item]` module: the app's own cells (touches still go
/// to the app), with the module's classes, `on-*` bindings (`pass-through`), `width` and
/// `format` (over the item's values) applied on top. Nothing while it isn't published; an
/// outlined placeholder in the settings preview.
fn pinned_app(
    m: &ModuleInstance,
    key: &str,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    out: &mut Vec<Cell>,
) {
    let Some(provider) = m.id.name.as_deref() else {
        return;
    };
    let item = m.id.instance.as_deref();
    let Some((ctx, class)) = world.published.find(provider) else {
        if world.preview {
            // Short: the item id (`timer`), else the provider's last segment (`Pomodoro`).
            let label = m.id.instance.clone().unwrap_or_else(|| {
                let p = m.id.name.as_deref().unwrap_or("app");
                p.rsplit('.').next().unwrap_or(p).to_owned()
            });
            out.push(placeholder(m, key, &label, "app"));
        }
        return;
    };
    let start = out.len();
    crate::published::pinned_cells(ctx, item, class, icons, icon_size, out);
    let cells: Vec<Cell> = out.drain(start..).collect();
    for cell in cells {
        out.extend(decorate_published(m, ctx, cell, icons, icon_size));
    }
}

/// A published item's cell with a module's config on top: classes, `on-*` bindings
/// (`pass-through`), `width`, and `format`/`format-icons`/`states` over the item's values
/// (sliders keep their track). `None` when the format hides it.
fn decorate_published(
    m: &ModuleInstance,
    ctx: &crate::published::PubContext,
    mut cell: Cell,
    icons: &mut Icons,
    icon_size: u32,
) -> Option<Cell> {
    let c = &m.common;
    cell.classes.extend(m.classes.iter().cloned());
    if !c.on.is_empty() {
        cell.on = c.on.iter().map(|(g, a)| (*g, a.clone())).collect();
        cell.pass_through = c.pass_through;
    }
    if c.width != sliver_config::Width::Auto {
        cell.width = match c.width {
            sliver_config::Width::Auto => Width::Auto,
            sliver_config::Width::Fixed(px) => Width::Fixed(px as f32),
            sliver_config::Width::Fill(n) => Width::Fill(n),
        };
    }
    let it = cell.published.as_ref().and_then(|p| {
        let id = p.item.as_str();
        ctx.items.iter().find(|i| i.id == id).or_else(|| {
            ctx.items.iter().find_map(|i| match &i.kind {
                sliver_api::item::ItemKind::Group { items } => items.iter().find(|c| c.id == id),
                _ => None,
            })
        })
    });
    if (c.format.is_some() || c.format_alt.is_some() || c.format_icons.is_some())
        && let Some(it) = it
        && !matches!(cell.content, Content::Slider { .. })
    {
        let (values, pct) = crate::published::item_values(it);
        let st = State {
            percentage: pct,
            ..State::default()
        };
        let f = format_module(c, "{text}", &st, &values);
        if f.hidden {
            return None;
        }
        cell.classes.extend(f.classes);
        let icon = f
            .icon
            .map(|i| i.0)
            .or_else(|| c.icon.clone())
            .and_then(|n| {
                icons.get(
                    &n,
                    c.icon_theme.as_deref(),
                    c.icon_size.unwrap_or(icon_size),
                )
            })
            .or(match &cell.content {
                Content::Label { icon: Some(i), .. } => Some(i.clone()),
                _ => None,
            });
        cell.content = match (icon, f.text.is_empty()) {
            (Some(i), true) => Content::icon(i),
            (Some(i), false) => Content::label(Some(i), f.text),
            (None, false) => Content::markup(f.text),
            (None, true) => Content::blank(),
        };
    }
    Some(cell)
}

fn placeholder(m: &ModuleInstance, key: &str, label: &str, class: &str) -> Cell {
    Cell {
        key: key.to_owned(),
        id: m.id.to_string(),
        kind: m.id.kind.clone(),
        mic: false,
        slider: false,
        css_id: m.css_id.clone(),
        classes: vec!["placeholder".into(), class.into()],
        width: width(m.common.width),
        content: Content::text(label),
        touch: Touch::None,
        on: Vec::new(),
        next_change: None,
        pressed: false,
        checked: false,
        drawer: None,
        disabled: false,
        foreground: None,
        background: None,
        published: None,
        pass_through: false,
        target: None,
        timer_total: None,
        menu: Vec::new(),
        slot: None,
        spectrum: Vec::new(),
    }
}

/// Every module some layout places (group members included, each id once), with its config
/// block.
pub fn placed_modules(
    config: &sliver_config::Config,
) -> Vec<(sliver_config::ModuleId, toml::Table)> {
    use sliver_config::{ModuleId, ModuleRef};
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut todo: Vec<ModuleRef> = Vec::new();
    for name in config.layouts.keys() {
        if let Ok(zones) = config.layout_zones(name) {
            todo.extend(zones.into_iter().flat_map(|z| z.modules));
        }
    }
    while let Some(r) = todo.pop() {
        let id = r.id().clone();
        if !seen.insert(id.to_string()) {
            continue;
        }
        let block = config.module_block(&r);
        if id.kind == "group" {
            let members = block
                .get("modules")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            todo.extend(
                members
                    .iter()
                    .filter_map(|v| v.as_str())
                    .filter_map(|s| ModuleId::parse(s).ok())
                    .map(ModuleRef::Named),
            );
            continue;
        }
        out.push((id, block));
    }
    out
}

/// Does some layout place a module of this `kind`? (Sources that cost something to watch,
/// such as failed systemd units, start only then.)
pub fn uses_kind(config: &sliver_config::Config, kind: &str) -> bool {
    placed_modules(config).iter().any(|(id, _)| id.kind == kind)
}

/// Runtime specs for every `custom/*`, `image` and `graph` module with an `exec`, in any layout (including group
/// members), for the feeds' module runtime. Problems come back as messages.
pub fn user_module_specs(
    config: &sliver_config::Config,
) -> (Vec<sliver_modules::Spec>, Vec<String>) {
    let (mut specs, mut errors) = (Vec::new(), Vec::new());
    for (id, block) in placed_modules(config) {
        if !matches!(id.kind.as_str(), "custom" | "image" | "graph") || !block.contains_key("exec")
        {
            continue;
        }
        let inst = ModuleInstance {
            css_id: id.css_id(),
            id: id.clone(),
            classes: Vec::new(),
            common: sliver_config::Common::default(),
            config: block,
            children: Vec::new(),
        };
        match sliver_modules::Spec::from_instance(&inst) {
            Ok(s) => specs.push(s),
            Err(e) => errors.push(format!("{id}: {e}")),
        }
    }
    specs.sort_by(|a, b| a.key.cmp(&b.key));
    (specs, errors)
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn finds_exec_modules_everywhere() {
        let l = sliver_config::load_str(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["custom/a", "group/g", "custom/static"]
            [modules."group/g"]
            modules = ["custom/b"]
            [modules."custom/a"]
            exec = "echo a"
            [modules."custom/b"]
            exec = "echo b"
            interval = 5
            [modules."custom/static"]
            format = "hi"
            "#,
            std::path::Path::new("."),
        );
        let (specs, errors) = user_module_specs(&l.config);
        assert!(errors.is_empty(), "{errors:?}");
        let keys: Vec<&str> = specs.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["custom/a", "custom/b"]);
    }
}

#[cfg(test)]
mod tests;
