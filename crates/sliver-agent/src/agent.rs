//! The agent core: config + context → resolved layers → frames, and touches → gestures →
//! actions. Sans-IO: the caller feeds it events and time, presents the frames it builds and
//! carries out the [`Effect`]s it returns (see `run` in main.rs and the tests).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local};
use sliver_config::{
    Action, Config, Context, CtxValue, Gesture as CGesture, Key, LayerMode, Resolved,
};
use sliver_ipc::proto::{DisplayState, KeyMode, Layer, TouchPhase};
use sliver_render::{Renderer, Stylesheet};

use crate::frame::{self, Frame};
use crate::gesture::{Bindings, Gesture, Recognizer};
use crate::icons::Icons;
use crate::modules::{self, Cell, UiState, World};

mod actions;
mod input;

use input::*;

/// Icons are looked up at this size unless a module says otherwise.
const ICON_LOOKUP_SIZE: u32 = 48;
/// Region ids of the `fn` layer start here, so touches are unambiguous whichever layer sliverd
/// shows.
pub const FN_REGION_BASE: u32 = 10_000;
/// Drag actions (`on-drag` commands) run at most this often; the final value always runs.
const DRAG_INTERVAL_US: u64 = 50_000;
/// An open menu closes on its own after this long, so an entry like "Close window" doesn't
/// stay armed on a bar nobody is looking at.
const MENU_TIMEOUT_US: u64 = 5_000_000;
/// After a slider is let go, how long it may keep showing the finger's value while the system
/// catches up before it shows what the system reports again.
const HELD_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Something for the caller to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Ask sliverd to press keys.
    EmitKeys(Vec<u16>, KeyMode),
    /// Start a process in the user session (argv).
    Spawn(Vec<String>),
    /// Something visible changed: render and present.
    Redraw,
    /// Only pressed feedback changed: [`Agent::render_press`] is enough (much cheaper).
    RedrawPress,
    /// Not fatal, worth logging.
    Log(String),
    /// Change session state (volume, brightness, media, inhibitors). Run off the main loop.
    Session(SessionOp),
    /// An action fired on a user module: re-run it (waybar `exec-on-event`).
    ModuleFired(String),
    /// Re-run user modules with `signal = N` (`{ signal = N }`, like `pkill -RTMIN+N waybar`).
    Signal(u32),
    /// Re-run one user module now (`{ update = "custom/x" }`).
    Update(String),
    /// Switch to a named profile (`{ profile = "work" }`): persisted, then hot-reloaded.
    Profile(String),
    /// Switch workspace or focus/close a window on the compositor (workspaces, taskbar).
    Desktop(sliver_desktop::control::Action),
    /// Tell the app that owns a published item what the user did (api.md §5).
    Api {
        context: sliver_api::registry::ContextId,
        item: String,
        what: sliver_api::registry::Interaction,
    },
    /// Mirror: the user touched this module of the held layout (`Control1.HeldPressed`).
    HeldPressed {
        module: String,
        zone: String,
        index: u32,
    },
}

/// Session-side changes the built-in modules make.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionOp {
    ScreenBrightness(u8),
    KeyboardBrightness(u8),
    Volume {
        mic: bool,
        pct: u8,
    },
    ToggleMute {
        mic: bool,
    },
    Media(sliver_session::mpris::Command),
    /// A transport command for a specific (picked) player.
    MediaOn {
        player: String,
        cmd: sliver_session::mpris::Command,
    },
    /// Seek the current (or picked) player to `pct` percent of the track.
    Seek {
        player: Option<String>,
        pct: u8,
    },
    ToggleCaffeine,
    ToggleWifi,
    ToggleBluetooth,
    ToggleDnd,
    ToggleDark,
    /// fcitx5: `None` toggles (activate/deactivate), `Some(id)` selects that input method.
    InputMethod(Option<String>),
    /// Invoke the latest notification's default action (where the daemon has a tool for it).
    OpenNotification,
    /// Close the latest notification.
    DismissNotification,
    /// A `{ dbus = {...} }` action.
    DBus(sliver_config::DBusCall),
}

pub struct Agent {
    pub config: Config,
    pub sheet: Stylesheet,
    renderer: Renderer,
    /// Character keys in combos follow the bar keyboard's layout, and the keymap they came
    /// from (rebuilt only when it changes).
    keymap: crate::keymap::KeyMapper,
    keymap_names: Option<sliver_desktop::XkbNames>,
    icons: Icons,
    pub width: u32,
    pub height: u32,
    /// The context the last render resolved with (built from the world, plus `ctx_overrides`).
    pub ctx: Context,
    /// Applied on top of the world's context (simulate one condition over live state).
    pub ctx_overrides: Vec<(String, CtxValue)>,
    /// Use exactly this context instead of building one from the world (the settings app's
    /// "Preview as").
    pub fixed_ctx: Option<Context>,
    /// Whether this Mac has a physical Esc key (decides the `esc` zone).
    pub physical_esc: bool,
    /// Custom layers opened over `base` (last = on top).
    pub layers: Vec<String>,
    ui: HashMap<String, UiState>,
    pub display: DisplayState,
    /// sliverd's safe hatch is showing its own layer.
    pub hatch: bool,
    /// Fn is held: sliverd shows our `fn` frame.
    pub fn_held: bool,
    frames: [Option<Frame>; 2],
    /// A base-layer change animating (`-sliver-layer-transition`), and the layer depth the base
    /// frame was last drawn at (deeper = slide forward).
    layer_anim: Option<frame::LayerAnim>,
    /// Tests: make the next frame build panic (to exercise the guard in `render`).
    #[cfg(test)]
    panic_in_build: bool,
    base_depth: usize,
    recognizer: Recognizer,
    /// Cell keys being touched (agent regions).
    pressed: HashSet<String>,
    /// Layer opened by a `hold`-mode action, closed when that region is released.
    hold_layer: Option<(u32, String)>,
    /// A `oneshot` layer: closes after the next action from it.
    oneshot: Option<String>,
    /// Macro steps and drawer closings due at a monotonic time (µs).
    scheduled: Vec<(u64, Scheduled)>,
    drag: HashMap<u32, DragState>,
    /// Press-and-slide anchors per region: (finger x, slider value 0–100) when sliding began.
    slide: HashMap<u32, (f32, f64)>,
    /// Published sliders being dragged: last value sent, by region.
    pub_drag: HashMap<u32, f64>,
    /// Problems resolving layers, for logging/status. Cleared on each render.
    pub errors: Vec<String>,
    /// `graph` modules' samples, by module id: oldest first, 0..=1, and when the last
    /// one was taken. Sampled in [`Agent::render`] while the bar is drawn.
    graphs: HashMap<String, (std::collections::VecDeque<f32>, Option<DateTime<Local>>)>,
    /// The emoji picker's recents (shared with the worlds it draws).
    emoji: modules::EmojiHandle,
    /// An open `menu` sub-bar: the entries it shows.
    menu: Option<Vec<sliver_config::MenuEntry>>,
    /// The button whose `menu` is open: its entries' module actions act on it (which window to
    /// close, for instance).
    menu_owner: Option<Cell>,
    /// Where that button was among the zones the menu covers (offset from the menu's start,
    /// width): the menu's ✕ takes its place, under the finger that opened it.
    menu_slot: Option<(f32, f32)>,
    /// Sliders under a finger, by cell key: the value there, and when it was let go. They show
    /// it at once (the system's level is set at most every [`DRAG_INTERVAL_US`] and reported back
    /// later), and after release until the system reports it or [`HELD_GRACE`] passes.
    under_finger: HashMap<String, (f32, Option<std::time::Instant>)>,
    /// An open popover a published item showed (api.md §3): context, popover item id, and the
    /// context's class (`app` / `status`).
    popover: Option<(sliver_api::registry::ContextId, String, String)>,
    /// An open bar-mode drawer: the group's key.
    subbar: Option<String>,
    /// `sliverctl bar hide`: the base layer shows nothing (Fn → F-keys still works).
    pub hidden: bool,
    /// `sliverctl hide MODULE`: module ids/keys hidden at runtime (not saved).
    pub hidden_modules: std::collections::BTreeSet<String>,
    /// `sliverctl layout NAME`: a copy of the config whose current layers show that layout.
    layout_override: Option<(String, Config)>,
    /// That override is held (`Control1.HoldLayout…`, Mirror to Touch Bar): touches on it go to
    /// the holder as `HeldPressed`. `Some(true)`: select only, nothing fires.
    held: Option<bool>,
    /// What the base layer resolved to last: its layout and the rule (index) that picked it.
    active: Option<(String, Option<usize>)>,
    /// `sliverctl update MODULE …`: output pushed to exec-less custom modules.
    pub pushed: HashMap<String, sliver_modules::Output>,
}

#[derive(Debug, Clone)]
enum Scheduled {
    Action(Action),
    CloseDrawer(String),
    CloseMenu,
    /// A running timer reaches zero (ignored if it was paused or reset since).
    TimerDone {
        key: String,
        generation: u64,
    },
}

#[derive(Debug, Default, Clone, Copy)]
struct DragState {
    last_run: Option<u64>,
    pending: Option<f64>,
}

impl Agent {
    pub fn new(
        config: Config,
        sheet: Stylesheet,
        renderer: Renderer,
        icons: Icons,
        width: u32,
        height: u32,
    ) -> Agent {
        // tiny-dfr's heuristic (docs/hardware.md): only the 2170 px bars lack a physical Esc.
        let physical_esc = width < 2170;
        let ctx = Context::new().with("physical_esc", physical_esc);
        Agent {
            ctx_overrides: Vec::new(),
            fixed_ctx: None,
            physical_esc,
            config,
            sheet,
            renderer,
            keymap: crate::keymap::KeyMapper::positional(),
            keymap_names: None,
            icons,
            width,
            height,
            ctx,
            layers: Vec::new(),
            ui: HashMap::new(),
            display: DisplayState::On,
            hatch: false,
            fn_held: false,
            frames: [None, None],
            layer_anim: None,
            #[cfg(test)]
            panic_in_build: false,
            base_depth: 0,
            recognizer: Recognizer::new(),
            pressed: HashSet::new(),
            hold_layer: None,
            oneshot: None,
            scheduled: Vec::new(),
            drag: HashMap::new(),
            slide: HashMap::new(),
            pub_drag: HashMap::new(),
            hidden: false,
            hidden_modules: Default::default(),
            layout_override: None,
            held: None,
            active: None,
            pushed: HashMap::new(),
            errors: Vec::new(),
            graphs: HashMap::new(),
            emoji: Default::default(),
            menu: None,
            menu_owner: None,
            menu_slot: None,
            under_finger: HashMap::new(),
            popover: None,
            subbar: None,
        }
    }

    /// The config layer currently shown in sliverd's `base` slot.
    pub fn base_layer(&self) -> &str {
        self.layers.last().map_or("base", String::as_str)
    }

    pub fn frame(&self, layer: Layer) -> Option<&Frame> {
        self.frames[slot(layer)].as_ref()
    }

    /// Takes a sample for each placed `graph` whose period has passed (renders happen at least
    /// that often while the bar is on: the graph's cell asks for it), and forgets graphs no
    /// longer placed.
    fn sample_graphs(&mut self, world: &World) {
        use crate::modules::{graph_params, graph_sample, is_graph};
        if !self.config.modules.keys().any(|id| is_graph(&id.kind)) {
            self.graphs.clear();
            return;
        }
        let placed: Vec<_> = crate::modules::placed_modules(&self.config)
            .into_iter()
            .filter(|(id, _)| is_graph(&id.kind))
            .collect();
        self.graphs
            .retain(|k, _| placed.iter().any(|(id, _)| id.to_string() == *k));
        for (id, block) in placed {
            let (every, cap) = graph_params(&block);
            let g = self.graphs.entry(id.to_string()).or_default();
            let due = g.1.is_none_or(|t| {
                (world.now - t)
                    .to_std()
                    .is_ok_and(|d| d + std::time::Duration::from_millis(50) >= every)
            });
            if !due {
                continue;
            }
            if let Some(v) = graph_sample(&self.config, &id, &block, world) {
                g.0.push_back(v);
                while g.0.len() > cap {
                    g.0.pop_front();
                }
                g.1 = Some(world.now);
            }
        }
    }

    /// The samples a `graph` cell draws: by its key, which ends with the module id (keys of
    /// group members are prefixed with the group's).
    fn graph_for(
        graphs: &HashMap<String, (std::collections::VecDeque<f32>, Option<DateTime<Local>>)>,
        config: &Config,
        key: &str,
    ) -> Option<(Vec<f32>, usize)> {
        let (id, (samples, _)) = graphs
            .iter()
            .find(|(id, _)| key == id.as_str() || key.ends_with(&format!("/{id}")))?;
        let block = config.modules.iter().find(|(m, _)| m.to_string() == *id)?.1;
        let (_, cap) = crate::modules::graph_params(block);
        Some((samples.iter().copied().collect(), cap))
    }

    /// The config `cava` should run with: while a spectrum (a `cava` module, a slider with
    /// `spectrum = true`) is on the layer being shown and the bar is lit. `None` stops it.
    pub fn spectrum_config(&self) -> Option<String> {
        if self.display == DisplayState::Off {
            return None;
        }
        let shown = self.frame(if self.fn_held { Layer::Fn } else { Layer::Base })?;
        let cell = shown.cells.iter().find(|c| modules::shows_spectrum(c))?;
        let id = sliver_config::ModuleId::parse(&cell.id).ok()?;
        let block = self
            .config
            .module_block(&sliver_config::ModuleRef::Named(id));
        Some(crate::modules::cava_config(&block))
    }

    pub fn ui(&mut self, key: &str) -> &mut UiState {
        self.ui.entry(key.to_owned()).or_default()
    }

    /// Re-resolves and re-renders both layers for `world`. Returns the layers whose frames were
    /// rebuilt (callers present them; the link drops unchanged pixels).
    pub fn render(&mut self, world: &World) -> Vec<Layer> {
        self.errors.clear();
        // Pixel shift: keep content clear of the ends sliverd moves it towards.
        self.renderer
            .set_extra_inset(crate::display::content_inset(&self.config.display));
        let mut ctx = match &self.fixed_ctx {
            Some(c) => c.clone(),
            None => crate::context::build(
                world.context.clone(),
                world.now,
                self.physical_esc,
                self.base_layer(),
                world.published.app.is_some(),
            ),
        };
        for (k, v) in &self.ctx_overrides {
            ctx.set(k, v.clone());
        }
        self.ctx = ctx;
        let bound = |g: crate::gesture::BarGesture| self.config.gestures.contains_key(g.name());
        use crate::gesture::BarGesture as BG;
        self.recognizer.set_bar(crate::gesture::BarBindings {
            two_finger: bound(BG::TwoFingerSwipeLeft) || bound(BG::TwoFingerSwipeRight),
            edge_left: bound(BG::EdgeSwipeFromLeft),
            edge_right: bound(BG::EdgeSwipeFromRight),
            width: self.width as f32,
        });
        // Locked: modules only ever see the sanitized world (lock-screen privacy, ux.md §5).
        let sanitized;
        let world = if self.ctx.flag("locked") {
            sanitized = crate::privacy::sanitize(world);
            &sanitized
        } else {
            world
        };
        self.sample_graphs(world);
        let mut out = Vec::new();
        let base = self.base_layer().to_owned();
        for (layer, name) in [(Layer::Base, base.as_str()), (Layer::Fn, "fn")] {
            let config = self
                .layout_override
                .as_ref()
                .map_or(&self.config, |(_, c)| c);
            match config.resolve(name, &self.ctx) {
                Ok(mut r) => {
                    if self.hidden && layer == Layer::Base {
                        r.zones.iter_mut().for_each(|z| z.modules.clear());
                    }
                    let id_base = if layer == Layer::Fn {
                        FN_REGION_BASE
                    } else {
                        0
                    };
                    if layer == Layer::Base {
                        let rule = r.rule.filter(|_| self.layout_override.is_none());
                        self.active = Some((r.layout.clone(), rule));
                    }
                    for key in modules::start_expanded_drawers(&r.zones) {
                        self.ui.entry(key).or_insert_with(|| UiState {
                            expanded: true,
                            ..UiState::default()
                        });
                    }
                    // A panic while building (a module bug, input nobody foresaw) must not take
                    // the agent down: systemd would restart it into the same panic, over and
                    // over. Keep showing this layer's last good frame and report it instead.
                    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.build(&r, world, id_base)
                    }));
                    let mut f = match built {
                        Ok(f) => f,
                        Err(p) => {
                            let msg = p
                                .downcast_ref::<String>()
                                .map(String::as_str)
                                .or_else(|| p.downcast_ref::<&str>().copied())
                                .unwrap_or("unknown panic");
                            self.errors
                                .push(format!("layer {name}: building the frame panicked: {msg}"));
                            continue;
                        }
                    };
                    if layer == Layer::Base {
                        self.animate_layer_change(&mut f);
                    }
                    self.frames[slot(layer)] = Some(f);
                    out.push(layer);
                }
                Err(e) => self.errors.push(format!("layer {name}: {e}")),
            }
        }
        // Scenes no longer shown (a layer left mid-fade) must not keep the loop awake.
        let shown: Vec<&sliver_render::Scene> =
            self.frames.iter().flatten().map(|f| &f.scene).collect();
        self.renderer.retain_scenes(&shown);
        out
    }

    /// Starts, continues or ends a layer-change animation on the new base frame `f`.
    fn animate_layer_change(&mut self, f: &mut Frame) {
        let now = std::time::Instant::now();
        let depth = self.layers.len();
        let old_depth = std::mem::replace(&mut self.base_depth, depth);
        if let Some(old) = &self.frames[0]
            && old.identity() != f.identity()
            && self.display != DisplayState::Off
            && let Some(t) = f.layout.layer_transition
        {
            let layer_changed = old.identity().0 != f.identity().0;
            let t = if layer_changed {
                t
            } else {
                // Only the layout changed (app/context rules): a cross-fade, not navigation.
                sliver_render::LayerTransition {
                    effect: sliver_render::LayerEffect::Fade,
                    ..t
                }
            };
            self.layer_anim = Some(frame::LayerAnim {
                // What's on the panel now, so an interrupted change continues smoothly.
                from: old.image.clone(),
                start: now,
                t,
                dir: if depth < old_depth { -1.0 } else { 1.0 },
                drawn: now,
            });
        }
        match self.layer_anim.as_mut().map(|a| (a.progress(now), a)) {
            Some((Some(e), a)) => {
                a.composite(&mut f.image, e);
                a.drawn = now;
            }
            Some((None, _)) => self.layer_anim = None,
            None => {}
        }
    }

    fn build(&mut self, r: &Resolved, world: &World, id_base: u32) -> Frame {
        #[cfg(test)]
        if self.panic_in_build {
            panic!("test panic in build");
        }
        // Published items the layout places itself (`app/<provider>#<item>`) leave the
        // App controls / Status items placeholders (api.md §3.1).
        fn pinned(ms: &[sliver_config::ModuleInstance], out: &mut Vec<(String, Option<String>)>) {
            for m in ms {
                if m.id.kind == "app"
                    && let Some(p) = &m.id.name
                {
                    out.push((p.clone(), m.id.instance.clone()));
                }
                pinned(&m.children, out);
            }
        }
        let mut pins = Vec::new();
        for z in &r.zones {
            pinned(&z.modules, &mut pins);
        }
        // Items configured by id (`[modules."app/…#…"]`) but shown in a placeholder.
        let configured: std::collections::HashMap<String, sliver_config::ModuleInstance> = self
            .config
            .modules
            .keys()
            .filter(|id| id.kind == "app" && id.instance.is_some())
            .filter(|id| {
                !pins.iter().any(|(p, i)| {
                    Some(p) == id.name.as_ref()
                        && i.as_ref().is_none_or(|i| Some(i) == id.instance.as_ref())
                })
            })
            .filter_map(|id| Some((id.to_string(), self.config.instance(id, &self.ctx)?)))
            .collect();
        let mut world_pushed;
        let world = if self.pushed.is_empty() && pins.is_empty() && configured.is_empty() {
            world
        } else {
            world_pushed = world.clone();
            for (k, v) in &self.pushed {
                world_pushed
                    .outputs
                    .entry(k.clone())
                    .or_insert_with(|| v.clone());
            }
            world_pushed.published.pinned = pins.into_iter().collect();
            world_pushed.published.configured = configured;
            &world_pushed
        };
        // A modal app context covers everything but the user's first zone (api.md §4).
        let modal = (id_base == 0 && world.published.modal)
            .then(|| world.published.app.clone())
            .flatten();
        // An open popover replaces everything but the first zone, over a modal context too.
        let mut popover = match (id_base, self.popover.clone()) {
            (0, Some((ctx, item, class))) => {
                let cells = world
                    .published
                    .contexts()
                    .find(|c| c.id == ctx)
                    .and_then(|c| {
                        crate::published::popover_cells(
                            c,
                            &item,
                            &class,
                            &mut self.icons,
                            ICON_LOOKUP_SIZE,
                        )
                    });
                if cells.is_none() {
                    // Its context or item went away (app closed, focus moved): it's closed.
                    self.popover = None;
                }
                cells
            }
            _ => None,
        };
        let modal = if popover.is_some() { None } else { modal };
        let mut zones = r.zones.clone();
        if popover.is_some() && !zones.is_empty() {
            zones.truncate(1);
            zones.push(sliver_config::ResolvedZone {
                name: "popover".into(),
                width: sliver_config::Width::Fill(1),
                align: sliver_config::Align::Start,
                class: vec!["menu".into(), "popover".into()],
                modules: Vec::new(),
            });
        }
        let menu = (id_base == 0 && modal.is_none() && popover.is_none())
            .then(|| self.menu.clone())
            .flatten();
        let subbar = (id_base == 0 && modal.is_none() && popover.is_none())
            .then(|| self.subbar.clone())
            .flatten();
        if (menu.is_some() || subbar.is_some()) && !zones.is_empty() {
            // A menu's entries fit their labels, so the layout's last zone (volume, say) stays
            // too, unless it's the only one to cover; a drawer's items take the room.
            let last = (menu.is_some() && zones.len() > 2)
                .then(|| zones.pop())
                .flatten();
            zones.truncate(1);
            zones.push(sliver_config::ResolvedZone {
                name: "menu".into(),
                width: sliver_config::Width::Fill(1),
                align: sliver_config::Align::Start,
                class: vec!["menu".into()],
                modules: Vec::new(),
            });
            zones.extend(last);
        }
        if modal.is_some() && !zones.is_empty() {
            zones.truncate(1);
            zones.push(sliver_config::ResolvedZone {
                name: "modal".into(),
                width: sliver_config::Width::Fill(1),
                align: sliver_config::Align::Start,
                class: vec!["modal".into()],
                modules: Vec::new(),
            });
        }
        let ui = &self.ui;
        let (graphs, config) = (&self.graphs, &self.config);
        let lookup = |k: &str| {
            let mut s = ui.get(k).cloned().unwrap_or_default();
            if !graphs.is_empty() {
                s.graph = Self::graph_for(graphs, config, k);
            }
            s
        };
        let hidden = &self.hidden_modules;
        let per_zone: Vec<Vec<Cell>> = zones
            .iter()
            .map(|z| {
                if z.name == "popover"
                    && let Some(mut cells) = popover.take()
                {
                    for c in &mut cells {
                        c.pressed = self.pressed.contains(&c.key);
                    }
                    return cells;
                }
                if z.name == "menu"
                    && menu.is_none()
                    && let Some(key) = &subbar
                {
                    // Looked up in the full zone list (the sub-bar replaces all but the first).
                    let mut cells = crate::modules::subbar_cells(
                        &r.zones,
                        key,
                        world,
                        &mut self.icons,
                        ICON_LOOKUP_SIZE,
                        &lookup,
                    )
                    .unwrap_or_default();
                    for c in &mut cells {
                        c.pressed = self.pressed.contains(&c.key);
                    }
                    return cells;
                }
                if z.name == "menu"
                    && let Some(entries) = &menu
                {
                    let mut cells = crate::modules::menu_cells(
                        entries,
                        self.menu_slot,
                        &mut self.icons,
                        ICON_LOOKUP_SIZE,
                    );
                    for c in &mut cells {
                        c.pressed = self.pressed.contains(&c.key);
                    }
                    return cells;
                }
                if z.name == "modal"
                    && let Some(ctx) = &modal
                {
                    let mut cells = Vec::new();
                    crate::published::cells(
                        ctx,
                        "app",
                        true,
                        &mut self.icons,
                        ICON_LOOKUP_SIZE,
                        &|_| false,
                        &mut cells,
                    );
                    cells.push(crate::published::close_cell(
                        ctx,
                        &mut self.icons,
                        ICON_LOOKUP_SIZE,
                    ));
                    for c in &mut cells {
                        c.pressed = self.pressed.contains(&c.key);
                    }
                    return cells;
                }
                let mut cells = Vec::new();
                modules::cells(
                    &z.modules,
                    world,
                    &mut self.icons,
                    ICON_LOOKUP_SIZE,
                    &lookup,
                    &mut cells,
                );
                cells.retain(|c| !hidden.contains(&c.id) && !hidden.contains(&c.key));
                for c in &mut cells {
                    c.pressed = self.pressed.contains(&c.key);
                    if let (Some(&(v, up)), sliver_render::Content::Slider { value, .. }) =
                        (self.under_finger.get(&c.key), &mut c.content)
                    {
                        let caught_up = (*value - v).abs() < 0.015;
                        if up.is_some_and(|t| caught_up || t.elapsed() > HELD_GRACE) {
                            self.under_finger.remove(&c.key);
                        } else {
                            *value = v;
                        }
                    }
                }
                cells
            })
            .collect();
        let mut classes = vec![format!("layer-{}", r.layer), format!("layout-{}", r.layout)];
        if modal.is_some() {
            classes.push("modal".into());
        }
        let mut per_zone = per_zone;
        // Fast-path key regions (sliverd presses them itself) get layout-mapped keys too.
        if world.desktop.bar_keymap != self.keymap_names {
            self.keymap_names = world.desktop.bar_keymap.clone();
            self.keymap = match &self.keymap_names {
                Some(n) => crate::keymap::KeyMapper::new(n),
                None => crate::keymap::KeyMapper::positional(),
            };
        }
        for c in per_zone.iter_mut().flatten() {
            if let crate::modules::Touch::Keys(keys) = &mut c.touch {
                *keys = self.keymap.map_all(keys);
            }
        }
        // Mirror: every item of the held layout reports its touches (`HeldPressed`), so none
        // stays sliverd-only (keys then fire from the agent) or untouchable.
        if self.held.is_some() && id_base == 0 {
            for c in per_zone.iter_mut().flatten() {
                c.touch = crate::modules::Touch::Agent;
            }
        }
        if self.ctx.flag("locked") {
            for cells in &mut per_zone {
                cells.retain(crate::privacy::allowed);
            }
        }
        self.fit_slots(&zones, &mut per_zone, &classes);
        // Published items with a lower `priority` give way when their zone overflows (api.md §3),
        // a group as a unit. Only measured when some priority differs.
        if per_zone.iter().flatten().any(|c| {
            c.published
                .as_ref()
                .is_some_and(|p| p.priority != 0 && p.priority != i32::MAX)
        }) {
            for _ in 0..crate::published::MAX_OVERFLOW_DROPS {
                let probe = frame::scene(&zones, &per_zone, classes.clone());
                let layout = self
                    .renderer
                    .layout(&probe, &self.sheet, self.width, self.height);
                let Some((zi, ctx, unit)) =
                    crate::published::overflow_victim(&per_zone, &layout.overflow)
                else {
                    break;
                };
                per_zone[zi].retain(|c| {
                    !c.published
                        .as_ref()
                        .is_some_and(|p| p.context == ctx && p.unit == unit)
                });
            }
        }
        let scene = frame::scene(&zones, &per_zone, classes);
        let cells = per_zone.into_iter().flatten().collect();
        let mut f = frame::render(
            &mut self.renderer,
            &self.sheet,
            &scene,
            cells,
            self.width,
            self.height,
            id_base,
        );
        if self.recognizer.bar_bound() {
            f.fill_gaps();
        }
        f
    }

    /// Fixed-width App controls / Status items slots (`width = 332`): measure the items, give
    /// the trailing spacer what's left, and if they don't fit, drop the lowest-priority item (a
    /// group as a unit) until they do. Fill slots need nothing: their spacer is `Fill`.
    fn fit_slots(
        &mut self,
        zones: &[sliver_config::ResolvedZone],
        per_zone: &mut [Vec<Cell>],
        classes: &[String],
    ) {
        use sliver_render::Width;
        let is_px_spacer = |c: &Cell| c.kind == "slot" && matches!(c.width, Width::Fixed(_));
        // The slots' sizes, then measure with their spacers at zero.
        let mut want: HashMap<String, f32> = HashMap::new();
        for c in per_zone.iter_mut().flatten().filter(|c| is_px_spacer(c)) {
            if let (Width::Fixed(w), Some(s)) = (c.width, &c.slot) {
                want.insert(s.clone(), w);
                c.width = Width::Fixed(0.0);
            }
        }
        if want.is_empty() {
            return;
        }
        for _ in 0..crate::published::MAX_OVERFLOW_DROPS {
            let probe = frame::scene(zones, per_zone, classes.to_vec());
            let layout = self
                .renderer
                .layout(&probe, &self.sheet, self.width, self.height);
            // Per slot: from its first item's left edge to the spacer's (gap included).
            let mut too_big: Option<String> = None;
            let mut sizes: Vec<(usize, usize, f32)> = Vec::new();
            let mut i = 0;
            for (zi, cells) in per_zone.iter().enumerate() {
                let mut first: HashMap<&str, f32> = HashMap::new();
                for (ci, c) in cells.iter().enumerate() {
                    let r = layout.items[i + ci];
                    let Some(s) = c.slot.as_deref() else { continue };
                    if c.kind != "slot" {
                        first.entry(s).or_insert(r.x);
                    } else if let Some(&w) = want.get(s) {
                        let used = first.get(s).map_or(0.0, |x0| r.x - x0);
                        if used > w + 0.5 {
                            too_big.get_or_insert_with(|| s.to_string());
                        }
                        sizes.push((zi, ci, (w - used).max(0.0)));
                    }
                }
                i += cells.len();
            }
            let Some(slot) = too_big else {
                for (zi, ci, rest) in sizes {
                    per_zone[zi][ci].width = Width::Fixed(rest.floor());
                }
                return;
            };
            // Drop the lowest-priority item of the overfull slot (the last one on a tie).
            let victim = per_zone
                .iter()
                .flatten()
                .filter(|c| c.slot.as_deref() == Some(slot.as_str()) && c.kind != "slot")
                .filter_map(|c| c.published.as_ref())
                .rev()
                .min_by_key(|p| p.priority)
                .map(|p| (p.context, p.unit.clone()));
            let Some((ctx, unit)) = victim else { return };
            for cells in per_zone.iter_mut() {
                cells.retain(|c| {
                    c.slot.as_deref() != Some(slot.as_str())
                        || !c
                            .published
                            .as_ref()
                            .is_some_and(|p| p.context == ctx && p.unit == unit)
                });
            }
        }
    }

    /// Re-renders just the images of frames whose pressed cells changed (after
    /// [`Effect::RedrawPress`]). Returns the layers to present.
    pub fn render_press(&mut self) -> Vec<Layer> {
        // A touch ends a layer change at once: the press draws on the settled layer.
        self.layer_anim = None;
        let pressed = &self.pressed;
        let is_pressed = |c: &Cell| pressed.contains(&c.key);
        let mut out = Vec::new();
        for (i, layer) in [Layer::Base, Layer::Fn].into_iter().enumerate() {
            if let Some(f) = &mut self.frames[i]
                && f.repress(&mut self.renderer, &self.sheet, &is_pressed)
            {
                out.push(layer);
            }
        }
        out
    }

    /// When some module's content changes on its own next (the earliest clock tick), if ever.
    /// Nothing ticks while the display is off (invariant 3).
    pub fn next_change(&self) -> Option<DateTime<Local>> {
        if self.display == DisplayState::Off {
            return None;
        }
        self.frames
            .iter()
            .flatten()
            .flat_map(|f| f.cells.iter().filter_map(|c| c.next_change))
            .min()
    }

    /// The next monotonic time (µs) [`Agent::tick`] has work: holds, double taps, macro steps,
    /// drawer closings.
    /// When a running style transition needs its next frame (`None`: at rest, or the display is
    /// off, so nothing wakes the loop).
    pub fn animation_deadline(&self) -> Option<std::time::Instant> {
        if self.display == DisplayState::Off {
            return None;
        }
        let layer = self
            .layer_anim
            .as_ref()
            .map(|a| a.drawn + sliver_render::FRAME);
        self.renderer
            .animation_deadline()
            .into_iter()
            .chain(layer)
            .min()
    }

    pub fn next_deadline(&self) -> Option<u64> {
        let drag = self
            .drag
            .values()
            .filter(|d| d.pending.is_some())
            .filter_map(|d| d.last_run.map(|t| t + DRAG_INTERVAL_US));
        self.recognizer
            .next_deadline()
            .into_iter()
            .chain(self.scheduled.iter().map(|s| s.0))
            .chain(drag)
            .min()
    }

    /// Runs whatever is due at `now` (monotonic µs).
    pub fn tick(&mut self, now: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        for (region, g) in self.recognizer.tick(now) {
            self.dispatch(region, g, now, &mut fx);
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.scheduled)
            .into_iter()
            .partition(|s| s.0 <= now);
        self.scheduled = later;
        for (_, s) in due {
            match s {
                Scheduled::Action(a) => self.execute(&a, None, None, now, &mut fx),
                Scheduled::TimerDone { key, generation } => {
                    if let Some(t) = self.ui(&key).timer.as_mut()
                        && t.generation == generation
                        && t.running_since.is_some()
                    {
                        t.running_since = None;
                        t.remaining = std::time::Duration::ZERO;
                        t.finished = true;
                        fx.push(Effect::Redraw);
                        fx.push(Effect::Spawn(
                            [
                                "notify-send",
                                "-a",
                                "Sliver",
                                "-i",
                                "alarm-symbolic",
                                "Timer finished",
                            ]
                            .map(String::from)
                            .into(),
                        ));
                    }
                }
                Scheduled::CloseDrawer(g) => {
                    if std::mem::take(&mut self.ui(&g).expanded) {
                        fx.push(Effect::Redraw);
                    }
                }
                Scheduled::CloseMenu => {
                    if self.menu.take().is_some() {
                        self.menu_owner = None;
                        self.menu_slot = None;
                        fx.push(Effect::Redraw);
                    }
                }
            }
        }
        let flush: Vec<u32> = self
            .drag
            .iter()
            .filter(|(_, d)| {
                d.pending.is_some() && d.last_run.is_some_and(|t| t + DRAG_INTERVAL_US <= now)
            })
            .map(|(r, _)| *r)
            .collect();
        for r in flush {
            self.run_drag(r, None, now, true, &mut fx);
        }
        fx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A panic while building a frame is caught: the layer keeps its last good frame, the
    /// error is reported, and the next render works again.
    #[test]
    fn a_panicking_build_keeps_the_last_frame() {
        let l = sliver_config::load_str("", std::path::Path::new("."));
        let mut a = Agent::new(
            l.config,
            Stylesheet::sliver(),
            Renderer::new(sliver_render::Fonts::embedded()),
            crate::icons::Icons::builtin_only(),
            2008,
            60,
        );
        let w = World::at(
            chrono::Local
                .with_ymd_and_hms(2026, 10, 12, 9, 41, 0)
                .unwrap(),
        );
        assert_eq!(a.render(&w), [Layer::Base, Layer::Fn]);
        let good = a.frame(Layer::Base).unwrap().image.clone();
        a.panic_in_build = true;
        assert!(a.render(&w).is_empty(), "nothing new to present");
        assert!(
            a.errors.iter().any(|e| e.contains("panicked")),
            "{:?}",
            a.errors
        );
        assert_eq!(a.frame(Layer::Base).unwrap().image.data(), good.data());
        a.panic_in_build = false;
        assert_eq!(a.render(&w), [Layer::Base, Layer::Fn]);
        assert!(a.errors.is_empty());
    }
}
