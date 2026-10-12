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

/// Icons are looked up at this size unless a module says otherwise.
const ICON_LOOKUP_SIZE: u32 = 48;
/// Region ids of the `fn` layer start here, so touches are unambiguous whichever layer sliverd
/// shows.
pub const FN_REGION_BASE: u32 = 10_000;
/// Drag actions (`on-drag` commands) run at most this often; the final value always runs.
const DRAG_INTERVAL_US: u64 = 50_000;

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
    /// Invoke the latest notification's default action (where the daemon has a tool for it).
    OpenNotification,
    /// Close the latest notification.
    DismissNotification,
}

pub struct Agent {
    pub config: Config,
    pub sheet: Stylesheet,
    renderer: Renderer,
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
    /// The emoji picker's recents (shared with the worlds it draws).
    emoji: modules::EmojiHandle,
    /// An open `menu` sub-bar: the entries it shows.
    menu: Option<Vec<sliver_config::MenuEntry>>,
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
    /// `sliverctl update MODULE …`: output pushed to exec-less custom modules.
    pub pushed: HashMap<String, sliver_modules::Output>,
}

#[derive(Debug, Clone)]
enum Scheduled {
    Action(Action),
    CloseDrawer(String),
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
            pushed: HashMap::new(),
            errors: Vec::new(),
            emoji: Default::default(),
            menu: None,
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

    pub fn ui(&mut self, key: &str) -> &mut UiState {
        self.ui.entry(key.to_owned()).or_default()
    }

    /// Re-resolves and re-renders both layers for `world`. Returns the layers whose frames were
    /// rebuilt (callers present them; the link drops unchanged pixels).
    pub fn render(&mut self, world: &World) -> Vec<Layer> {
        self.errors.clear();
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
                    let mut f = self.build(&r, world, id_base);
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
            });
        }
        match self.layer_anim.as_ref().map(|a| (a, a.progress(now))) {
            Some((a, Some(e))) => a.composite(&mut f.image, e),
            Some((_, None)) => self.layer_anim = None,
            None => {}
        }
    }

    fn build(&mut self, r: &Resolved, world: &World, id_base: u32) -> Frame {
        let mut world_pushed;
        let world = if self.pushed.is_empty() {
            world
        } else {
            world_pushed = world.clone();
            for (k, v) in &self.pushed {
                world_pushed
                    .outputs
                    .entry(k.clone())
                    .or_insert_with(|| v.clone());
            }
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
            zones.truncate(1);
            zones.push(sliver_config::ResolvedZone {
                name: "menu".into(),
                width: sliver_config::Width::Fill(1),
                align: sliver_config::Align::Start,
                class: vec!["menu".into()],
                modules: Vec::new(),
            });
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
        let lookup = |k: &str| ui.get(k).cloned().unwrap_or_default();
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
                    let mut cells =
                        crate::modules::menu_cells(entries, &mut self.icons, ICON_LOOKUP_SIZE);
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
                }
                cells
            })
            .collect();
        let mut classes = vec![format!("layer-{}", r.layer), format!("layout-{}", r.layout)];
        if modal.is_some() {
            classes.push("modal".into());
        }
        let scene = frame::scene(&zones, &per_zone, classes);
        let cells = per_zone.into_iter().flatten().collect();
        frame::render(
            &mut self.renderer,
            &self.sheet,
            &scene,
            cells,
            self.width,
            self.height,
            id_base,
        )
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
            .map(|_| std::time::Instant::now() + sliver_render::FRAME);
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

    /// A touch on one of our agent regions (sliverd's `touch`, timestamps in µs).
    pub fn touch(
        &mut self,
        region: u32,
        slot: u8,
        phase: TouchPhase,
        x: f32,
        time_us: u64,
    ) -> Vec<Effect> {
        let mut fx = Vec::new();
        let Some(cell) = self.cell(region) else {
            return fx;
        };
        let b = bindings(cell);
        for (r, g) in self.recognizer.touch(region, b, slot, phase, x, time_us) {
            if let Gesture::Bar(bg) = g {
                self.bar_gesture(bg, time_us, &mut fx);
            } else {
                self.dispatch(r, g, time_us, &mut fx);
            }
        }
        fx
    }

    /// A bar-wide gesture: run its `[gestures]` action.
    fn bar_gesture(&mut self, g: crate::gesture::BarGesture, now: u64, fx: &mut Vec<Effect>) {
        if let Some(a) = self.config.gestures.get(g.name()).cloned() {
            self.execute(&a, None, None, now, fx);
        }
    }

    /// Display off, hatch, layer switch: forget touches and pressed feedback.
    pub fn cancel_touches(&mut self) -> Vec<Effect> {
        let had = !self.pressed.is_empty();
        self.recognizer.cancel_all();
        self.pressed.clear();
        self.hold_layer = None;
        if had {
            vec![Effect::Redraw]
        } else {
            Vec::new()
        }
    }

    fn frame_for(&self, region: u32) -> Option<&Frame> {
        let slot = if region > FN_REGION_BASE { 1 } else { 0 };
        self.frames[slot].as_ref()
    }

    fn cell(&self, region: u32) -> Option<&Cell> {
        let f = self.frame_for(region)?;
        f.cells.get(f.cell_index(region)?)
    }

    fn dispatch(&mut self, region: u32, g: Gesture, now: u64, fx: &mut Vec<Effect>) {
        let Some(cell) = self.cell(region).cloned() else {
            return;
        };
        if let Some(p) = &cell.published
            && !matches!(g, Gesture::Pressed(_))
        {
            self.dispatch_published(region, &cell, p, g, now, fx);
            return;
        }
        let want = match g {
            Gesture::Bar(_) => return, // handled in `touch`
            Gesture::Pressed(on) => {
                let changed = if on {
                    self.pressed.insert(cell.key.clone())
                } else {
                    self.pressed.remove(&cell.key)
                };
                if changed {
                    fx.push(Effect::RedrawPress);
                }
                if !on {
                    self.slide.remove(&region);
                }
                return;
            }
            Gesture::Slide(x) => {
                // The slider opened under the finger (press on its icon). Move the value by
                // how far the finger slid, so touching it never jumps the value.
                let f = self.frame_for(region).expect("cell came from a frame");
                let i = f.cell_index(region).expect("cell came from a frame");
                let (Some(t), sliver_render::Content::Slider { value, .. }) =
                    (f.layout.tracks.get(i).copied().flatten(), &cell.content)
                else {
                    return; // Not open (yet): nothing to slide.
                };
                if t.w <= 0.0 {
                    return;
                }
                let (x0, v0) = *self
                    .slide
                    .entry(region)
                    .or_insert((x, (*value as f64 * 100.0).round()));
                let v = (v0 + ((x - x0) / t.w) as f64 * 100.0)
                    .clamp(0.0, 100.0)
                    .round();
                self.keep_drawer_open(&cell, now);
                self.run_drag(region, Some(v), now, false, fx);
                return;
            }
            Gesture::Drag(x) => {
                let f = self.frame_for(region).expect("cell came from a frame");
                let i = f.cell_index(region).expect("cell came from a frame");
                // Sliders report 0–100 along their trough, other modules the bar x.
                let value = match f.layout.tracks.get(i).copied().flatten() {
                    Some(t) if t.w > 0.0 => {
                        (((x - t.x) / t.w).clamp(0.0, 1.0) * 100.0).round() as f64
                    }
                    _ => x.round() as f64,
                };
                self.keep_drawer_open(&cell, now);
                self.run_drag(region, Some(value), now, false, fx);
                return;
            }
            Gesture::Release => {
                if let Some((r, name)) = &self.hold_layer
                    && *r == region
                {
                    if self.layers.last() == Some(name) {
                        self.layers.pop();
                        fx.extend(self.cancel_touches());
                        fx.push(Effect::Redraw);
                    }
                    self.hold_layer = None;
                }
                if self.drag.get(&region).is_some_and(|d| d.pending.is_some()) {
                    self.run_drag(region, None, now, true, fx);
                }
                CGesture::Release
            }
            Gesture::Tap => CGesture::Tap,
            Gesture::DoubleTap => CGesture::DoubleTap,
            Gesture::Hold => CGesture::Hold,
            Gesture::SwipeLeft => CGesture::SwipeLeft,
            Gesture::SwipeRight => CGesture::SwipeRight,
        };
        let action = cell
            .on
            .iter()
            .find(|(cg, _)| *cg == want)
            .map(|(_, a)| a.clone());
        match (action, &cell.drawer) {
            (Some(a), _) => {
                let layer_before = self.base_layer().to_owned();
                self.execute(&a, None, Some(region), now, fx);
                if cell.kind == "custom" {
                    fx.push(Effect::ModuleFired(cell.id.clone()));
                }
                // A oneshot layer closes after the first action taken from it.
                if self.oneshot.as_deref() == Some(layer_before.as_str())
                    && region <= FN_REGION_BASE
                    && !matches!(a, Action::Layer { .. })
                {
                    self.oneshot = None;
                    if self.layers.last() == Some(&layer_before) {
                        self.layers.pop();
                        fx.extend(self.cancel_touches());
                        fx.push(Effect::Redraw);
                    }
                }
            }
            (None, Some(d)) if want == CGesture::Tap && d.bar => {
                self.subbar = Some(d.group.clone());
                fx.extend(self.cancel_touches());
                fx.push(Effect::Redraw);
            }
            (None, Some(d)) if want == CGesture::Tap => {
                let open = !self.ui(&d.group).expanded;
                self.ui(&d.group).expanded = open;
                self.scheduled
                    .retain(|s| !matches!(&s.1, Scheduled::CloseDrawer(g) if *g == d.group));
                if open && let Some(after) = d.collapse_after {
                    self.scheduled.push((
                        now + after.as_micros() as u64,
                        Scheduled::CloseDrawer(d.group.clone()),
                    ));
                }
                fx.push(Effect::Redraw);
            }
            _ => {}
        }
    }

    /// An auto-closing drawer (an opened collapsed slider) stays open while it's being used.
    fn keep_drawer_open(&mut self, cell: &Cell, now: u64) {
        let Some(d) = &cell.drawer else { return };
        let Some(after) = d.collapse_after else {
            return;
        };
        if !self.ui(&d.group).expanded {
            return;
        }
        self.scheduled
            .retain(|s| !matches!(&s.1, Scheduled::CloseDrawer(g) if *g == d.group));
        self.scheduled.push((
            now + after.as_micros() as u64,
            Scheduled::CloseDrawer(d.group.clone()),
        ));
    }

    /// Runs `on-drag` with `value` now if allowed, else keeps it pending. `flush` runs the
    /// pending value.
    fn run_drag(
        &mut self,
        region: u32,
        value: Option<f64>,
        now: u64,
        flush: bool,
        fx: &mut Vec<Effect>,
    ) {
        let d = self.drag.entry(region).or_default();
        if let Some(v) = value {
            d.pending = Some(v);
        }
        let due = flush || d.last_run.is_none_or(|t| t + DRAG_INTERVAL_US <= now);
        if !due {
            return;
        }
        let Some(v) = d.pending.take() else { return };
        d.last_run = Some(now);
        let action = self.cell(region).and_then(|c| {
            c.on.iter()
                .find(|(g, _)| *g == CGesture::Drag)
                .map(|(_, a)| a.clone())
        });
        if let Some(a) = action {
            self.execute(&a, Some(v), Some(region), now, fx);
        }
    }

    fn execute(
        &mut self,
        a: &Action,
        value: Option<f64>,
        region: Option<u32>,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        let subst = |s: &str| match value {
            Some(v) => s.replace("{value}", &format!("{v}")),
            None => s.to_owned(),
        };
        match a {
            Action::Key(k) => fx.push(Effect::EmitKeys(vec![k.code()], KeyMode::Tap)),
            Action::Keys(ks) => fx.push(Effect::EmitKeys(
                ks.iter().map(|k| k.code()).collect(),
                KeyMode::Tap,
            )),
            Action::Shell(cmd) => {
                fx.push(Effect::Spawn(vec!["sh".into(), "-c".into(), subst(cmd)]))
            }
            Action::Exec(argv) => fx.push(Effect::Spawn(argv.iter().map(|s| subst(s)).collect())),
            Action::Open(app) => fx.push(Effect::Spawn(vec!["gtk-launch".into(), app.clone()])),
            Action::Url(url) => fx.push(Effect::Spawn(vec!["xdg-open".into(), url.clone()])),
            Action::Layer { name, mode } => {
                if !self.config.layers.contains_key(name) {
                    fx.push(Effect::Log(format!("no layer {name:?} in [layers]")));
                    return;
                }
                match mode {
                    LayerMode::Toggle if self.layers.last() == Some(name) => {
                        self.layers.pop();
                    }
                    _ => {
                        self.layers.retain(|l| l != name);
                        self.layers.push(name.clone());
                    }
                }
                match mode {
                    LayerMode::Hold => self.hold_layer = region.map(|r| (r, name.clone())),
                    LayerMode::Oneshot => self.oneshot = Some(name.clone()),
                    LayerMode::Toggle => {}
                }
                // The visible base frame changes owner. Keep the hold layer's own touch alive.
                if *mode != LayerMode::Hold {
                    fx.extend(self.cancel_touches());
                }
                fx.push(Effect::Redraw);
            }
            Action::CloseLayer => {
                if self.layers.pop().is_some() {
                    fx.extend(self.cancel_touches());
                    fx.push(Effect::Redraw);
                }
            }
            Action::ToggleAlt => {
                if let Some(c) = region.and_then(|r| self.cell(r)).map(|c| c.key.clone()) {
                    let ui = self.ui(&c);
                    ui.show_alt = !ui.show_alt;
                    fx.push(Effect::Redraw);
                }
            }
            Action::Macro(steps) => {
                let mut at = now;
                for s in steps {
                    match s {
                        Action::Sleep(d) => at += d.as_micros() as u64,
                        a if at == now => self.execute(a, value, region, now, fx),
                        a => self.scheduled.push((at, Scheduled::Action(a.clone()))),
                    }
                }
            }
            Action::Sleep(_) => {}
            Action::DBus(c) if c.args.is_empty() => fx.push(Effect::Spawn(vec![
                "busctl".into(),
                if c.system_bus { "--system" } else { "--user" }.into(),
                "call".into(),
                c.dest.clone(),
                c.path.clone(),
                c.interface.clone(),
                c.method.clone(),
            ])),
            Action::Layout(name) => match self.set_layout(name) {
                Ok(()) => {
                    fx.extend(self.cancel_touches());
                    fx.push(Effect::Redraw);
                }
                Err(e) => fx.push(Effect::Log(format!("layout: {e}"))),
            },
            Action::Profile(name) => fx.push(Effect::Profile(name.clone())),
            Action::Signal(n) => fx.push(Effect::Signal(*n)),
            Action::Update(m) => fx.push(Effect::Update(m.clone())),
            Action::ModuleAction(name) => {
                let Some(cell) = region.and_then(|r| self.cell(r)).cloned() else {
                    return;
                };
                self.module_action(&cell, name, value, now, fx);
            }
            other => fx.push(Effect::Log(format!("action not supported yet: {other:?}"))),
        }
    }
}

impl Agent {
    /// Share the emoji picker's recents with the worlds this agent draws (main does this once).
    pub fn set_emoji(&mut self, handle: modules::EmojiHandle) {
        self.emoji = handle;
    }

    /// Relative icon paths in the config resolve against this (the config file's directory).
    pub fn set_config_dir(&mut self, dir: Option<&std::path::Path>) {
        self.icons.set_base_dir(dir);
    }

    /// The desktop's reduce-motion setting: transitions (and layer slides) jump to their end.
    pub fn set_reduce_motion(&mut self, on: bool) {
        self.renderer.set_reduce_motion(on);
    }

    /// `Control1.SetLayout`: show `layout` on the current base layer until the next call (an
    /// empty name goes back to the rules).
    pub fn set_layout(&mut self, layout: &str) -> Result<(), String> {
        if layout.is_empty() {
            self.layout_override = None;
            return Ok(());
        }
        self.config
            .layout_zones(layout)
            .map_err(|e| e.to_string())?;
        let layer = self.base_layer().to_owned();
        let mut c = self.config.clone();
        c.layers.insert(layer.clone(), layout.to_owned());
        c.rules.retain(|r| r.layer != layer);
        self.layout_override = Some((layout.to_owned(), c));
        Ok(())
    }

    /// The layout forced by [`Agent::set_layout`], if any.
    pub fn layout_override(&self) -> Option<&str> {
        self.layout_override.as_ref().map(|(n, _)| n.as_str())
    }

    /// Runs an action that didn't come from a touch (D-Bus `Control1`, sliverctl).
    pub fn run_action(&mut self, a: &Action, now: u64, fx: &mut Vec<Effect>) {
        self.execute(a, None, None, now, fx);
    }

    /// `Control1.Trigger(module, gesture)`: as if the user did it on the module's cell.
    pub fn trigger(
        &mut self,
        module: &str,
        g: sliver_api::item::Gesture,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        use sliver_api::item::Gesture as AG;
        let found = [Layer::Base, Layer::Fn].iter().find_map(|l| {
            let f = self.frame(*l)?;
            let i = f
                .cells
                .iter()
                .position(|c| c.id == module || c.key == module)?;
            Some(f.id_base + i as u32 + 1)
        });
        let Some(region) = found else {
            fx.push(Effect::Log(format!(
                "trigger: no module {module:?} on the bar"
            )));
            return;
        };
        let g = match g {
            AG::Tap => Gesture::Tap,
            AG::Hold => Gesture::Hold,
            AG::SwipeLeft => Gesture::SwipeLeft,
            AG::SwipeRight => Gesture::SwipeRight,
            AG::DoubleTap => Gesture::DoubleTap,
            AG::Drag => return,
        };
        self.dispatch(region, g, now, fx);
    }

    /// What's on the bar now, for `Control1.ListModules` / `sliverctl modules`.
    pub fn list_modules(&self) -> Vec<sliver_api::item::Props> {
        use sliver_api::item::Prop;
        [(Layer::Base, "base"), (Layer::Fn, "fn")]
            .iter()
            .filter_map(|(l, name)| Some((self.frame(*l)?, *name)))
            .flat_map(|(f, layer)| {
                f.cells.iter().zip(&f.layout.items).map(move |(c, r)| {
                    [
                        ("id", Prop::Str(c.id.clone())),
                        ("key", Prop::Str(c.key.clone())),
                        ("type", Prop::Str(c.kind.clone())),
                        ("layer", Prop::Str(layer.into())),
                        (
                            "classes",
                            Prop::List(c.classes.iter().cloned().map(Prop::Str).collect()),
                        ),
                        ("x", Prop::Int(r.x as i64)),
                        ("width", Prop::Int(r.w as i64)),
                    ]
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v))
                    .collect()
                })
            })
            .collect()
    }

    /// A gesture on an item an app published: the user's binding on the placeholder wins
    /// (and passes the event through only with `pass-through`), else the app hears about it.
    fn dispatch_published(
        &mut self,
        region: u32,
        cell: &Cell,
        p: &crate::published::PubRef,
        g: Gesture,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        use crate::published::PubKind;
        use sliver_api::item::Gesture as AG;
        use sliver_api::registry::{Interaction as I, Phase};
        let send = |what: I, fx: &mut Vec<Effect>| {
            fx.push(Effect::Api {
                context: p.context,
                item: p.item.clone(),
                what,
            })
        };
        if let Gesture::Drag(x) = g {
            let PubKind::Slider { min, max, step } = p.kind else {
                return;
            };
            let f = self.frame_for(region).expect("cell came from a frame");
            let i = f.cell_index(region).expect("cell came from a frame");
            let Some(t) = f.layout.tracks.get(i).copied().flatten() else {
                return;
            };
            let frac = if t.w > 0.0 {
                ((x - t.x) / t.w).clamp(0.0, 1.0) as f64
            } else {
                0.0
            };
            let mut v = min + frac * (max - min);
            if step > 0.0 {
                v = (min + ((v - min) / step).round() * step).clamp(min, max);
            }
            let phase = if self.pub_drag.contains_key(&region) {
                Phase::Change
            } else {
                Phase::Begin
            };
            if self.pub_drag.insert(region, v) != Some(v) || phase == Phase::Begin {
                send(I::Slider { value: v, phase }, fx);
            }
            return;
        }
        if g == Gesture::Release
            && let Some(v) = self.pub_drag.remove(&region)
        {
            send(
                I::Slider {
                    value: v,
                    phase: Phase::End,
                },
                fx,
            );
        }
        let (want, ag) = match g {
            Gesture::Tap => (CGesture::Tap, AG::Tap),
            Gesture::DoubleTap => (CGesture::DoubleTap, AG::DoubleTap),
            Gesture::Hold => (CGesture::Hold, AG::Hold),
            Gesture::SwipeLeft => (CGesture::SwipeLeft, AG::SwipeLeft),
            Gesture::SwipeRight => (CGesture::SwipeRight, AG::SwipeRight),
            _ => return,
        };
        let user = cell
            .on
            .iter()
            .find(|(cg, _)| *cg == want)
            .map(|(_, a)| a.clone());
        if let Some(a) = &user {
            self.execute(a, None, Some(region), now, fx);
            if !cell.pass_through {
                return;
            }
        }
        match (p.kind, ag) {
            (PubKind::Close, AG::Tap) => send(I::CloseModal, fx),
            (PubKind::Close, _) => {}
            (PubKind::Segment(i), AG::Tap) => send(I::Select(i), fx),
            (PubKind::Candidate { index, .. }, AG::Tap) => send(I::Select(index), fx),
            // Swiping left brings the candidates to the right into view.
            (PubKind::Candidate { next, .. }, AG::SwipeLeft) => send(I::Scroll(next), fx),
            (PubKind::Candidate { prev, .. }, AG::SwipeRight) => send(I::Scroll(prev), fx),
            (PubKind::Candidate { .. }, _) => {}
            (PubKind::ScrollTo(to), AG::Tap) => send(I::Scroll(to), fx),
            (PubKind::ScrollTo(_), _) => {}
            (PubKind::Popover, AG::Tap) => {
                let class = cell
                    .classes
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "app".into());
                self.popover = Some((p.context, p.item.clone(), class));
                fx.extend(self.cancel_touches());
                fx.push(Effect::Redraw);
                send(I::Popover(true), fx);
            }
            (PubKind::PopoverClose, AG::Tap) => {
                self.popover = None;
                fx.extend(self.cancel_touches());
                fx.push(Effect::Redraw);
                send(I::Popover(false), fx);
            }
            (PubKind::PopoverClose, _) => {}
            (PubKind::Slider { .. }, AG::Tap) => {}
            _ => send(I::Gesture(ag), fx),
        }
    }

    /// A built-in module's own action (`actions = { on-hold = "…" }`, or its default bindings).
    fn module_action(
        &mut self,
        cell: &Cell,
        name: &str,
        value: Option<f64>,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        use sliver_session::mpris::Command;
        let pct = value.map(|v| v.clamp(0.0, 100.0).round() as u8);
        let kind = cell.kind.as_str();
        let op = match (kind, name, pct) {
            ("backlight" | "backlight/slider", "set", Some(p)) => SessionOp::ScreenBrightness(p),
            ("kbd-backlight" | "kbd-backlight/slider", "set", Some(p)) => {
                SessionOp::KeyboardBrightness(p)
            }
            ("audio" | "audio/slider", "set", Some(p)) => SessionOp::Volume {
                mic: cell.mic,
                pct: p,
            },
            ("audio" | "audio/slider", "toggle-mute", _) => SessionOp::ToggleMute { mic: cell.mic },
            ("mpris", "play-pause" | "next" | "previous", _) => {
                let cmd = match name {
                    "play-pause" => Command::PlayPause,
                    "next" => Command::Next,
                    _ => Command::Previous,
                };
                match self.ui(&cell.key).player.clone() {
                    Some(player) => SessionOp::MediaOn { player, cmd },
                    None => SessionOp::Media(cmd),
                }
            }
            ("mpris", "seek", Some(p)) => SessionOp::Seek {
                player: self.ui(&cell.key).player.clone(),
                pct: p,
            },
            ("mpris", n, _) if n.starts_with("player:") => {
                let key = cell.key.clone();
                self.ui(&key).player = Some(n["player:".len()..].to_string());
                fx.push(Effect::Redraw);
                return;
            }
            ("idle-inhibitor", "toggle", _) => SessionOp::ToggleCaffeine,
            ("network", "toggle", _) => SessionOp::ToggleWifi,
            ("bluetooth", "toggle", _) => SessionOp::ToggleBluetooth,
            ("dnd", "toggle", _) => SessionOp::ToggleDnd,
            ("dark-mode", "toggle", _) => SessionOp::ToggleDark,
            ("notifications", "open", _) => SessionOp::OpenNotification,
            ("notifications", "dismiss", _) => SessionOp::DismissNotification,
            ("timer", "toggle", _) => {
                let total = cell
                    .timer_total
                    .unwrap_or(std::time::Duration::from_secs(25 * 60));
                let wall = Local::now();
                let key = cell.key.clone();
                let t = self
                    .ui(&key)
                    .timer
                    .get_or_insert_with(|| modules::TimerState::new(total));
                if t.finished {
                    *t = modules::TimerState::new(total);
                } else if t.running_since.is_some() {
                    t.remaining = t.left(wall);
                    t.running_since = None;
                    t.generation += 1;
                } else {
                    t.running_since = Some(wall);
                    t.generation += 1;
                    let due = now + t.remaining.as_micros() as u64;
                    let generation = t.generation;
                    self.scheduled
                        .push((due, Scheduled::TimerDone { key, generation }));
                }
                fx.push(Effect::Redraw);
                return;
            }
            ("timer", "reset", _) => {
                let key = cell.key.clone();
                self.ui(&key).timer = None;
                fx.push(Effect::Redraw);
                return;
            }
            (_, "menu:open", _) => {
                if !cell.menu.is_empty() {
                    self.menu = Some(cell.menu.clone());
                    fx.extend(self.cancel_touches());
                    fx.push(Effect::Redraw);
                }
                return;
            }
            ("menu", "menu:close", _) => {
                self.menu = None;
                self.subbar = None;
                fx.extend(self.cancel_touches());
                fx.push(Effect::Redraw);
                return;
            }
            ("menu", n, _) if n.starts_with("menu:pick:") => {
                let pick = n["menu:pick:".len()..].parse::<usize>().ok();
                if let Some(entry) =
                    pick.and_then(|i| self.menu.as_ref().and_then(|m| m.get(i)).cloned())
                {
                    self.menu = None;
                    fx.extend(self.cancel_touches());
                    fx.push(Effect::Redraw);
                    // The `language` picker's entries are not config actions.
                    if let sliver_config::Action::ModuleAction(a) = &entry.on_tap
                        && let Some(i) =
                            a.strip_prefix("language:set:").and_then(|n| n.parse().ok())
                    {
                        fx.push(Effect::Desktop(sliver_desktop::control::Action::SetLayout(
                            i,
                        )));
                        return;
                    }
                    self.execute(&entry.on_tap, None, None, now, fx);
                }
                return;
            }
            ("emoji", n, _) => {
                self.emoji_action(&cell.key, n, now, fx);
                return;
            }
            ("language", "next", _) => {
                fx.push(Effect::Desktop(sliver_desktop::control::Action::NextLayout));
                return;
            }
            ("workspaces" | "taskbar", "focus", _) => {
                if let Some(t) = &cell.target {
                    fx.push(Effect::Desktop(t.clone()));
                }
                return;
            }
            ("taskbar", "close", _) => {
                if let Some(sliver_desktop::control::Action::FocusWindow { address }) = &cell.target
                {
                    fx.push(Effect::Desktop(
                        sliver_desktop::control::Action::CloseWindow {
                            address: address.clone(),
                        },
                    ));
                }
                return;
            }
            (_, "expand", _) => {
                let ui = self.ui(&cell.key);
                ui.expanded = !ui.expanded;
                fx.push(Effect::Redraw);
                return;
            }
            _ => {
                fx.push(Effect::Log(format!("{kind}: no action {name:?}")));
                return;
            }
        };
        fx.push(Effect::Session(op));
    }
}

impl Agent {
    /// The emoji strip's actions: `pick:<method>:<emoji>`, `tab:N`, `page:±N`, `close`, and
    /// `tones:<emoji>` / `tones-close` for the skin-tone choice.
    fn emoji_action(&mut self, cell_key: &str, name: &str, now: u64, fx: &mut Vec<Effect>) {
        // The strip's own key is the cell key without its `/tab…`, `/e…`, `/close` suffix.
        let strip = cell_key
            .rsplit_once('/')
            .map_or(cell_key, |(k, _)| k)
            .to_string();
        if let Some(rest) = name.strip_prefix("pick:") {
            let Some((method, text)) = rest.split_once(':') else {
                return;
            };
            self.emoji.push(text);
            self.type_text(method, text, now, fx);
            self.ui(&strip).emoji.tones = None;
            fx.push(Effect::Redraw);
        } else if let Some(e) = name.strip_prefix("tones:") {
            self.ui(&strip).emoji.tones = Some(e.to_string());
            fx.push(Effect::Redraw);
        } else if name == "tones-close" {
            self.ui(&strip).emoji.tones = None;
            fx.push(Effect::Redraw);
        } else if let Some(t) = name
            .strip_prefix("tab:")
            .and_then(|n| n.parse::<usize>().ok())
        {
            let ui = self.ui(&strip);
            ui.emoji.tab = t;
            ui.emoji.offset = 0;
            ui.emoji.tones = None;
            fx.push(Effect::Redraw);
        } else if let Some(d) = name
            .strip_prefix("page:")
            .and_then(|n| n.parse::<i64>().ok())
        {
            let ui = self.ui(&strip);
            ui.emoji.offset = (ui.emoji.offset as i64 + d).max(0) as usize;
            fx.push(Effect::Redraw);
        } else if name == "close" {
            self.execute(&Action::CloseLayer, None, None, now, fx);
        }
    }

    /// Type `text` into the focused app. `auto` = wtype if installed, else copy to the clipboard;
    /// `wtype` types directly (virtual keyboard); `paste` copies and presses Ctrl+V through
    /// sliverd (works anywhere on Wayland, replaces the clipboard); `copy` only copies.
    fn type_text(&mut self, method: &str, text: &str, now: u64, fx: &mut Vec<Effect>) {
        let method = match method {
            "auto" if tool_exists("wtype") => "wtype",
            "auto" => "copy",
            m => m,
        };
        match method {
            "wtype" => fx.push(Effect::Spawn(vec![
                "wtype".into(),
                "--".into(),
                text.into(),
            ])),
            "paste" | "copy" => {
                fx.push(Effect::Spawn(vec![
                    "wl-copy".into(),
                    "--".into(),
                    text.into(),
                ]));
                if method == "paste" {
                    // Give wl-copy a moment to own the selection before pasting.
                    if let (Some(ctrl), Some(v)) = (Key::from_name("LeftCtrl"), Key::from_name("V"))
                    {
                        self.scheduled.push((
                            now + 120_000,
                            Scheduled::Action(Action::Keys(vec![ctrl, v])),
                        ));
                    }
                }
            }
            other => fx.push(Effect::Log(format!(
                "emoji: unknown insert method {other:?}"
            ))),
        }
    }
}

/// Is `name` an executable on `$PATH`?
fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

/// What the recognizer should look for on this cell.
/// A slider shown as just its icon until opened (`collapsed = true`).
fn collapsed_slider(c: &Cell) -> bool {
    c.kind.ends_with("/slider")
        && c.drawer.is_some()
        && !matches!(c.content, sliver_render::Content::Slider { .. })
}

fn bindings(c: &Cell) -> Bindings {
    if let Some(p) = &c.published {
        use sliver_api::item::Gesture as AG;
        let has = |g: AG| p.gestures.contains(&g);
        let slider = matches!(p.kind, crate::published::PubKind::Slider { .. });
        let scrubber = matches!(p.kind, crate::published::PubKind::Candidate { .. });
        let user = |g: CGesture| c.on.iter().any(|(x, _)| *x == g);
        return Bindings {
            tap: !slider,
            double_tap: has(AG::DoubleTap) || user(CGesture::DoubleTap),
            hold: has(AG::Hold) || user(CGesture::Hold),
            release: slider,
            swipe: scrubber
                || has(AG::SwipeLeft)
                || has(AG::SwipeRight)
                || user(CGesture::SwipeLeft)
                || user(CGesture::SwipeRight),
            drag: slider,
            on_press: false,
            slide: false,
            swipe_threshold: None,
        };
    }
    let has = |g: CGesture| c.on.iter().any(|(x, _)| *x == g);
    let hold_layer = c.on.iter().any(|(g, a)| {
        *g == CGesture::Tap
            && matches!(
                a,
                Action::Layer {
                    mode: LayerMode::Hold,
                    ..
                }
            )
    });
    Bindings {
        tap: has(CGesture::Tap) || c.drawer.is_some(),
        double_tap: has(CGesture::DoubleTap),
        hold: has(CGesture::Hold),
        release: has(CGesture::Release),
        swipe: has(CGesture::SwipeLeft) || has(CGesture::SwipeRight),
        drag: has(CGesture::Drag),
        // A collapsed slider opens on press, so the same touch can slide it.
        slide: collapsed_slider(c),
        on_press: collapsed_slider(c)
            || hold_layer
            || c.on
                .iter()
                .any(|(g, a)| *g == CGesture::Tap && matches!(a, Action::Key(_) | Action::Keys(_))),
        swipe_threshold: None,
    }
}

fn slot(layer: Layer) -> usize {
    match layer {
        Layer::Base => 0,
        Layer::Fn => 1,
    }
}
