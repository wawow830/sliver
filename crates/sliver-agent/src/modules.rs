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
use std::collections::HashMap;

use sliver_config::Context;
use sliver_desktop::DesktopState;
use sliver_modules::Output;
use sliver_render::{Content, Width};
use sliver_session::{BatteryState, SessionState};

use crate::icons::Icons;

/// The emoji picker's recents, shared by the world (drawing) and the agent (picking).
#[derive(Clone, Default)]
pub struct EmojiHandle(std::sync::Arc<std::sync::Mutex<sliver_emoji::Recents>>);

impl std::fmt::Debug for EmojiHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmojiHandle")
    }
}

impl EmojiHandle {
    /// Recents saved under `$XDG_STATE_HOME/sliver/emoji-recents`.
    pub fn persistent() -> EmojiHandle {
        let path = sliver_emoji::Recents::default_path(|k| std::env::var(k).ok());
        let r = path.map_or_else(sliver_emoji::Recents::new, sliver_emoji::Recents::at);
        EmojiHandle(std::sync::Arc::new(std::sync::Mutex::new(r)))
    }

    pub fn items(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items()
            .to_vec()
    }

    pub fn push(&self, text: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    }

    /// `e` in the skin tone last picked for it (or plain).
    pub fn preferred(&self, e: &sliver_emoji::Emoji) -> String {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .preferred(e)
    }
}

/// Shared system-statistics sampler (CPU usage needs the previous sample, so one instance lives
/// as long as the agent). Cheap to clone; reads files only when a stats module asks.
#[derive(Clone, Default)]
pub struct Stats(std::sync::Arc<std::sync::Mutex<sliver_stats::Sampler>>);

impl std::fmt::Debug for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Stats")
    }
}

impl Stats {
    fn with<R>(&self, f: impl FnOnce(&mut sliver_stats::Sampler) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// A sampler reading these roots instead of `/proc` and `/sys` (tests).
    pub fn with_roots(
        proc_root: impl Into<std::path::PathBuf>,
        sys_root: impl Into<std::path::PathBuf>,
    ) -> Stats {
        Stats(std::sync::Arc::new(std::sync::Mutex::new(
            sliver_stats::Sampler::with_roots(proc_root, sys_root),
        )))
    }
}

/// Everything modules and rules can read.
#[derive(Debug, Clone)]
pub struct World {
    pub now: DateTime<Local>,
    pub session: SessionState,
    pub desktop: DesktopState,
    /// Rule context from the feeds (compositor + session keys); the agent adds its own.
    pub context: Context,
    /// Latest output of each user module (`custom/*` with `exec`), by module key.
    pub outputs: HashMap<String, Output>,
    /// What apps publish for the App controls / Status items placeholders.
    pub published: crate::published::Published,
    /// The settings app's editor: show empty placeholders as outlined `.placeholder` cells.
    pub preview: bool,
    /// cpu, memory, load, temperature, disk.
    pub stats: Stats,
    /// Recently used emoji (the picker's first tab).
    pub emoji: EmojiHandle,
    /// Local file of the current player's album art (fetched by the feeds).
    pub media_art: Option<std::path::PathBuf>,
    /// Installed applications, for taskbar icons.
    pub apps: std::sync::Arc<sliver_icons::desktop::DesktopEntries>,
}

impl World {
    /// A world with no session or desktop data (headless previews, tests).
    pub fn at(now: DateTime<Local>) -> World {
        World {
            now,
            session: SessionState::default(),
            desktop: DesktopState::default(),
            context: Context::new(),
            outputs: HashMap::new(),
            published: Default::default(),
            preview: false,
            stats: Stats::default(),
            apps: Default::default(),
            emoji: Default::default(),
            media_art: None,
        }
    }

    /// Plausible sample data for previews (the settings editor), so data modules show
    /// something instead of hiding: battery, volume, brightness, now playing, focused window.
    pub fn sample(now: DateTime<Local>) -> World {
        use sliver_session::{Media, PlaybackStatus};
        let mut w = World::at(now);
        w.preview = true;
        let s = &mut w.session;
        s.power.on_battery = Some(true);
        s.power.battery_pct = Some(76);
        s.power.battery_state = BatteryState::Discharging;
        s.power.profile = Some("balanced".into());
        s.audio.volume_pct = Some(40);
        s.audio.muted = Some(false);
        s.audio.mic_volume_pct = Some(80);
        s.audio.mic_muted = Some(false);
        s.brightness.screen_pct = Some(70);
        s.brightness.keyboard_pct = Some(30);
        s.media = Some(Media {
            player: "org.mpris.MediaPlayer2.preview".into(),
            identity: "Music".into(),
            status: PlaybackStatus::Playing,
            title: "Song Title".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            ..Media::default()
        });
        let d = &mut w.desktop;
        d.connected = true;
        d.keyboard_layout = Some("English (US)".into());
        d.window = Some(sliver_desktop::Window {
            class: "firefox".into(),
            title: "Mozilla Firefox".into(),
            ..Default::default()
        });
        w
    }
}

/// Per-module UI state that survives re-resolution, keyed by [`Cell::key`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiState {
    /// `format-alt` toggled on.
    pub show_alt: bool,
    /// A drawer group is open.
    pub expanded: bool,
    /// `timer` modules: the countdown.
    pub timer: Option<TimerState>,
    /// `emoji` strips: which tab and how far scrolled.
    pub emoji: EmojiUi,
    /// `mpris`: the player the user picked (hold), instead of the most recently active one.
    pub player: Option<String>,
}

/// Where an emoji strip is: tab 0 = recents, 1..=9 = the Unicode groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmojiUi {
    pub tab: usize,
    pub offset: usize,
    /// Choosing a skin tone for this emoji (its plain text): the page shows its variants.
    pub tones: Option<String>,
    /// Searching: the letters typed so far (`Some("")` = search is open and empty).
    pub search: Option<String>,
}

/// A countdown that survives re-resolution (it lives in [`UiState`], keyed by the cell).
#[derive(Debug, Clone, PartialEq)]
pub struct TimerState {
    /// Time left when it was last started, paused or created.
    pub remaining: std::time::Duration,
    /// Wall-clock start of the current run; `None` while paused or finished.
    pub running_since: Option<DateTime<Local>>,
    pub finished: bool,
    /// Bumped on every start/pause so a stale "done" deadline is ignored.
    pub generation: u64,
}

impl TimerState {
    pub fn new(total: std::time::Duration) -> TimerState {
        TimerState {
            remaining: total,
            running_since: None,
            finished: false,
            generation: 0,
        }
    }

    /// What is left at `now`.
    pub fn left(&self, now: DateTime<Local>) -> std::time::Duration {
        match self.running_since {
            Some(t) => self
                .remaining
                .saturating_sub((now - t).to_std().unwrap_or_default()),
            None => self.remaining,
        }
    }
}

/// `MM:SS`, or `H:MM:SS` from an hour up.
pub fn clock_text(d: std::time::Duration) -> String {
    let s = d.as_secs() + u64::from(d.subsec_nanos() > 0); // round up: 0:00 means done
    match s {
        0..=3599 => format!("{:02}:{:02}", s / 60, s % 60),
        _ => format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60),
    }
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
    /// The module id (`custom/vpn#work`), as the user module runtime knows it.
    pub id: String,
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
    m.config
        .get("drawer")
        .and_then(|d| d.get("mode"))
        .and_then(|v| v.as_str())
        == Some("bar")
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
    out.extend(menu_cells(&[], icons, icon_size));
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
                    bar,
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

/// The emoji picker as a row of cells: group tabs, one page of emoji, a close button.
/// Tap an emoji to type it, tap a tab to switch group, swipe to page, ✕ to close the layer.
/// Hold an emoji that has skin tones to choose one: the page becomes ‹, the plain emoji and its
/// five tones. Group pages show each emoji in the tone last picked for it (from recents).
fn emoji_strip(m: &ModuleInstance, key: &str, world: &World, ui: &EmojiUi, out: &mut Vec<Cell>) {
    use sliver_emoji::Group;
    let cols = m
        .config
        .get("columns")
        .and_then(|v| v.as_integer())
        .map_or(14, |n| n.clamp(4, 40) as usize);
    let insert = m
        .config
        .get("insert")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let recents = world.emoji.items();
    // An empty recents tab shows the first group rather than a blank bar.
    let tab = if ui.tab == 0 && recents.is_empty() {
        1
    } else {
        ui.tab.min(Group::ALL.len())
    };
    // (text to show and type, the plain emoji when it has tones)
    let items: Vec<(String, Option<&'static str>)> = if tab == 0 {
        recents
            .iter()
            .map(|t| {
                let fam = sliver_emoji::family(t).filter(|e| e.tones.is_some());
                (t.clone(), fam.map(|e| e.text))
            })
            .collect()
    } else {
        sliver_emoji::in_group(Group::ALL[tab - 1])
            .map(|e| {
                let has = e.tones.is_some();
                (world.emoji.preferred(e), has.then_some(e.text))
            })
            .collect()
    };
    let offset = ui.offset.min(items.len().saturating_sub(cols));
    let bare = |k: String, text: &str, w: f32, classes: &[&str], on: Vec<(Gesture, Action)>| Cell {
        key: k,
        id: m.id.to_string(),
        kind: "emoji".into(),
        mic: false,
        css_id: m.css_id.clone(),
        classes: classes
            .iter()
            .map(|c| c.to_string())
            .chain(m.classes.iter().cloned())
            .collect(),
        width: Width::Fixed(w),
        content: Content::Markup(sliver_render::markup::escape(text)),
        touch: Touch::Agent,
        on,
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
    };
    let act = |name: String| Action::ModuleAction(name);
    if let Some(query) = ui.search.as_ref().filter(|_| ui.tones.is_none()) {
        search_strip(m, key, world, ui, query, insert, &bare, out);
        return;
    }
    for t in 0..=Group::ALL.len() {
        let icon = if t == 0 {
            "🕘"
        } else {
            Group::ALL[t - 1].icon()
        };
        let mut c = bare(
            format!("{key}/tab{t}"),
            icon,
            52.0,
            &["emoji-tab"],
            vec![(Gesture::Tap, act(format!("tab:{t}")))],
        );
        c.checked = t == tab;
        out.push(c);
    }
    out.push(bare(
        format!("{key}/search"),
        "🔍",
        52.0,
        &["emoji-tab", "emoji-search"],
        vec![(Gesture::Tap, act("search".into()))],
    ));
    let tones = ui
        .tones
        .as_deref()
        .and_then(sliver_emoji::family)
        .and_then(|e| Some((e.text, e.tones?)));
    if let Some((plain, variants)) = tones {
        out.push(bare(
            format!("{key}/back"),
            "‹",
            52.0,
            &["emoji-back"],
            vec![(Gesture::Tap, act("tones-close".into()))],
        ));
        for (i, text) in std::iter::once(plain).chain(variants).enumerate() {
            out.push(bare(
                format!("{key}/t{i}"),
                text,
                76.0,
                &["emoji", "emoji-tone"],
                vec![(Gesture::Tap, act(format!("pick:{insert}:{text}")))],
            ));
        }
    } else {
        for (i, (text, plain)) in items.iter().enumerate().skip(offset).take(cols) {
            let mut on = vec![
                (Gesture::Tap, act(format!("pick:{insert}:{text}"))),
                (Gesture::SwipeLeft, act(format!("page:{}", cols))),
                (Gesture::SwipeRight, act(format!("page:-{}", cols))),
            ];
            if let Some(p) = plain {
                on.push((Gesture::Hold, act(format!("tones:{p}"))));
            }
            out.push(bare(format!("{key}/e{i}"), text, 76.0, &["emoji"], on));
        }
    }
    out.push(bare(
        format!("{key}/close"),
        "✕",
        52.0,
        &["emoji-close"],
        vec![(Gesture::Tap, act("close".into()))],
    ));
}

/// How many results fit next to the keys.
const SEARCH_RESULTS: usize = 6;

type Bare<'a> = dyn Fn(String, &str, f32, &[&str], Vec<(Gesture, Action)>) -> Cell + 'a;

/// Emoji search: no keyboard is grabbed. A row of letters (and a space) builds the query on the
/// bar itself; the matches (names, word prefixes) sit at the right, swipe to page. ⌫ erases,
/// tapping the query clears it, ✕ goes back to the groups.
#[allow(clippy::too_many_arguments)]
fn search_strip(
    m: &ModuleInstance,
    key: &str,
    world: &World,
    ui: &EmojiUi,
    query: &str,
    insert: &str,
    bare: &Bare<'_>,
    out: &mut Vec<Cell>,
) {
    let act = |name: String| Action::ModuleAction(name);
    let shown = if query.is_empty() {
        "🔍".to_string()
    } else {
        format!("🔍 {query}")
    };
    out.push(bare(
        format!("{key}/query"),
        &shown,
        150.0,
        &["emoji-query"],
        vec![(Gesture::Tap, act("s-clear".into()))],
    ));
    for c in "abcdefghijklmnopqrstuvwxyz ".chars() {
        let label = if c == ' ' {
            "␣".to_string()
        } else {
            c.to_string()
        };
        out.push(bare(
            format!("{key}/k{}", if c == ' ' { '_' } else { c }),
            &label,
            36.0,
            &["emoji-key"],
            vec![(Gesture::Tap, act(format!("s:{c}")))],
        ));
    }
    out.push(bare(
        format!("{key}/back"),
        "⌫",
        52.0,
        &["emoji-key", "emoji-back"],
        vec![(Gesture::Tap, act("s-back".into()))],
    ));
    let hits = sliver_emoji::search(query, 120);
    let offset = ui.offset.min(hits.len().saturating_sub(SEARCH_RESULTS));
    for (i, e) in hits.iter().enumerate().skip(offset).take(SEARCH_RESULTS) {
        let text = world.emoji.preferred(e);
        let mut on = vec![
            (Gesture::Tap, act(format!("pick:{insert}:{text}"))),
            (Gesture::SwipeLeft, act(format!("page:{SEARCH_RESULTS}"))),
            (Gesture::SwipeRight, act(format!("page:-{SEARCH_RESULTS}"))),
        ];
        if e.tones.is_some() {
            on.push((Gesture::Hold, act(format!("tones:{}", e.text))));
        }
        out.push(bare(format!("{key}/e{i}"), &text, 64.0, &["emoji"], on));
    }
    if !query.is_empty() && hits.is_empty() {
        out.push(bare(
            format!("{key}/none"),
            "no match",
            140.0,
            &["emoji-none"],
            Vec::new(),
        ));
    }
    let _ = m;
    out.push(bare(
        format!("{key}/close"),
        "✕",
        52.0,
        &["emoji-close"],
        vec![(Gesture::Tap, act("s-close".into()))],
    ));
}

/// A logind `Manager` call on the system bus.
fn logind(method: &str, args: Vec<toml::Value>) -> Action {
    Action::DBus(sliver_config::DBusCall {
        system_bus: true,
        dest: "org.freedesktop.login1".into(),
        path: "/org/freedesktop/login1".into(),
        interface: "org.freedesktop.login1.Manager".into(),
        method: method.into(),
        args,
    })
}

/// The `power` module's sub-bars: `dangerous` = the long-press one (reboot, power off).
pub fn power_menu(dangerous: bool) -> Vec<sliver_config::MenuEntry> {
    let entry = |label: &str, icon: &str, on_tap: Action| sliver_config::MenuEntry {
        label: label.into(),
        icon: Some(icon.into()),
        on_tap,
    };
    let interactive = vec![toml::Value::Boolean(false)];
    if dangerous {
        vec![
            entry(
                "Reboot",
                "system-reboot-symbolic",
                logind("Reboot", interactive.clone()),
            ),
            entry(
                "Power off",
                "system-shutdown-symbolic",
                logind("PowerOff", interactive),
            ),
        ]
    } else {
        vec![
            entry("Lock", "system-lock-screen-symbolic", lock_action()),
            entry(
                "Suspend",
                "media-playback-pause-symbolic",
                logind("Suspend", interactive),
            ),
        ]
    }
}

/// Lock this session the way `loginctl lock-session` does: logind tells the locker.
fn lock_action() -> Action {
    Action::DBus(sliver_config::DBusCall {
        system_bus: true,
        dest: "org.freedesktop.login1".into(),
        path: "/org/freedesktop/login1/session/auto".into(),
        interface: "org.freedesktop.login1.Session".into(),
        method: "Lock".into(),
        args: Vec::new(),
    })
}

/// The portal's interactive screenshot (the user picks area/window in the portal's own UI).
fn screenshot_action() -> Action {
    let mut opts = toml::Table::new();
    opts.insert("interactive".into(), toml::Value::Boolean(true));
    Action::DBus(sliver_config::DBusCall {
        system_bus: false,
        dest: "org.freedesktop.portal.Desktop".into(),
        path: "/org/freedesktop/portal/desktop".into(),
        interface: "org.freedesktop.portal.Screenshot".into(),
        method: "Screenshot".into(),
        args: vec![toml::Value::String(String::new()), toml::Value::Table(opts)],
    })
}

/// What the `screenshot` and `lock` modules do on tap.
pub fn oneshot_action(name: &str) -> Option<Action> {
    match name {
        "screenshot" => Some(screenshot_action()),
        "lock" => Some(lock_action()),
        _ => None,
    }
}

/// The cells of an open `menu` sub-bar: one per entry (label and/or icon), then a ✕.
pub fn menu_cells(
    entries: &[sliver_config::MenuEntry],
    icons: &mut Icons,
    icon_size: u32,
) -> Vec<Cell> {
    let cell = |key: String, content: Content, width: Width, classes: &[&str], tap: String| Cell {
        key,
        id: "menu".into(),
        kind: "menu".into(),
        mic: false,
        css_id: "menu".into(),
        classes: classes.iter().map(|c| c.to_string()).collect(),
        width,
        content,
        touch: Touch::Agent,
        on: vec![(Gesture::Tap, Action::ModuleAction(tap))],
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
    };
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let icon = e
            .icon
            .as_deref()
            .and_then(|n| icons.get(n, None, icon_size));
        let label = sliver_render::markup::escape(&e.label);
        let content = match (icon, label.is_empty()) {
            (Some(ic), false) => Content::IconMarkup(ic, label),
            (Some(ic), true) => Content::Icon(ic),
            (None, _) => Content::Markup(label),
        };
        out.push(cell(
            format!("menu/{i}"),
            content,
            Width::Fill(1),
            &["menu-item"],
            format!("menu:pick:{i}"),
        ));
    }
    let x = icons
        .get("window-close-symbolic", None, icon_size)
        .map_or_else(|| Content::Markup("✕".into()), Content::Icon);
    out.push(cell(
        "menu/close".into(),
        x,
        Width::Fixed(72.0),
        &["menu-close"],
        "menu:close".into(),
    ));
    out
}

/// `pinned = ["org.mozilla.firefox", "kitty"]`: desktop ids (with or without `.desktop`).
fn pinned_ids(m: &ModuleInstance) -> Vec<String> {
    m.config
        .get("pinned")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(|s| s.strip_suffix(".desktop").unwrap_or(s).to_string())
        .collect()
}

/// The first window (any workspace) that belongs to the pinned app `id`.
fn pinned_window(world: &World, id: &str) -> Option<usize> {
    world.desktop.windows.iter().position(|w| {
        world
            .apps
            .find_by_app_id(&w.class)
            .is_some_and(|e| e.id == id)
    })
}

/// A launcher for a pinned app: tap focuses its window when it is running, else starts it
/// (`gtk-launch`); hold closes the running window.
#[allow(clippy::too_many_arguments)]
fn pinned_cell(
    m: &ModuleInstance,
    key: &str,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    index: usize,
    id: &str,
    window: Option<usize>,
) -> Cell {
    let entry = world.apps.get(id);
    let icon = entry
        .and_then(|e| e.icon.as_deref())
        .or(Some(id))
        .and_then(|n| {
            icons.get(
                n,
                m.common.icon_theme.as_deref(),
                m.common.icon_size.unwrap_or(icon_size),
            )
        });
    let name = entry.map_or(id, |e| e.name.as_str());
    let content = icon.map_or_else(
        || {
            Content::Markup(sliver_render::markup::escape(
                &name.chars().take(2).collect::<String>(),
            ))
        },
        Content::Icon,
    );
    let win = window.map(|i| &world.desktop.windows[i]);
    let target = win.map(|w| sliver_desktop::control::Action::FocusWindow {
        address: w.address.clone(),
    });
    let mut classes = vec!["pinned".to_string()];
    classes.push(if win.is_some() { "running" } else { "idle" }.to_string());
    if win.is_some_and(|w| w.focused) {
        classes.push("focused".into());
    }
    if win.is_some_and(|w| w.urgent) {
        classes.push("urgent".into());
    }
    classes.extend(m.classes.iter().cloned());
    let tap = if win.is_some() {
        Action::ModuleAction("focus".into())
    } else {
        Action::Open(id.to_string())
    };
    let mut on = vec![(Gesture::Tap, tap)];
    if win.is_some() {
        on.push((Gesture::Hold, Action::ModuleAction("close".into())));
    }
    Cell {
        key: format!("{key}/pin{index}"),
        id: m.id.to_string(),
        kind: "taskbar".into(),
        mic: false,
        css_id: m.css_id.clone(),
        classes,
        width: Width::Auto,
        content,
        touch: Touch::Agent,
        on,
        next_change: None,
        pressed: false,
        checked: win.is_some_and(|w| w.focused),
        drawer: None,
        disabled: false,
        foreground: None,
        background: None,
        published: None,
        pass_through: false,
        target,
        timer_total: None,
        menu: Vec::new(),
        slot: None,
    }
}

/// Which workspaces/windows a `workspaces` / `taskbar` module shows (indexes into the lists).
/// Waybar semantics: workspaces of the focused output unless `all-outputs`; windows of the
/// active workspace unless `all-workspaces`.
fn listed(m: &ModuleInstance, world: &World) -> Vec<usize> {
    let flag = |k: &str| m.config.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let d = &world.desktop;
    if m.id.kind == "workspaces" {
        let here = d.monitor.as_deref();
        return (0..d.workspaces.len())
            .filter(|&i| {
                flag("all-outputs")
                    || here.is_none()
                    || d.workspaces[i].monitor.is_none()
                    || d.workspaces[i].monitor.as_deref() == here
            })
            .collect();
    }
    let active = d.workspace.as_ref().map(|w| w.id);
    // niri's window workspace ids are niri ids: match against the active workspace's info.
    let active = d
        .workspaces
        .iter()
        .find(|w| w.active)
        .map(|w| w.id)
        .or(active);
    (0..d.windows.len())
        .filter(|&i| {
            flag("all-workspaces") || active.is_none() || Some(d.windows[i].workspace) == active
        })
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
    /// Draw as a slider at this value (0..1).
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
            extra_classes: Vec::new(),
            raw: false,
            refresh: None,
            target: None,
            timer_total: None,
            menu: Vec::new(),
            fallback_text: None,
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
fn spec(m: &ModuleInstance, world: &World, ui: &UiState, item: Option<usize>) -> Option<Spec> {
    let s = &world.session;
    let kind = m.id.kind.as_str();
    let pct = |v: Option<u8>| v.map(f64::from);
    Some(match kind {
        "custom" if world.outputs.contains_key(&m.id.to_string()) => {
            let out = &world.outputs[&m.id.to_string()];
            let v = sliver_modules::view(m, out, ui.show_alt);
            if v.hidden {
                return None;
            }
            // `view` already applied the module's format; pass the result through as-is.
            let mut sp = Spec::new(ValueMap::new().with("", FmtValue::Text(v.text)), "{}");
            sp.extra_classes = v.classes;
            sp.default_icon = v.icon;
            let slider = m
                .config
                .get("slider")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if slider && let Some(value) = v.value {
                sp.slider = Some((value / 100.0).clamp(0.0, 1.0) as f32);
            }
            sp.raw = true;
            sp
        }
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
            // The picked player, if it is still there; else the session's choice.
            let media = ui
                .player
                .as_deref()
                .and_then(|p| s.players.iter().find(|m| m.player == p))
                .or(s.media.as_ref())?;
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
            // Album art as the icon (when it is the session's own player and has been fetched).
            let art = m
                .config
                .get("art")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let is_session_player = s.media.as_ref().is_some_and(|x| x.player == media.player);
            if art && is_session_player {
                if let Some(p) = &world.media_art {
                    sp.default_icon = Some(p.display().to_string());
                }
            }
            // Hold: switch to the next player when several are running.
            if s.players.len() > 1 {
                let idx = s
                    .players
                    .iter()
                    .position(|m| m.player == media.player)
                    .unwrap_or(0);
                let next = &s.players[(idx + 1) % s.players.len()].player;
                sp.defaults.push((
                    Gesture::Hold,
                    Action::ModuleAction(format!("player:{next}")),
                ));
            }
            // `scrub = true`: a progress slider; drag to seek.
            if m.config.get("scrub").and_then(|v| v.as_bool()) == Some(true) {
                if let Some(len) = media.length_us.filter(|l| *l > 0) {
                    let now_ms = world.now.timestamp_millis().max(0) as u64;
                    let mut pos = media.position_us.unwrap_or(0).max(0);
                    if media.status == sliver_session::PlaybackStatus::Playing {
                        if let Some(at) = media.position_at_ms {
                            pos += (now_ms.saturating_sub(at) as i64) * 1000;
                        }
                    }
                    sp.slider = Some((pos as f64 / len as f64).clamp(0.0, 1.0) as f32);
                    sp.percentage = Some(pos as f64 * 100.0 / len as f64);
                    if media.can_seek {
                        sp.defaults
                            .retain(|(g, _)| *g != Gesture::SwipeLeft && *g != Gesture::SwipeRight);
                        sp.defaults
                            .push((Gesture::Drag, Action::ModuleAction("seek".into())));
                    }
                    if media.status == sliver_session::PlaybackStatus::Playing {
                        sp.refresh = Some(std::time::Duration::from_secs(1));
                    }
                }
            }
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
            let mut sp = Spec::new(ValueMap::new().with("", esc(l)).with("long", esc(l)), "{}")
                .on(Gesture::Tap, "next");
            sp.target = Some(sliver_desktop::control::Action::NextLayout);
            // Hold lists the configured layouts (when the compositor says what they are).
            let d = &world.desktop;
            if d.keyboard_layouts.len() > 1 {
                sp.menu = d
                    .keyboard_layouts
                    .iter()
                    .enumerate()
                    .map(|(i, name)| sliver_config::MenuEntry {
                        label: if d.keyboard_layout_index == Some(i) {
                            format!("✓ {name}")
                        } else {
                            name.clone()
                        },
                        icon: None,
                        on_tap: Action::ModuleAction(format!("language:set:{i}")),
                    })
                    .collect();
            }
            sp
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
        "cpu" | "memory" | "temperature" | "disk" | "load" => return stat_spec(m, world),
        "network" => {
            use sliver_session::network::{NetKind, NetState};
            let n = &s.network;
            if n.state == NetState::Unknown {
                return None; // no NetworkManager
            }
            let (status, icon): (&'static str, String) = match (n.state, n.kind) {
                _ if n.wifi_enabled == Some(false) => {
                    ("off", "network-wireless-disabled-symbolic".into())
                }
                (NetState::Disconnected, _) => {
                    ("disconnected", "network-wireless-offline-symbolic".into())
                }
                (NetState::Connecting, _) => {
                    ("connecting", "network-wireless-acquiring-symbolic".into())
                }
                (_, NetKind::Wifi) => {
                    let level = match n.signal_pct.unwrap_or(0) {
                        80..=100 => "excellent",
                        55..=79 => "good",
                        30..=54 => "ok",
                        1..=29 => "weak",
                        _ => "none",
                    };
                    ("wifi", format!("network-wireless-signal-{level}-symbolic"))
                }
                (_, NetKind::Ethernet) => ("ethernet", "network-wired-symbolic".into()),
                (_, NetKind::Vpn) => ("vpn", "network-vpn-symbolic".into()),
                _ => ("linked", "network-wired-symbolic".into()),
            };
            let essid = n.ssid.clone().unwrap_or_else(|| n.name.clone());
            // Throughput is only sampled when the module has an explicit `interval`, so a
            // plain network cell still never wakes the bar.
            let polled = matches!(m.common.interval, Some(sliver_config::Interval::Every(_)));
            let (rates, hist) = if polled {
                world.stats.with(|s| {
                    let r = s.net_rates(std::time::Instant::now());
                    (r.unwrap_or_default(), s.net_history())
                })
            } else {
                Default::default()
            };
            let v = ValueMap::new()
                .with(
                    "bandwidthDownBytes",
                    sliver_stats::human_rate(rates.down, false),
                )
                .with(
                    "bandwidthUpBytes",
                    sliver_stats::human_rate(rates.up, false),
                )
                .with(
                    "bandwidthTotalBytes",
                    sliver_stats::human_rate(rates.down + rates.up, false),
                )
                .with(
                    "bandwidthDownBits",
                    sliver_stats::human_rate(rates.down, true),
                )
                .with("bandwidthUpBits", sliver_stats::human_rate(rates.up, true))
                .with(
                    "bandwidthTotalBits",
                    sliver_stats::human_rate(rates.down + rates.up, true),
                )
                .with("spark", sliver_stats::sparkline(&hist, None))
                .with("ifname", esc(n.iface.as_deref().unwrap_or("")))
                .with("essid", esc(&essid))
                .with(
                    "signalStrength",
                    FmtValue::Num(f64::from(n.signal_pct.unwrap_or(0))),
                )
                .with("ipaddr", esc(n.ipv4.as_deref().unwrap_or("")))
                .with("state", status)
                .with("", esc(&essid));
            let default = match status {
                "wifi" => "{essid}",
                "disconnected" | "connecting" => "",
                _ => "{ifname}",
            };
            let mut sp = Spec::new(v, default).icon(icon).on(Gesture::Tap, "toggle");
            sp.percentage = n.signal_pct.map(f64::from);
            sp.status = Some(status);
            sp.checked = n.wifi_enabled == Some(true) && status != "disconnected";
            if polled {
                sp.refresh = Some(poll_every(m, 5));
            }
            sp
        }
        "bluetooth" => {
            let b = &s.bluetooth;
            if !b.available {
                return None;
            }
            let status = if !b.powered {
                "off"
            } else if b.connected.is_empty() {
                "on"
            } else {
                "connected"
            };
            let first = b.connected.first();
            let v = ValueMap::new()
                .with("status", status)
                .with("num_connections", b.connected.len() as i64)
                .with("device_alias", esc(first.map_or("", |d| d.name.as_str())))
                .with(
                    "device_battery_percentage",
                    FmtValue::Num(first.and_then(|d| d.battery).map_or(0.0, f64::from)),
                )
                .with("", esc(first.map_or("", |d| d.name.as_str())));
            let icon = match status {
                "off" => "bluetooth-disabled-symbolic",
                "on" => "bluetooth-active-symbolic",
                _ => "bluetooth-active-symbolic",
            };
            let default = if status == "connected" {
                "{device_alias}"
            } else {
                ""
            };
            let mut sp = Spec::new(v, default).icon(icon).on(Gesture::Tap, "toggle");
            sp.status = Some(status);
            sp.checked = status == "connected";
            sp
        }
        // The picker's entry button: opens the `emoji` layer (the strip lives there).
        "emoji" => {
            let layer = m
                .config
                .get("layer")
                .and_then(|v| v.as_str())
                .unwrap_or("emoji");
            let mut sp = Spec::new(ValueMap::new(), "😀");
            sp.defaults.push((
                Gesture::Tap,
                Action::Layer {
                    name: layer.into(),
                    mode: sliver_config::LayerMode::Toggle,
                },
            ));
            sp
        }
        "timer" => {
            let total = m
                .config
                .get("duration")
                .and_then(|v| sliver_config::parse_duration(v).ok())
                .filter(|d| !d.is_zero())
                .unwrap_or(std::time::Duration::from_secs(25 * 60));
            let (left, status) = match &ui.timer {
                None => (total, "idle"),
                Some(t) if t.finished => (std::time::Duration::ZERO, "finished"),
                Some(t) if t.running_since.is_some() => (t.left(world.now), "running"),
                Some(t) => (t.remaining, "paused"),
            };
            let pct = 100.0 * (1.0 - left.as_secs_f64() / total.as_secs_f64());
            let v = ValueMap::new()
                .with("remaining", clock_text(left))
                .with("minutes", (left.as_secs() / 60) as i64)
                .with("seconds", (left.as_secs() % 60) as i64)
                .with("", clock_text(left));
            let mut sp = Spec::new(v, "{remaining}")
                .on(Gesture::Tap, "toggle")
                .on(Gesture::Hold, "reset");
            sp.percentage = Some(pct.clamp(0.0, 100.0));
            sp.status = Some(status);
            sp.checked = status == "running";
            sp.timer_total = Some(total);
            if status == "running" {
                sp.refresh = Some(std::time::Duration::from_secs(1));
            }
            sp
        }
        "notifications" => {
            let n = &s.notifications;
            let count = if n.count == 0 {
                FmtValue::Text(String::new())
            } else {
                FmtValue::Num(f64::from(n.count))
            };
            let (summary, app) = n
                .latest
                .as_ref()
                .map_or((String::new(), String::new()), |l| {
                    (l.summary.clone(), l.app.clone())
                });
            let v = ValueMap::new()
                .with("count", count.clone())
                .with("summary", esc(&summary))
                .with("app", esc(&app))
                .with("", count);
            // The latest summary when there is one (a bell with just the number otherwise).
            let default = if n.latest.is_some() {
                "{summary}"
            } else {
                "{count}"
            };
            let mut sp = Spec::new(v, default)
                .icon(if n.dnd == Some(true) {
                    "notifications-disabled-symbolic"
                } else {
                    "notifications-symbolic"
                })
                .on(Gesture::Tap, "open")
                .on(Gesture::SwipeLeft, "dismiss")
                .on(Gesture::SwipeRight, "dismiss");
            sp.status = Some(if n.count > 0 { "pending" } else { "none" });
            sp.extra_classes = n
                .dnd
                .filter(|d| *d)
                .map(|_| "dnd".to_string())
                .into_iter()
                .collect();
            sp
        }
        // Do Not Disturb, when the notification daemon reports it (dunst, KDE).
        // One-shot actions: no state, so they are always there.
        "screenshot" => {
            let mut sp = Spec::new(ValueMap::new(), "")
                .icon("applets-screenshooter-symbolic")
                .on(Gesture::Tap, "screenshot");
            sp.fallback_text = Some("📷");
            sp
        }
        "lock" => {
            let mut sp = Spec::new(ValueMap::new(), "")
                .icon("system-lock-screen-symbolic")
                .on(Gesture::Tap, "lock");
            sp.fallback_text = Some("🔒");
            sp
        }
        // Tap: lock and suspend. Hold: reboot and power off, so they can't be tapped by accident.
        "power" => {
            let mut sp = Spec::new(ValueMap::new(), "")
                .icon("system-shutdown-symbolic")
                .on(Gesture::Hold, "power:hold");
            sp.fallback_text = Some("⏻");
            sp.menu = power_menu(false);
            sp
        }
        "dnd" => {
            let on = s.notifications.dnd?;
            let mut sp = Spec::new(
                ValueMap::new().with("state", if on { "on" } else { "off" }),
                "",
            )
            .icon(if on {
                "notifications-disabled-symbolic"
            } else {
                "notifications-symbolic"
            });
            sp.status = Some(if on { "on" } else { "off" });
            sp.checked = on;
            sp.defaults
                .push((Gesture::Tap, Action::ModuleAction("toggle".into())));
            sp
        }
        // Light/dark colour scheme, when the portal reports one.
        "dark-mode" => {
            let dark = s.appearance.dark?;
            let mut sp = Spec::new(
                ValueMap::new().with("state", if dark { "dark" } else { "light" }),
                "",
            )
            .icon(if dark {
                "weather-clear-night-symbolic"
            } else {
                "weather-clear-symbolic"
            });
            sp.status = Some(if dark { "dark" } else { "light" });
            sp.checked = dark;
            sp.defaults
                .push((Gesture::Tap, Action::ModuleAction("toggle".into())));
            sp
        }
        "workspaces" => {
            let ws = world.desktop.workspaces.get(item?)?;
            let status = if ws.urgent {
                "urgent"
            } else if ws.active {
                "active"
            } else if ws.windows == 0 {
                "empty"
            } else {
                "default"
            };
            let v = ValueMap::new()
                .with("name", esc(&ws.name))
                .with("id", ws.id)
                .with("windows", ws.windows as i64)
                .with("", esc(&ws.name));
            let mut sp = Spec::new(v, "{name}").on(Gesture::Tap, "focus");
            sp.status = Some(status);
            sp.checked = ws.active;
            sp.target = Some(sliver_desktop::control::Action::FocusWorkspace(ws.clone()));
            sp
        }
        "taskbar" => {
            let w = world.desktop.windows.get(item?)?;
            let v = ValueMap::new()
                .with("title", esc(&w.title))
                .with("app_id", esc(&w.class))
                .with("", esc(&w.title));
            // Icon only by default: the bar is too narrow for titles.
            let mut sp = Spec::new(v, "")
                .on(Gesture::Tap, "focus")
                .on(Gesture::Hold, "close");
            let entry = world.apps.find_by_app_id(&w.class);
            sp.default_icon = Some(
                entry
                    .and_then(|e| e.icon.clone())
                    .unwrap_or_else(|| w.class.clone()),
            );
            sp.checked = w.focused;
            sp.extra_classes = [(w.focused, "focused"), (w.urgent, "urgent")]
                .into_iter()
                .filter(|(on, _)| *on)
                .map(|(_, c)| c.to_string())
                .collect();
            sp.target = Some(sliver_desktop::control::Action::FocusWindow {
                address: w.address.clone(),
            });
            sp
        }
        // No data source wired yet: show nothing rather than a broken placeholder.
        _ => return None,
    })
}

const GIB: f64 = (1u64 << 30) as f64;

/// Rounds like waybar's integer percentages and one-decimal sizes.
fn pct0(v: f64) -> FmtValue {
    FmtValue::Num(v.round())
}

fn gib(bytes: u64) -> FmtValue {
    FmtValue::Num((bytes as f64 / GIB * 10.0).round() / 10.0)
}

/// How often a polled module refreshes: its `interval`, else the module's own default.
fn poll_every(m: &ModuleInstance, default_secs: u64) -> std::time::Duration {
    match m.common.interval {
        Some(sliver_config::Interval::Every(d)) => d,
        _ => std::time::Duration::from_secs(default_secs),
    }
}

/// `cpu`, `memory`, `load`, `temperature`, `disk`: read when drawn, redrawn on their interval
/// (the agent's loop wakes only while such a cell is on the bar and the display is on).
fn stat_spec(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let stats = &world.stats;
    let now = std::time::Instant::now();
    let mut sp = match m.id.kind.as_str() {
        "cpu" => {
            let (usage, load, ghz, hist) = stats.with(|s| {
                let u = s.cpu_usage(now);
                (u, s.load(), s.cpu_ghz(), s.cpu_history())
            });
            let usage = usage?;
            let v = ValueMap::new()
                .with("usage", pct0(usage))
                .with("", pct0(usage))
                .with("load", load.map_or(0.0, |l| l.one))
                .with("avg_frequency", (ghz.unwrap_or(0.0) * 10.0).round() / 10.0)
                .with("spark", sliver_stats::sparkline(&hist, Some(100.0)));
            let mut sp = Spec::new(v, "CPU {usage}%");
            sp.percentage = Some(usage);
            sp
        }
        "memory" => {
            let mem = stats.with(|s| s.mem())?;
            let v = ValueMap::new()
                .with("percentage", pct0(mem.percent_used()))
                .with("", pct0(mem.percent_used()))
                .with("used", gib(mem.used()))
                .with("total", gib(mem.total))
                .with("avail", gib(mem.available))
                .with("swapPercentage", pct0(mem.swap_percent()))
                .with("swapUsed", gib(mem.swap_used()))
                .with("swapTotal", gib(mem.swap_total))
                .with("swapAvail", gib(mem.swap_free));
            let mut sp = Spec::new(v, "MEM {percentage}%");
            sp.percentage = Some(mem.percent_used());
            sp
        }
        "load" => {
            let l = stats.with(|s| s.load())?;
            let v = ValueMap::new()
                .with("load1", l.one)
                .with("load5", l.five)
                .with("load15", l.fifteen)
                .with("", l.one);
            Spec::new(v, "{load1}")
        }
        "temperature" => {
            let c = temperature(m, stats)?;
            let v = ValueMap::new()
                .with("temperatureC", c.round())
                .with("temperatureF", (c * 9.0 / 5.0 + 32.0).round())
                .with("temperatureK", (c + 273.15).round())
                .with("", c.round());
            let mut sp = Spec::new(v, "{temperatureC}°C");
            let critical = m
                .config
                .get("critical-threshold")
                .and_then(|t| t.as_float().or(t.as_integer().map(|i| i as f64)));
            sp.status = critical.filter(|t| c >= *t).map(|_| "critical");
            sp
        }
        "disk" => {
            let path = m.config.get("path").and_then(|p| p.as_str()).unwrap_or("/");
            let d = sliver_stats::disk(std::path::Path::new(path))?;
            let v = ValueMap::new()
                .with("percentage_used", pct0(d.percent_used()))
                .with("percentage_free", pct0(d.percent_free()))
                .with("used", gib(d.used()))
                .with("free", gib(d.free))
                .with("total", gib(d.total))
                .with("path", esc(path))
                .with("", pct0(d.percent_used()));
            let mut sp = Spec::new(v, "{percentage_used}%");
            sp.percentage = Some(d.percent_used());
            sp
        }
        _ => return None,
    };
    let default = match m.id.kind.as_str() {
        "memory" | "disk" => 30,
        _ => 10,
    };
    sp.refresh = Some(poll_every(m, default));
    Some(sp)
}

/// The temperature to show: `hwmon-path` (a `temp*_input` file), `thermal-zone` (an index), or the
/// hottest sensor.
fn temperature(m: &ModuleInstance, stats: &Stats) -> Option<f64> {
    let read = |p: &std::path::Path| {
        std::fs::read_to_string(p)
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| v / 1000.0)
    };
    if let Some(p) = m.config.get("hwmon-path").and_then(|v| v.as_str()) {
        return read(std::path::Path::new(p));
    }
    if let Some(z) = m.config.get("thermal-zone").and_then(|v| v.as_integer()) {
        let root = stats.with(|s| s.sys_root().to_path_buf());
        return read(&root.join(format!("class/thermal/thermal_zone{z}/temp")));
    }
    stats
        .with(|s| s.sensors())
        .iter()
        .map(|s| s.celsius)
        .fold(None, |a: Option<f64>, c| Some(a.map_or(c, |a| a.max(c))))
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
            (None, Some(i), true) => Content::Icon(i),
            (None, Some(i), false) => Content::IconMarkup(i, f.text),
            (None, None, false) => Content::Markup(f.text),
            // A window whose app has no icon: its name's first letters.
            (None, None, true) if m.id.kind == "taskbar" => {
                let name = world
                    .desktop
                    .windows
                    .get(item.unwrap_or(usize::MAX))
                    .map_or("", |w| w.class.as_str());
                Content::Markup(sliver_render::markup::escape(
                    &name.chars().take(2).collect::<String>(),
                ))
            }
            (None, None, true) => sp
                .fallback_text
                .map_or(Content::Blank, |t| Content::Markup(t.into())),
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
    // `menu = [...]`: the trigger gesture (default: tap, or hold when the module also taps) opens
    // the sub-bar instead of whatever that gesture did.
    let menu = if m.common.menu.is_empty() {
        std::mem::take(&mut sp.menu)
    } else {
        m.common.menu.clone()
    };
    if !menu.is_empty() {
        let has_tap = on.iter().any(|(g, _)| *g == Gesture::Tap);
        let trigger =
            m.common
                .menu_trigger
                .unwrap_or(if has_tap { Gesture::Hold } else { Gesture::Tap });
        on.retain(|(g, _)| *g != trigger);
        on.push((trigger, Action::ModuleAction("menu:open".into())));
    }
    let collapsed_slider = m.id.kind.ends_with("/slider") && sp.slider.is_none();
    let mut touch = touch_kind(m, &on);
    if collapsed_slider {
        touch = Touch::Agent;
    }
    Some(Cell {
        key: key.clone(),
        id: m.id.to_string(),
        kind: m.id.kind.clone(),
        mic: m.config.get("node").and_then(|v| v.as_str()) == Some("source"),
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
        drawer: (collapsed_slider
            || m.id.kind.ends_with("/slider") && ui.expanded && is_collapsible(m))
        .then(|| Drawer {
            group: key,
            collapse_after: Some(std::time::Duration::from_secs(5)),
            bar: false,
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

    fn stat_cells(toml: &str, w: &World) -> Vec<Cell> {
        let loaded = load_str(toml, std::path::Path::new("."));
        assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
        let r = loaded.config.resolve("base", &Context::new()).unwrap();
        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        for z in &r.zones {
            cells(
                &z.modules,
                w,
                &mut icons,
                36,
                &|_| UiState::default(),
                &mut out,
            );
        }
        out
    }

    #[test]
    fn stats_modules_read_files_and_schedule_their_own_refresh() {
        let dir = std::env::temp_dir().join(format!("sliver-agent-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let put = |rel: &str, text: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        put("proc/stat", "cpu  100 0 100 800 0 0 0 0 0 0\n");
        put(
            "proc/meminfo",
            "MemTotal: 16777216 kB\nMemAvailable: 4194304 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
        );
        put("proc/loadavg", "1.50 1.00 0.50 1/100 1\n");
        put("sys/class/hwmon/hwmon0/name", "chip\n");
        put("sys/class/hwmon/hwmon0/temp1_input", "61500\n");
        let mut w = world();
        w.stats = Stats::with_roots(dir.join("proc"), dir.join("sys"));
        let cells = stat_cells(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["cpu", "memory", "load", "temperature", "temperature#hot", "disk", "cpu#fast"]
            [modules.cpu]
            format = "{usage}% @{avg_frequency}"
            [modules.memory]
            format = "{used}/{total}G {percentage}%"
            [modules.load]
            format = "L {load1}/{load5}"
            [modules."temperature#hot"]
            critical-threshold = 60
            [modules.disk]
            path = "/"
            [modules."cpu#fast"]
            interval = "2s"
            "#,
            &w,
        );
        let text = |c: &Cell| match &c.content {
            Content::Markup(t) | Content::Text(t) => t.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(text(&cells[0]), "20% @0");
        assert_eq!(text(&cells[1]), "12/16G 75%");
        assert_eq!(text(&cells[2]), "L 1.5/1");
        assert_eq!(text(&cells[3]), "62°C");
        assert!(!cells[3].classes.contains(&"critical".to_string()));
        assert!(
            cells[4].classes.contains(&"critical".to_string()),
            "61.5 >= 60: {:?}",
            cells[4].classes
        );
        assert!(text(&cells[5]).ends_with('%'));
        // refresh: the module's interval, else its default; the clock-like wake-up the loop uses
        let due = |c: &Cell| (c.next_change.unwrap() - w.now).num_seconds();
        assert_eq!(
            (due(&cells[0]), due(&cells[1]), due(&cells[6])),
            (10, 30, 2)
        );
        // with no data source (no /proc here) the module hides instead of showing zeros
        let mut empty = world();
        empty.stats = Stats::with_roots("/nonexistent", "/nonexistent");
        let none = stat_cells(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"cpu\", \"memory\", \"temperature\"]\n",
            &empty,
        );
        assert!(none.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn network_bandwidth_and_cpu_sparkline() {
        let dir = std::env::temp_dir().join(format!("sliver-agent-bw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proc/net")).unwrap();
        std::fs::write(dir.join("proc/stat"), "cpu  100 0 100 800 0 0 0 0 0 0\n").unwrap();
        std::fs::write(
            dir.join("proc/net/dev"),
            " eth0: 1000 1 0 0 0 0 0 0 500 1 0 0 0 0 0 0\n",
        )
        .unwrap();
        let mut w = world();
        w.stats = Stats::with_roots(dir.join("proc"), dir.join("sys"));
        w.session.network.state = sliver_session::network::NetState::Connected;
        w.session.network.kind = sliver_session::network::NetKind::Ethernet;
        w.session.network.iface = Some("eth0".into());
        let text = |c: &Cell| match &c.content {
            Content::Markup(t) | Content::Text(t) | Content::IconMarkup(_, t) => t.clone(),
            other => panic!("{other:?}"),
        };
        let toml = |extra: &str| {
            format!(
                "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"network\", \"cpu\"]\n[modules.network]\nformat = \"{{bandwidthDownBytes}} {{bandwidthUpBits}}\"\n{extra}[modules.cpu]\nformat = \"{{spark}}\"\n"
            )
        };
        // no `interval`: not sampled, no wake-ups
        let cells = stat_cells(&toml(""), &w);
        assert!(cells[0].next_change.is_none());
        assert_eq!(text(&cells[0]), "0B/s 0b/s");
        assert_eq!(text(&cells[1]).chars().count(), 1, "one cpu sample so far");
        // with one: the first read primes the counters
        let cells = stat_cells(&toml("interval = 2\n"), &w);
        assert_eq!((cells[0].next_change.unwrap() - w.now).num_seconds(), 2);
        assert_eq!(text(&cells[0]), "0B/s 0b/s");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspaces_and_taskbar_follow_the_compositor_lists() {
        use sliver_desktop::{WindowInfo, WorkspaceInfo};
        let ws = |id: i64, name: &str, windows: u32, active: bool, mon: &str| WorkspaceInfo {
            id,
            name: name.into(),
            active,
            urgent: false,
            monitor: Some(mon.into()),
            windows,
        };
        let win = |addr: &str, class: &str, ws: i64, focused: bool, urgent: bool| WindowInfo {
            address: addr.into(),
            class: class.into(),
            title: format!("{class} window"),
            workspace: ws,
            focused,
            urgent,
            pid: None,
        };
        let mut w = world();
        let d = &mut w.desktop;
        d.connected = true;
        d.monitor = Some("eDP-1".into());
        d.workspace = Some(sliver_desktop::Workspace {
            id: 2,
            name: "web".into(),
        });
        d.workspaces = vec![
            ws(1, "1", 1, false, "eDP-1"),
            ws(2, "web", 2, true, "eDP-1"),
            ws(3, "3", 0, false, "eDP-1"),
            ws(4, "ext", 1, false, "HDMI-A-1"),
        ];
        d.windows = vec![
            win("a1", "kitty", 1, false, false),
            win("b2", "firefox", 2, true, false),
            win("c3", "mpv", 2, false, true),
        ];
        let cells = stat_cells(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["workspaces", "taskbar"]
            [modules.workspaces]
            format = "{name}:{windows}"
            "#,
            &w,
        );
        let text = |c: &Cell| match &c.content {
            Content::Markup(t) | Content::Text(t) => t.clone(),
            Content::IconMarkup(_, t) => t.clone(),
            other => format!("{other:?}"),
        };
        let wsc: Vec<&Cell> = cells.iter().filter(|c| c.kind == "workspaces").collect();
        assert_eq!(
            wsc.iter().map(|c| text(c)).collect::<Vec<_>>(),
            ["1:1", "web:2", "3:0"],
            "other output hidden"
        );
        assert!(wsc[1].checked && wsc[1].classes.contains(&"active".to_string()));
        assert!(wsc[2].classes.contains(&"empty".to_string()));
        assert_eq!(wsc[0].touch, Touch::Agent);
        assert_eq!(wsc[1].key, "workspaces/2");
        assert!(
            matches!(&wsc[0].target, Some(sliver_desktop::control::Action::FocusWorkspace(x)) if x.id == 1)
        );
        let tb: Vec<&Cell> = cells.iter().filter(|c| c.kind == "taskbar").collect();
        assert_eq!(tb.len(), 2, "only the active workspace's windows");
        assert!(tb[0].classes.contains(&"focused".to_string()) && tb[0].checked);
        assert!(tb[1].classes.contains(&"urgent".to_string()));
        // no icon theme in tests: the app's first letters stand in
        assert_eq!(text(tb[0]), "fi");
        let hold = tb[0]
            .on
            .iter()
            .find(|(g, _)| *g == sliver_config::Gesture::Hold)
            .map(|(_, a)| a.clone());
        assert!(matches!(hold, Some(sliver_config::Action::ModuleAction(n)) if n == "close"));
        // no compositor list (no backend): the modules show nothing
        let none = stat_cells(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"workspaces\", \"taskbar\"]\n",
            &world(),
        );
        assert!(none.is_empty());
    }

    #[test]
    fn notifications_and_dnd() {
        let toml =
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"notifications\", \"dnd\"]\n";
        let mut w = world();
        let c = stat_cells(toml, &w);
        assert_eq!(
            c.len(),
            1,
            "dnd is hidden while the daemon doesn't report it"
        );
        assert!(
            matches!(&c[0].content, Content::Icon(_) | Content::Blank),
            "no text at zero: {:?}",
            c[0].content
        );
        w.session.notifications.count = 3;
        w.session.notifications.dnd = Some(true);
        let c = stat_cells(toml, &w);
        assert_eq!(c.len(), 2);
        assert!(
            matches!(&c[0].content, Content::IconMarkup(_, t) | Content::Markup(t) if t == "3"),
            "{:?}",
            c[0].content
        );
        assert!(
            c[0].classes.contains(&"dnd".to_string())
                && c[0].classes.contains(&"pending".to_string())
        );
        assert!(c[1].checked);
    }

    #[test]
    fn network_and_bluetooth_modules() {
        use sliver_session::bluetooth::BtDevice;
        use sliver_session::network::{NetKind, NetState};
        let toml =
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"network\", \"bluetooth\"]\n";
        let mut w = world();
        assert!(
            stat_cells(toml, &w).is_empty(),
            "no NetworkManager / BlueZ: nothing to show"
        );
        let n = &mut w.session.network;
        n.state = NetState::Connected;
        n.kind = NetKind::Wifi;
        n.name = "Home".into();
        n.ssid = Some("Home <5G>".into());
        n.signal_pct = Some(70);
        n.iface = Some("wlan0".into());
        w.session.bluetooth.available = true;
        w.session.bluetooth.powered = true;
        w.session.bluetooth.connected = vec![BtDevice {
            name: "Buds".into(),
            address: "x".into(),
            battery: Some(55),
        }];
        let c = stat_cells(toml, &w);
        assert_eq!(c.len(), 2);
        let text = |c: &Cell| match &c.content {
            Content::Markup(t) | Content::IconMarkup(_, t) | Content::Text(t) => t.clone(),
            other => format!("{other:?}"),
        };
        assert_eq!(
            text(&c[0]),
            "Home &lt;5G&gt;",
            "an SSID cannot inject markup"
        );
        assert!(c[0].classes.contains(&"wifi".to_string()));
        assert_eq!(text(&c[1]), "Buds");
        assert!(c[1].checked && c[1].classes.contains(&"connected".to_string()));
        w.session.network.state = NetState::Disconnected;
        w.session.bluetooth.powered = false;
        let c = stat_cells(toml, &w);
        assert!(c[0].classes.contains(&"disconnected".to_string()));
        assert!(c[1].classes.contains(&"off".to_string()));
    }

    #[test]
    fn taskbar_pinned_apps() {
        use sliver_desktop::WindowInfo;
        let dir = std::env::temp_dir().join(format!("sliver-pinned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("org.mozilla.firefox.desktop"), "[Desktop Entry]\nType=Application\nName=Firefox\nIcon=firefox\nStartupWMClass=firefox\n").unwrap();
        std::fs::write(
            dir.join("kitty.desktop"),
            "[Desktop Entry]\nType=Application\nName=kitty\nIcon=kitty\n",
        )
        .unwrap();
        let mut w = world();
        w.apps = std::sync::Arc::new(sliver_icons::desktop::DesktopEntries::scan(
            std::slice::from_ref(&dir),
        ));
        w.desktop.connected = true;
        w.desktop.workspace = Some(sliver_desktop::Workspace {
            id: 1,
            name: "1".into(),
        });
        let win = |addr: &str, class: &str, focused: bool| WindowInfo {
            address: addr.into(),
            class: class.into(),
            title: class.into(),
            workspace: 1,
            focused,
            ..Default::default()
        };
        w.desktop.windows = vec![win("a1", "mpv", false), win("b2", "firefox", true)];
        let cells = stat_cells(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"taskbar\"]\n[modules.taskbar]\npinned = [\"org.mozilla.firefox.desktop\", \"kitty\"]\n",
            &w,
        );
        let keys: Vec<&str> = cells.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            ["taskbar/pin0", "taskbar/pin1", "taskbar/a1"],
            "pinned first, running pinned app not repeated"
        );
        assert!(
            cells[0].classes.contains(&"running".to_string())
                && cells[0].classes.contains(&"focused".to_string())
                && cells[0].checked
        );
        assert!(
            matches!(&cells[0].target, Some(sliver_desktop::control::Action::FocusWindow { address }) if address == "b2")
        );
        assert!(cells[1].classes.contains(&"idle".to_string()) && cells[1].target.is_none());
        assert!(
            cells[1]
                .on
                .iter()
                .any(|(g, a)| *g == sliver_config::Gesture::Tap
                    && matches!(a, sliver_config::Action::Open(id) if id == "kitty")),
            "an idle launcher starts the app"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
    fn preview_shows_samples_and_placeholders() {
        let l = load_str("", std::path::Path::new("."));
        let ctx = Context::new().with("physical_esc", true);
        let r = l.config.resolve("base", &ctx).unwrap();
        let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        for z in &r.zones {
            cells(
                &z.modules,
                &w,
                &mut icons,
                36,
                &|_| UiState::default(),
                &mut out,
            );
        }
        // The default main zone shows F1-F12 until an app fills App controls (group/fkeys).
        let fkeys = out
            .iter()
            .filter(|c| c.key.starts_with("group/fkeys/"))
            .count();
        assert_eq!(fkeys, 12);
        let kinds: Vec<&str> = out
            .iter()
            .filter(|c| !c.key.starts_with("group/fkeys/"))
            .map(|c| c.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                "app-controls",
                "status-items",
                "mpris",
                "custom",
                "backlight/slider",
                "audio/slider",
                "key"
            ],
        );
        assert!(out[0].classes.contains(&"placeholder".to_string()));
    }

    #[test]
    fn pinned_app_placeholders_have_short_labels() {
        let l = load_str(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"app/org.example.Pomodoro#timer\", \"app/org.example.Pomodoro\"]\n",
            std::path::Path::new("."),
        );
        assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
        let r = l.config.resolve("base", &Context::new()).unwrap();
        let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        for z in &r.zones {
            cells(
                &z.modules,
                &w,
                &mut icons,
                36,
                &|_| UiState::default(),
                &mut out,
            );
        }
        let labels: Vec<String> = out
            .iter()
            .filter(|c| c.classes.contains(&"placeholder".to_string()))
            .map(|c| match &c.content {
                Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(labels, ["timer", "Pomodoro"]);
    }

    #[test]
    fn placeholders_take_the_module_width() {
        let l = load_str(
            "[modules.status-items]\nwidth = 332\n",
            std::path::Path::new("."),
        );
        assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
        let ctx = Context::new().with("physical_esc", true);
        let r = l.config.resolve("base", &ctx).unwrap();
        let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        for z in &r.zones {
            cells(
                &z.modules,
                &w,
                &mut icons,
                36,
                &|_| UiState::default(),
                &mut out,
            );
        }
        let status = out.iter().find(|c| c.kind == "status-items").unwrap();
        assert_eq!(status.width, Width::Fixed(332.0));
        let app = out.iter().find(|c| c.kind == "app-controls").unwrap();
        assert_eq!(app.width, Width::Auto, "unset stays auto");
    }

    #[test]
    fn closed_drawer_shows_its_handle() {
        let cells = build("", "base");
        // group/fkeys (F1-F12 until an app publishes) comes first; it's not a drawer.
        let cells: Vec<Cell> = cells
            .into_iter()
            .filter(|c| !c.key.starts_with("group/fkeys/"))
            .collect();
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
                Content::Icon(i) | Content::IconText(i, _) | Content::IconMarkup(i, _) => {
                    Some(i.clone())
                }
                _ => None,
            });
        cell.content = match (icon, f.text.is_empty()) {
            (Some(i), true) => Content::Icon(i),
            (Some(i), false) => Content::IconMarkup(i, f.text),
            (None, false) => Content::Markup(f.text),
            (None, true) => Content::Blank,
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
        css_id: m.css_id.clone(),
        classes: vec!["placeholder".into(), class.into()],
        width: width(m.common.width),
        content: Content::Text(label.into()),
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
    }
}

/// Runtime specs for every `custom/*` module with an `exec`, in any layout (including group
/// members), for `sliver-feeds`' module runtime. Problems come back as messages.
pub fn user_module_specs(
    config: &sliver_config::Config,
) -> (Vec<sliver_modules::Spec>, Vec<String>) {
    use sliver_config::{ModuleId, ModuleRef};
    let mut seen = std::collections::HashSet::new();
    let (mut specs, mut errors) = (Vec::new(), Vec::new());
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
        if id.kind != "custom" || !block.contains_key("exec") {
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
