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

use crate::act::{Act, Do, MenuItem, UiChange, Verbs};
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
    /// The module type (`clock`, `audio`, …).
    pub kind: String,
    pub css_id: String,
    pub classes: Vec<String>,
    pub width: Width,
    pub content: Content,
    pub touch: Touch,
    /// Gesture bindings, for the agent's recognizer.
    pub on: Vec<(Gesture, Do)>,
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
    /// Part of a sized App controls / Status items slot (`width` = px or fill), by the slot's
    /// key: its published items, and the trailing `kind = "slot"` spacer that reserves the rest.
    pub slot: Option<String>,
    /// `spectrum = true`: the live spectrum's levels, drawn under the content. Empty: none.
    pub spectrum: Vec<f32>,
}

impl Cell {
    /// A cell showing `content` that does nothing yet.
    pub fn new(
        key: impl Into<String>,
        id: impl Into<String>,
        kind: impl Into<String>,
        css_id: impl Into<String>,
        content: Content,
    ) -> Cell {
        Cell {
            key: key.into(),
            id: id.into(),
            kind: kind.into(),
            css_id: css_id.into(),
            classes: Vec::new(),
            width: Width::Auto,
            content,
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
            slot: None,
            spectrum: Vec::new(),
        }
    }

    /// The binding for gesture `g`, if any.
    pub fn binding(&self, g: Gesture) -> Option<&Do> {
        self.on.iter().find(|(x, _)| *x == g).map(|(_, d)| d)
    }

    /// A tap here does `d` (handled by the agent).
    pub(crate) fn tapped(mut self, d: impl Into<Do>) -> Cell {
        self.on = vec![(Gesture::Tap, d.into())];
        self.touch = Touch::Agent;
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drawer {
    /// The group's key (for [`UiState::expanded`]).
    pub group: String,
    /// Close again this long after opening (`drawer.collapse-after`).
    pub collapse_after: Option<std::time::Duration>,
    pub opens: Opens,
}

/// Where a drawer's contents show when it opens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Opens {
    /// In place: the group's other modules join the bar.
    Inline,
    /// `mode = "bar"`: the group's other modules cover the bar as a sub-bar.
    SubBar,
    /// A `collapsed = true` slider: its track replaces its icon.
    Slider,
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
                    opens: if bar { Opens::SubBar } else { Opens::Inline },
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
            let on = user_bindings(m);
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
                let ui = ui(&item_key);
                let at = At {
                    m,
                    key: &item_key,
                    world,
                    ui: &ui,
                    item: Some(i),
                };
                out.extend(cell(at, icons, icon_size));
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
                let ui = ui(&item_key);
                let at = At {
                    m,
                    key: &item_key,
                    world,
                    ui: &ui,
                    item: Some(i),
                };
                out.extend(cell(at, icons, icon_size));
            }
            continue;
        }
        let ui = ui(&key);
        let at = At {
            m,
            key: &key,
            world,
            ui: &ui,
            item: None,
        };
        out.extend(cell(at, icons, icon_size));
    }
}

/// A cell's gestures: user bindings first, then `actions = { on-x = "name" }`, then the module's
/// default ones, all resolved against the module's `verbs`. With a `menu` verb, its trigger
/// (default: tap, or hold when the module also taps) opens the sub-bar instead of whatever that
/// gesture did.
fn bindings(
    m: &ModuleInstance,
    verbs: &[(&'static str, Do)],
    defaults: &[(Gesture, &'static str)],
) -> Vec<(Gesture, Do)> {
    let c = &m.common;
    let kind = m.id.kind.as_str();
    let verb = |name: &str| Do::bind(&Action::ModuleAction(name.into()), kind, verbs);
    let mut on: Vec<(Gesture, Do)> =
        c.on.iter()
            .map(|(g, a)| (*g, Do::bind(a, kind, verbs)))
            .collect();
    let named = c.actions.iter().map(|(g, n)| (*g, n.as_str()));
    for (g, name) in named.chain(defaults.iter().map(|(g, n)| (*g, *n))) {
        if !on.iter().any(|(x, _)| *x == g) {
            on.push((g, verb(name)));
        }
    }
    if verbs.iter().any(|(n, _)| *n == "menu") {
        let has_tap = on.iter().any(|(g, _)| *g == Gesture::Tap);
        let trigger = c
            .menu_trigger
            .unwrap_or(if has_tap { Gesture::Hold } else { Gesture::Tap });
        on.retain(|(g, _)| *g != trigger);
        on.push((trigger, verb("menu")));
    }
    on
}

/// Just the user's `on-*` bindings: what a placeholder (or a configured published item) adds
/// to the app's own cells.
fn user_bindings(m: &ModuleInstance) -> Vec<(Gesture, Do)> {
    let kind = m.id.kind.as_str();
    m.common
        .on
        .iter()
        .map(|(g, a)| (*g, Do::bind(a, kind, &[])))
        .collect()
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
    /// A level (0..1): what a slider shows. A `set` verb lets a drag change it.
    level: Option<f32>,
    /// Drawn as a slider at this value (0..1); set by [`as_slider`] from `level`.
    slider: Option<f32>,
    checked: bool,
    /// What the module's names mean ([`crate::act`]).
    verbs: Verbs,
    /// Gestures bound to verbs unless the user bound them.
    defaults: Vec<(Gesture, &'static str)>,
    /// Classes from the module's own output.
    extra_classes: Vec<String>,
    /// The text is already formatted (user modules): don't apply `format` again.
    raw: bool,
    /// Polled modules: redraw after this long.
    refresh: Option<std::time::Duration>,
    /// A sub-bar built from live data (the `language` picker), used when the config has no `menu`.
    menu: Vec<MenuItem>,
    /// Shown when there is no icon (and no text), instead of an empty cell.
    fallback_text: Option<String>,
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
            slider: None,
            checked: false,
            verbs: Vec::new(),
            defaults: Vec::new(),
            extra_classes: Vec::new(),
            raw: false,
            refresh: None,
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

    /// The module's `name` means `d`.
    fn verb(mut self, name: &'static str, d: impl Into<Do>) -> Spec {
        self.verbs.retain(|(n, _)| *n != name);
        self.verbs.push((name, d.into()));
        self
    }

    /// Gesture `g` does the verb `name` unless the user bound it.
    fn on(mut self, g: Gesture, name: &'static str) -> Spec {
        self.defaults.retain(|(x, _)| *x != g);
        self.defaults.push((g, name));
        self
    }

    /// [`Spec::verb`] and [`Spec::on`] at once.
    fn does(self, g: Gesture, name: &'static str, d: impl Into<Do>) -> Spec {
        self.verb(name, d).on(g, name)
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

/// Where a module is drawn, for its spec: the module, its cell's key, the world, its state, and
/// which item of a multi-item module (workspaces, taskbar, privacy) this cell is.
struct At<'a> {
    m: &'a ModuleInstance,
    key: &'a str,
    world: &'a World,
    ui: &'a UiState,
    item: Option<usize>,
}

/// The module's spec for this world, or `None` to hide it (no data, nothing to show).
fn spec(at: &At) -> Option<Spec> {
    let sp = source(at)?;
    Some(if is_slider(at.m) {
        as_slider(sp, at.m, at.ui)
    } else {
        sp
    })
}

/// Does the module draw as a slider? `slider = true`, which the resolver also sets for a module
/// written as waybar's `<type>/slider`.
fn is_slider(m: &ModuleInstance) -> bool {
    flag(m, "slider")
}

/// `collapsed = true`: a slider shows just its icon until it's tapped open.
fn is_collapsible(m: &ModuleInstance) -> bool {
    flag(m, "collapsed")
}

fn flag(m: &ModuleInstance, key: &str) -> bool {
    m.config.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// The slider presentation, for any module with a [`Spec::level`]: no text, a drag sets the level
/// (when the module has a `set` verb), and a tap no longer does the button's thing. A collapsed
/// slider shows just its icon until it's tapped open (like macOS's control strip).
fn as_slider(mut sp: Spec, m: &ModuleInstance, ui: &UiState) -> Spec {
    let Some(level) = sp.level else {
        return sp;
    };
    sp.default_format = "";
    sp.defaults.retain(|(g, _)| *g != Gesture::Tap);
    if is_collapsible(m) && !ui.expanded {
        return sp;
    }
    sp.slider = Some(level);
    if sp.verbs.iter().any(|(n, _)| *n == "set") {
        sp = sp.on(Gesture::Drag, "set");
    }
    sp
}

/// What the module shows, before any presentation.
fn source(at: &At) -> Option<Spec> {
    let (m, world, ui, item) = (at.m, at.world, at.ui, at.item);
    match m.id.kind.as_str() {
        "custom" if world.outputs.contains_key(&m.id.to_string()) => {
            widgets::custom_output(m, world, ui)
        }
        "key" | "custom" | "spacer" => Some(Spec::new(ValueMap::new(), "")),
        "graph" | "custom-graph" => widgets::graph(m, world, ui),
        "cava" => widgets::cava(m, world),
        "image" => widgets::image(m, world),
        "clock" => widgets::clock(at),
        "battery" => system::battery(world),
        "backlight" | "kbd-backlight" => system::backlight(m, world),
        "audio" => system::audio(m, world),
        "mpris" => media::mpris(at),
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
        "timer" => widgets::timer(at),
        "notifications" => system::notifications(world),
        "screenshot" => system::screenshot(),
        "systemd-failed-units" => system::failed_units(m, world),
        "lock" => system::lock(),
        "power" => system::power(at.key),
        "dnd" => system::dnd(world),
        "dark-mode" => system::dark_mode(world),
        "privacy" => system::privacy(world, item),
        "workspaces" => desktop::workspaces(world, item),
        "taskbar" => desktop::taskbar(world, item),
        // No data source wired yet: show nothing rather than a broken placeholder.
        _ => None,
    }
}

/// A formatted label: the one step every module's (and configured published item's) text goes
/// through. waybar's `format`/`format-icons`/`states` over `values`, then the icon: the format's,
/// else the module's `icon`, else `fallback` (a name looked up the same way). `None` when the
/// format hides the cell.
pub(crate) struct Shown {
    pub icon: Option<sliver_render::Icon>,
    pub text: String,
    pub classes: Vec<String>,
}

pub(crate) fn present(
    c: &sliver_config::Common,
    default_format: &str,
    st: &State,
    values: &dyn Values,
    fallback: Option<&str>,
    icons: &mut Icons,
    icon_size: u32,
) -> Option<Shown> {
    let f = format_module(c, default_format, st, values);
    if f.hidden {
        return None;
    }
    let name = f.icon.map(|i| i.0).or_else(|| c.icon.clone());
    let icon = name
        .as_deref()
        .or(fallback)
        .and_then(|n| icons.get(n, c.icon_theme.as_deref(), c.icon_size.unwrap_or(icon_size)));
    Some(Shown {
        icon,
        text: f.text,
        classes: f.classes,
    })
}

fn cell(at: At, icons: &mut Icons, icon_size: u32) -> Option<Cell> {
    let (m, world, ui) = (at.m, at.world, at.ui);
    let c = &m.common;
    let mut sp = spec(&at)?;
    let key = at.key.to_owned();
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
        let f = present(
            fc,
            sp.default_format,
            &st,
            sp.values.as_ref(),
            sp.default_icon.as_deref(),
            icons,
            icon_size,
        )?;
        classes.extend(f.classes);
        classes.extend(sp.extra_classes.iter().cloned());
        match (sp.slider, f.icon, f.text.is_empty()) {
            (Some(value), icon, _) => Content::Slider { icon, value },
            (None, None, true) => Content::text(sp.fallback_text.as_deref().unwrap_or_default()),
            // Format strings are Pango-style markup, like waybar (values from outside are escaped).
            (None, icon, _) => Content::label(icon, f.text),
        }
    };
    // Every module can be opened or closed (`expand`), and one with a `menu` opens it.
    let menu = if c.menu.is_empty() {
        std::mem::take(&mut sp.menu)
    } else {
        button_menu(&c.menu, sp.values.as_ref(), &m.id.kind, &sp.verbs)
    };
    let mut verbs = std::mem::take(&mut sp.verbs);
    verbs.push((
        "expand",
        Do::Agent(Act::Ui {
            key: key.clone(),
            change: UiChange::Expand,
        }),
    ));
    if !menu.is_empty() {
        verbs.push((
            "menu",
            Do::Agent(Act::Menu {
                key: key.clone(),
                items: menu,
            }),
        ));
    }
    let on = bindings(m, &verbs, &sp.defaults);
    let collapsed = is_slider(m) && is_collapsible(m) && sp.level.is_some();
    let mut touch = touch_kind(m, &on);
    if collapsed && sp.slider.is_none() {
        touch = Touch::Agent;
    }
    let mut cell = Cell::new(
        key.clone(),
        m.id.to_string(),
        m.id.kind.clone(),
        m.css_id.clone(),
        content,
    );
    cell.classes = classes;
    cell.width = width(c.width);
    cell.touch = touch;
    cell.on = on;
    cell.next_change = next_change;
    cell.checked = sp.checked;
    cell.spectrum = spectrum(m, world);
    // A collapsed slider opens like a drawer handle and closes on its own.
    cell.drawer = collapsed.then(|| Drawer {
        group: key,
        collapse_after: Some(std::time::Duration::from_secs(5)),
        opens: Opens::Slider,
    });
    Some(cell)
}

/// Key regions are the fast path: sliverd presses and releases the keys itself, so they work
/// even if the agent hangs. Only plain "tap = key, fire on press" bindings qualify (repeat is
/// the kernel's job once the key is down).
fn touch_kind(m: &ModuleInstance, on: &[(Gesture, Do)]) -> Touch {
    let press = m.common.fire_on == Some(FireOn::Press);
    match on {
        [(Gesture::Tap, Do::Run(Action::Key(k)))] if press => Touch::Keys(vec![k.code()]),
        [(Gesture::Tap, Do::Run(Action::Keys(ks)))] if press => {
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
        cell.on = user_bindings(m);
        cell.pass_through = c.pass_through;
    }
    if c.width != sliver_config::Width::Auto {
        cell.width = width(c.width);
    }
    let it = cell.published.as_ref().and_then(|p| ctx.item(&p.item));
    if (c.format.is_some() || c.format_alt.is_some() || c.format_icons.is_some())
        && let Some(it) = it
        && !matches!(cell.content, Content::Slider { .. })
    {
        let (values, pct) = crate::published::item_values(it);
        let st = State {
            percentage: pct,
            ..State::default()
        };
        let f = present(c, "{text}", &st, &values, None, icons, icon_size)?;
        cell.classes.extend(f.classes);
        let own = match &cell.content {
            Content::Label { icon, .. } => icon.clone(),
            _ => None,
        };
        cell.content = Content::label(f.icon.or(own), f.text);
    }
    Some(cell)
}

/// An empty App controls / Status items placeholder, drawn only in previews. It takes the
/// module's `width`, so the settings editor's resize handle shows what it does.
fn placeholder(m: &ModuleInstance, key: &str, label: &str, class: &str) -> Cell {
    let mut c = Cell::new(
        key,
        m.id.to_string(),
        m.id.kind.clone(),
        m.css_id.clone(),
        Content::text(label),
    );
    c.classes = vec!["placeholder".into(), class.into()];
    c.width = width(m.common.width);
    c
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
