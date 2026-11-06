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
pub struct Stats(std::sync::Arc<std::sync::Mutex<crate::stats::Sampler>>);

impl std::fmt::Debug for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Stats")
    }
}

impl Stats {
    fn with<R>(&self, f: impl FnOnce(&mut crate::stats::Sampler) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// A sampler reading these roots instead of `/proc` and `/sys` (tests).
    pub fn with_roots(
        proc_root: impl Into<std::path::PathBuf>,
        sys_root: impl Into<std::path::PathBuf>,
    ) -> Stats {
        Stats(std::sync::Arc::new(std::sync::Mutex::new(
            crate::stats::Sampler::with_roots(proc_root, sys_root),
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
    /// The latest spectrum from `cava` (0..=1 per bar), empty while it isn't running.
    pub spectrum: Vec<f32>,
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
            spectrum: Vec::new(),
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
    /// `graph`: the samples to draw, oldest first, 0..=1 (the agent keeps them, see
    /// [`graph_sample`]), and how many fit.
    pub graph: Option<(Vec<f32>, usize)>,
    /// `clock` with `timezones`: how far the user swiped through the list (wraps around).
    pub tz: i32,
}

/// Is this a `graph` module kind (`graph`, waybar-style `custom-graph/<name>`)?
pub fn is_graph(kind: &str) -> bool {
    matches!(kind, "graph" | "custom-graph")
}

/// `graph` defaults: a sample every 2 s, 30 kept.
pub const GRAPH_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
pub const GRAPH_SAMPLES: usize = 30;

/// A graph's sampling period and history length, from its block (`interval`, `samples`).
pub fn graph_params(block: &toml::Table) -> (std::time::Duration, usize) {
    let every = block
        .get("interval")
        .and_then(|v| sliver_config::parse_duration(v).ok())
        .unwrap_or(GRAPH_EVERY)
        .max(std::time::Duration::from_millis(100));
    let samples = block
        .get("samples")
        .and_then(|v| v.as_integer())
        .map_or(GRAPH_SAMPLES, |n| n.clamp(2, 600) as usize);
    (every, samples)
}

/// `cava` defaults: 20 bars at 60 frames a second, the panel's refresh and the most the bar
/// draws while animating.
pub const CAVA_BARS: usize = 20;
pub const CAVA_FPS: i64 = 60;

/// How many bars a `cava` module draws (`bars`, 1–128).
pub fn cava_bars(block: &toml::Table) -> usize {
    block
        .get("bars")
        .and_then(|v| v.as_integer())
        .map_or(CAVA_BARS, |n| n.clamp(1, 128) as usize)
}

/// The config `cava` runs with for this module: waybar's `cava` keys in cava's own sections,
/// and raw output (one line of `bars` numbers, 0–1000, per frame) that the agent reads. Keys of
/// the wrong type, and strings that would break out of their line, are left out.
pub fn cava_config(block: &toml::Table) -> String {
    const KEYS: &[(&str, &str, &str)] = &[
        ("general", "autosens", "autosens"),
        ("general", "sensitivity", "sensitivity"),
        ("general", "lower_cutoff_freq", "lower_cutoff_freq"),
        ("general", "higher_cutoff_freq", "higher_cutoff_freq"),
        ("input", "method", "method"),
        ("input", "source", "source"),
        ("output", "reverse", "reverse"),
        ("smoothing", "monstercat", "monstercat"),
        ("smoothing", "waves", "waves"),
        ("smoothing", "noise_reduction", "noise_reduction"),
    ];
    let value = |v: &toml::Value| match v {
        toml::Value::Boolean(b) => Some(u8::from(*b).to_string()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::String(s) if !s.contains(['\n', '\r', '[', ';', '#']) => Some(s.clone()),
        _ => None,
    };
    let fps = block
        .get("framerate")
        .and_then(|v| v.as_integer())
        .map_or(CAVA_FPS, |n| n.clamp(1, CAVA_FPS));
    // A second of silence puts cava to sleep: no frames, and no work, until sound comes back.
    let sleep = block
        .get("sleep_timer")
        .and_then(|v| v.as_integer())
        .unwrap_or(1);
    let stereo = block
        .get("stereo")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut sections: Vec<(&str, Vec<String>)> = vec![
        (
            "general",
            vec![
                format!("bars = {}", cava_bars(block)),
                format!("framerate = {fps}"),
                format!("sleep_timer = {sleep}"),
            ],
        ),
        ("input", Vec::new()),
        (
            "output",
            vec![
                "method = raw".into(),
                "raw_target = /dev/stdout".into(),
                "data_format = ascii".into(),
                "ascii_max_range = 1000".into(),
                "bar_delimiter = 32".into(),
                "frame_delimiter = 10".into(),
                format!("channels = {}", if stereo { "stereo" } else { "mono" }),
            ],
        ),
        ("smoothing", Vec::new()),
    ];
    for (section, key, cava_key) in KEYS {
        if let Some(v) = block.get(*key).and_then(value)
            && let Some((_, lines)) = sections.iter_mut().find(|(s, _)| s == section)
        {
            lines.push(format!("{cava_key} = {v}"));
        }
    }
    sections
        .iter()
        .map(|(s, lines)| format!("[{s}]\n{}\n", lines.join("\n")))
        .collect()
}

/// `spectrum = true`: the live spectrum under the module's content (`cava`'s keys apply), at
/// rest until cava's first frame. Empty: none.
fn spectrum(m: &ModuleInstance, world: &World) -> Vec<f32> {
    if m.config.get("spectrum").and_then(|v| v.as_bool()) != Some(true) {
        return Vec::new();
    }
    let n = cava_bars(&m.config);
    if world.spectrum.len() == n {
        world.spectrum.clone()
    } else {
        vec![0.0; n]
    }
}

/// Does this cell show the spectrum (a `cava` module, or any with `spectrum = true`)?
pub fn shows_spectrum(c: &Cell) -> bool {
    c.kind == "cava" || !c.spectrum.is_empty()
}

/// The value a `graph` plots now, scaled to 0..=1 by `min`/`max` (default 0 and 100): from its
/// own script (`exec`: a number, or JSON `percentage`), or else another module's `percentage`
/// (`source = "cpu"`, `"memory"`, `"battery"`, `"custom/x"`…). `None`: nothing to plot yet.
pub fn graph_sample(
    config: &sliver_config::Config,
    id: &sliver_config::ModuleId,
    block: &toml::Table,
    world: &World,
) -> Option<f32> {
    use sliver_config::{ModuleId, ModuleRef};
    let from_output = |out: &sliver_modules::Output| {
        out.percentage.or_else(|| {
            out.text
                .lines()
                .next()?
                .trim()
                .trim_end_matches('%')
                .trim()
                .parse()
                .ok()
        })
    };
    let raw = if block.contains_key("exec") {
        from_output(world.outputs.get(&id.to_string())?)?
    } else {
        let src = ModuleId::parse(block.get("source")?.as_str()?).ok()?;
        if src.kind == "custom" {
            from_output(world.outputs.get(&src.to_string())?)?
        } else {
            let inst = ModuleInstance {
                css_id: src.css_id(),
                config: config.module_block(&ModuleRef::Named(src.clone())),
                id: src,
                classes: Vec::new(),
                common: sliver_config::Common::default(),
                children: Vec::new(),
            };
            spec(&inst, world, &UiState::default(), None)?.percentage?
        }
    };
    let num = |k: &str, d: f64| {
        block
            .get(k)
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(d)
    };
    let (lo, hi) = (num("min", 0.0), num("max", 100.0));
    if !raw.is_finite() || hi <= lo {
        return None;
    }
    Some(((raw - lo) / (hi - lo)).clamp(0.0, 1.0) as f32)
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
        slider: false,
        css_id: m.css_id.clone(),
        classes: classes
            .iter()
            .map(|c| c.to_string())
            .chain(m.classes.iter().cloned())
            .collect(),
        width: Width::Fixed(w),
        content: Content::markup(sliver_render::markup::escape(text)),
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
        spectrum: Vec::new(),
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

/// The cells of an open `menu` sub-bar: a ✕, then one per entry (label and/or icon, as wide as
/// that needs). The ✕ comes first and, given the opening button's `slot` (offset, width), takes
/// its place, so tapping there again dismisses instead of acting; so does any space around the
/// entries.
pub fn menu_cells(
    entries: &[sliver_config::MenuEntry],
    slot: Option<(f32, f32)>,
    icons: &mut Icons,
    icon_size: u32,
) -> Vec<Cell> {
    let cell = |key: String, content: Content, width: Width, classes: &[&str], tap: String| Cell {
        key,
        id: "menu".into(),
        kind: "menu".into(),
        mic: false,
        slider: false,
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
        spectrum: Vec::new(),
    };
    let x = icons
        .get("window-close-symbolic", None, icon_size)
        .map_or_else(|| Content::markup("✕"), Content::icon);
    let mut out = Vec::new();
    if let Some((offset, _)) = slot.filter(|s| s.0 > 0.0) {
        out.push(cell(
            "menu/before".into(),
            Content::Spacer,
            Width::Fixed(offset),
            &["menu-spacer"],
            "menu:close".into(),
        ));
    }
    out.push(cell(
        "menu/close".into(),
        x,
        Width::Fixed(slot.map_or(72.0, |s| s.1)),
        &["menu-close"],
        "menu:close".into(),
    ));
    for (i, e) in entries.iter().enumerate() {
        let icon = e
            .icon
            .as_deref()
            .and_then(|n| icons.get(n, None, icon_size));
        let label = sliver_render::markup::escape(&e.label);
        let content = match (icon, label.is_empty()) {
            (Some(ic), false) => Content::label(Some(ic), label),
            (Some(ic), true) => Content::icon(ic),
            (None, _) => Content::markup(label),
        };
        out.push(cell(
            format!("menu/{i}"),
            content,
            Width::Auto,
            &["menu-item"],
            format!("menu:pick:{i}"),
        ));
    }
    if !entries.is_empty() {
        out.push(cell(
            "menu/after".into(),
            Content::Spacer,
            Width::Fill(1),
            &["menu-spacer"],
            "menu:close".into(),
        ));
    }
    out
}

/// A button's `menu`, its labels filled in with the button's own placeholders, so one entry
/// names what it acts on ("Close {name}" reads "Close Firefox"). Labels stay plain text.
fn button_menu(
    menu: &[sliver_config::MenuEntry],
    values: &dyn sliver_config::Values,
) -> Vec<sliver_config::MenuEntry> {
    menu.iter()
        .map(|e| match sliver_config::Template::parse(&e.label) {
            Ok(t) if e.label.contains('{') => sliver_config::MenuEntry {
                label: sliver_render::markup::parse(&t.render(values)).plain(),
                ..e.clone()
            },
            _ => e.clone(),
        })
        .collect()
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

/// The class a taskbar button gets for its app, so a stylesheet can treat one app's icon
/// differently: `app-` and its desktop id (else its app id), lowercased, other characters as
/// `-` (`app-org-kde-dolphin`). Pinned or not, an app gets the same one.
pub(crate) fn app_class(id: &str) -> String {
    let id: String = id
        .trim_end_matches(".desktop")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("app-{id}")
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
    let win = window.map(|i| &world.desktop.windows[i]);
    let focused = win.is_some_and(|w| w.focused);
    // The same `format` as the window buttons (icon only by default).
    let esc = |s: &str| FmtValue::Text(sliver_render::markup::escape(s));
    let values = ValueMap::new()
        .with("name", esc(name))
        .with("title", esc(win.map_or("", |w| w.title.as_str())))
        .with("app_id", esc(win.map_or(id, |w| w.class.as_str())));
    let st = State {
        status: focused.then_some("focused"),
        ..Default::default()
    };
    let label = format_module(&m.common, "", &st, &values).text;
    let content = match icon {
        Some(icon) if label.is_empty() => Content::icon(icon),
        Some(icon) => Content::label(Some(icon), label),
        None if label.is_empty() => Content::markup(sliver_render::markup::escape(
            &name.chars().take(2).collect::<String>(),
        )),
        None => Content::markup(label),
    };
    let target = win.map(|w| sliver_desktop::control::Action::FocusWindow {
        address: w.address.clone(),
    });
    let mut classes = vec!["pinned".to_string(), app_class(id)];
    classes.push(if win.is_some() { "running" } else { "idle" }.to_string());
    if focused {
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
    let mut defaults = vec![(Gesture::Tap, tap)];
    if win.is_some() {
        defaults.push((Gesture::Hold, Action::ModuleAction("close".into())));
    }
    // The module's own bindings and `menu` apply to its launchers too.
    let on = bindings(m, defaults, !m.common.menu.is_empty());
    Cell {
        key: format!("{key}/pin{index}"),
        id: m.id.to_string(),
        kind: "taskbar".into(),
        mic: false,
        slider: false,
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
        menu: button_menu(&m.common.menu, &values),
        slot: None,
        spectrum: Vec::new(),
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

mod desktop;
mod media;
mod system;
mod widgets;

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
                .with("spark", crate::stats::sparkline(&hist, Some(100.0)));
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
            let d = crate::stats::disk(std::path::Path::new(path))?;
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

const CLOCK_DEFAULT: &str = "{:%H:%M}";

/// Clock placeholders: `{}`/`{time}` with a strftime spec, like waybar (`{:%a %d %b}`), and
/// `{timezone}`. waybar's `timezone`/`timezones` (`""` is local time; swipe to cycle) and `locale`
/// (`de_DE`; names of days and months) are read from the module block.
struct Clock {
    now: DateTime<Local>,
    /// `None`: local time. `Err`: a name that isn't in the time-zone database.
    zone: Option<Result<chrono_tz::Tz, String>>,
    locale: Option<Result<chrono::Locale, String>>,
}

impl Clock {
    /// The clock for this module, and how many time zones it cycles through.
    fn new(m: &ModuleInstance, now: DateTime<Local>, tz: i32) -> (Clock, usize) {
        let s = |v: &toml::Value| v.as_str().map(str::to_owned);
        let zones: Vec<String> = match (m.config.get("timezones"), m.config.get("timezone")) {
            (Some(toml::Value::Array(a)), _) if !a.is_empty() => a.iter().filter_map(s).collect(),
            (_, Some(z)) => s(z).into_iter().collect(),
            _ => Vec::new(),
        };
        let zone = (!zones.is_empty())
            .then(|| &zones[tz.rem_euclid(zones.len() as i32) as usize])
            .filter(|z| !z.is_empty())
            .map(|z| z.parse::<chrono_tz::Tz>().map_err(|_| z.clone()));
        let locale = m
            .config
            .get("locale")
            .and_then(toml::Value::as_str)
            .map(|l| {
                // `de_DE.UTF-8@euro` → `de_DE`, as found in $LANG.
                let base = l.split(['.', '@']).next().unwrap_or(l);
                chrono::Locale::try_from(base).map_err(|_| l.to_owned())
            });
        (Clock { now, zone, locale }, zones.len())
    }
}

impl Values for Clock {
    fn format(&self, name: &str, spec: Option<&str>) -> Option<String> {
        match name {
            "" | "time" => {
                let spec = spec.unwrap_or("%H:%M");
                let items: Vec<FmtItem> = StrftimeItems::new(spec).collect();
                if items.iter().any(|i| matches!(i, FmtItem::Error)) {
                    return Some(format!("⚠ bad time format {spec:?}"));
                }
                let locale = match &self.locale {
                    None => None,
                    Some(Ok(l)) => Some(*l),
                    Some(Err(l)) => return Some(format!("⚠ unknown locale {l:?}")),
                };
                Some(match &self.zone {
                    Some(Err(z)) => format!("⚠ unknown time zone {z:?}"),
                    Some(Ok(tz)) => strftime(&self.now.with_timezone(tz), &items, locale),
                    None => strftime(&self.now, &items, locale),
                })
            }
            "timezone" => Some(match &self.zone {
                None => "local".to_owned(),
                Some(Ok(tz)) => tz.name().to_owned(),
                Some(Err(z)) => z.clone(),
            }),
            _ => None,
        }
    }
}

fn strftime<Tz: chrono::TimeZone>(
    t: &DateTime<Tz>,
    items: &[FmtItem],
    locale: Option<chrono::Locale>,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match locale {
        Some(l) => t.format_localized_with_items(items.iter(), l).to_string(),
        None => t.format_with_items(items.iter()).to_string(),
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
        assert!(matches!(&cells[0].content, Content::Label { text: t, .. } if t == "F1"));
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
    fn clock_time_zones_and_locale() {
        let toml = r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["clock", "clock#de", "clock#bad"]
            [modules.clock]
            format = "{:%H:%M} {timezone}"
            timezones = ["", "Asia/Tokyo"]
            [modules."clock#de"]
            format = "{:%A}"
            locale = "de_DE.UTF-8"
            [modules."clock#bad"]
            timezone = "Mars/Olympus"
            "#;
        let loaded = load_str(toml, std::path::Path::new("."));
        let r = loaded.config.resolve("base", &Context::new()).unwrap();
        let at = |tz: i32| {
            let mut out = Vec::new();
            let ui = move |_: &str| UiState {
                tz,
                ..UiState::default()
            };
            let mut icons = Icons::builtin_only();
            cells(&r.zones[0].modules, &world(), &mut icons, 36, &ui, &mut out);
            out.iter()
                .map(|c| match &c.content {
                    Content::Label { text: t, .. } => t.clone(),
                    other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>()
        };
        let now = world().now;
        let tokyo = now.with_timezone(&chrono_tz::Asia::Tokyo).format("%H:%M");
        assert_eq!(at(0)[0], format!("{} local", now.format("%H:%M")));
        assert_eq!(at(1)[0], format!("{tokyo} Asia/Tokyo"));
        assert_eq!(at(-1)[0], at(1)[0], "swiping back wraps around");
        assert_eq!(at(0)[1], "Montag", "2026-10-12 in German");
        assert!(at(0)[2].contains("unknown time zone"), "{:?}", at(0)[2]);

        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        cells(
            &r.zones[0].modules,
            &world(),
            &mut icons,
            36,
            &|_| UiState::default(),
            &mut out,
        );
        let swipes = |c: &Cell| {
            c.on.iter()
                .filter(|(g, _)| *g == Gesture::SwipeLeft)
                .count()
        };
        assert_eq!(
            (swipes(&out[0]), swipes(&out[1])),
            (1, 0),
            "only a list cycles"
        );
    }

    #[test]
    fn window_title_rewrite() {
        let mut w = World::sample(world().now);
        w.desktop.window.as_mut().unwrap().title = "Docs <b> - Mozilla Firefox".into();
        let cells = stat_cells(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["window", "window#plain"]
            [modules.window]
            rewrite = [{ match = "(.*) - Mozilla Firefox", replace = "web: $1" }]
            "#,
            &w,
        );
        let text = |c: &Cell| match &c.content {
            Content::Label { text: t, .. } => t.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            text(&cells[0]),
            "web: Docs &lt;b&gt;",
            "rewritten, still escaped"
        );
        assert_eq!(text(&cells[1]), "Docs &lt;b&gt; - Mozilla Firefox");
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
            Content::Label { text: t, .. } => t.clone(),
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
            Content::Label { text: t, .. } => t.clone(),
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
            Content::Label { text: t, .. } => t.clone(),
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
            matches!(&c[0].content, Content::Label { text, .. } if text.is_empty()),
            "no text at zero: {:?}",
            c[0].content
        );
        w.session.notifications.count = 3;
        w.session.notifications.dnd = Some(true);
        let c = stat_cells(toml, &w);
        assert_eq!(c.len(), 2);
        assert!(
            matches!(&c[0].content, Content::Label { text: t, .. } if t == "3"),
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
            Content::Label { text: t, .. } => t.clone(),
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
                && cells[0]
                    .classes
                    .contains(&"app-org-mozilla-firefox".to_string())
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
        // `format-focused` labels only the app you're in, with its name (`{name}`): a pinned
        // app by its desktop entry, a window without one by its class.
        let label = |c: &Cell| match &c.content {
            Content::Label { text: t, .. } => t.clone(),
            _ => String::new(),
        };
        let conf = "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"taskbar\"]\n[modules.taskbar]\npinned = [\"org.mozilla.firefox.desktop\", \"kitty\"]\nformat-focused = \"{name}\"\n";
        let cells = stat_cells(conf, &w);
        let labels: Vec<String> = cells.iter().map(label).collect();
        // No icons here: the others fall back to their first letters.
        assert_eq!(labels, ["Firefox", "ki", "mp"]);
        w.desktop.windows = vec![win("a1", "mpv", true), win("b2", "firefox", false)];
        let cells = stat_cells(conf, &w);
        assert!(cells[2].classes.contains(&"focused".to_string()));
        let labels: Vec<String> = cells.iter().map(label).collect();
        assert_eq!(labels, ["Fi", "ki", "mpv"]);
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
        assert!(matches!(&cells[0].content, Content::Label { text: t, .. } if t == "09:41"));
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
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                "app-controls",
                "status-items",
                "mpris",
                "custom/expand",
                "backlight/slider",
                "audio/slider",
                "key/Mute"
            ],
        );
        assert!(out[0].classes.contains(&"placeholder".to_string()));
        assert_eq!(out.iter().filter(|c| c.slider).count(), 2);
    }

    #[test]
    fn slider_is_one_switch_for_every_module_with_a_level() {
        let l = load_str(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"audio/slider\", \"audio#s\", \"audio#b\"]\n[modules.\"audio/slider\"]\ncollapsed = false\n[modules.\"audio#s\"]\nslider = true\n",
            std::path::Path::new("."),
        );
        assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
        let r = l.config.resolve("base", &Context::new()).unwrap();
        let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
        let mut out = Vec::new();
        for z in &r.zones {
            let ui = |_: &str| UiState::default();
            cells(
                &z.modules,
                &w,
                &mut Icons::builtin_only(),
                36,
                &ui,
                &mut out,
            );
        }
        let drags = |c: &Cell| c.on.iter().any(|(g, _)| *g == Gesture::Drag);
        let taps = |c: &Cell| c.on.iter().any(|(g, _)| *g == Gesture::Tap);
        // Written as waybar's `audio/slider` or as `slider = true`: the same slider.
        for c in &out[..2] {
            assert!(matches!(c.content, Content::Slider { .. }), "{}", c.id);
            assert!(c.slider && drags(c) && !taps(c), "{}", c.id);
        }
        // Without it, the volume is a button whose tap mutes.
        assert!(!out[2].slider && taps(&out[2]) && !drags(&out[2]));
        assert!(!matches!(out[2].content, Content::Slider { .. }));
    }

    #[test]
    fn graphs_preview_with_a_made_up_history() {
        let l = load_str(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"graph#cpu\"]\n[modules.\"graph#cpu\"]\nsource = \"cpu\"\nsamples = 20\n",
            std::path::Path::new("."),
        );
        assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
        let r = l.config.resolve("base", &Context::new()).unwrap();
        let w = World::sample(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
        assert!(w.preview);
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
        match &out[0].content {
            Content::Graph { values, capacity } => {
                assert_eq!((values.len(), *capacity), (20, 20));
                assert!(values.iter().all(|v| (0.0..=1.0).contains(v)));
            }
            other => panic!("{other:?}"),
        }
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
                Content::Label { text: t, .. } => t.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(labels, ["timer", "Pomodoro"]);
    }

    /// Waybar's systemd-failed-units: counts from both managers, hidden while none failed
    /// (unless `hide-on-ok = false`), `system`/`user` filters, and a tap lists the units.
    #[test]
    fn systemd_failed_units() {
        use sliver_session::failed::{FailedUnits, Manager};
        let run = |toml: &str, f: &FailedUnits| -> Vec<Cell> {
            let l = load_str(
                &format!(
                    "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"systemd-failed-units\"]\n\
                     [modules.systemd-failed-units]\n{toml}"
                ),
                std::path::Path::new("."),
            );
            assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
            let r = l.config.resolve("base", &Context::new()).unwrap();
            let mut w = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 7).unwrap());
            w.session.failed_units = f.clone();
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
            out
        };
        let text = |c: &Cell| match &c.content {
            Content::Label { text: t, .. } => t.clone(),
            other => format!("{other:?}"),
        };
        let mgr = |u: &[&str]| {
            Some(Manager {
                count: u.len() as u32,
                units: u.iter().map(|s| s.to_string()).collect(),
            })
        };
        let none = FailedUnits {
            watched: true,
            system: mgr(&[]),
            user: mgr(&[]),
        };
        assert!(run("", &none).is_empty(), "hidden while all is well");
        assert!(
            run("", &FailedUnits::default()).is_empty(),
            "unknown: hidden"
        );
        let ok = run("hide-on-ok = false", &none);
        assert_eq!(text(&ok[0]), "✓");
        assert!(
            ok[0].classes.contains(&"ok".to_owned()),
            "{:?}",
            ok[0].classes
        );

        let some = FailedUnits {
            watched: true,
            system: mgr(&["nfs.mount"]),
            user: mgr(&["pipewire.service"]),
        };
        let c = run("", &some);
        assert_eq!(text(&c[0]), "2 failed");
        assert!(c[0].classes.contains(&"degraded".to_owned()));
        let labels: Vec<&str> = c[0].menu.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["nfs.mount", "pipewire.service (user)"]);
        assert_eq!(
            c[0].menu[1].on_tap,
            Action::Exec(
                [
                    "xdg-terminal-exec",
                    "systemctl",
                    "--user",
                    "status",
                    "pipewire.service"
                ]
                .map(String::from)
                .into()
            )
        );
        assert!(
            c[0].on
                .iter()
                .any(|(g, a)| *g == Gesture::Tap && *a == Action::ModuleAction("menu:open".into()))
        );
        let c = run(
            "user = false\nformat = \"{nr_failed_system}/{nr_failed_user}: {units}\"",
            &some,
        );
        assert_eq!(text(&c[0]), "1/0: nfs.mount");
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
        assert!(
            matches!(&cells[1].content, Content::Label { icon: Some(_), text } if text.is_empty())
        );
    }

    #[test]
    fn drawer_options_class_direction_and_start() {
        let loaded = load_str(
            r#"
            [[layouts.t.zones]]
            name = "main"
            modules = ["group/d"]
            [modules."group/d"]
            modules = ["key/F1", "key/F2", "key/F3"]
            drawer = { start-expanded = true, transition-left-to-right = false, children-class = "kid" }
            [[rules]]
            layout = "t"
            "#,
            std::path::Path::new("."),
        );
        assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
        let r = loaded.config.resolve("base", &Context::new()).unwrap();
        assert_eq!(start_expanded_drawers(&r.zones), ["group/d"]);

        let open = |k: &str| UiState {
            expanded: k == "group/d",
            ..UiState::default()
        };
        let mut out = Vec::new();
        let mut icons = Icons::builtin_only();
        cells(
            &r.zones[0].modules,
            &world(),
            &mut icons,
            36,
            &open,
            &mut out,
        );
        let keys: Vec<&str> = out.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            ["group/d/key/F2", "group/d/key/F3", "group/d/key/F1"],
            "opens towards the left: the leader comes last"
        );
        assert!(out[2].drawer.is_some(), "the leader is still the handle");
        assert!(out[0].drawer.is_none());
        assert!(out.iter().all(|c| c.classes.contains(&"kid".to_string())
            && !c.classes.contains(&"drawer-child".to_string())));
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
