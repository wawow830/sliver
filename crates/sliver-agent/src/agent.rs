//! The agent core: config + context → resolved layers → frames, and touches → gestures →
//! actions. Sans-IO: the caller feeds it events and time, presents the frames it builds and
//! carries out the [`Effect`]s it returns (see [`crate::driver`], which runs it, and the tests).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local};
use sliver_config::{Action, Config, Context, CtxValue, Gesture as CGesture, LayerMode, Resolved};
use sliver_ipc::proto::{DisplayState, KeyMode, Layer, TouchPhase};
use sliver_render::{Renderer, Stylesheet};

use crate::act::Do;
use crate::frame::{self, Frame};
use crate::gesture::{Bindings, Gesture, Recognizer};
use crate::icons::Icons;
use crate::modules::{self, Cell, UiState, World};

mod actions;
mod control;
mod input;
mod overlay;

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
const HELD_GRACE_US: u64 = 1_000_000;

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
    /// A `{ dbus = {...} }` action.
    DBus(sliver_config::DBusCall),
}

pub struct Agent {
    config: Config,
    sheet: Stylesheet,
    renderer: Renderer,
    /// Character keys in combos follow the bar keyboard's layout, and the keymap they came
    /// from (rebuilt only when it changes).
    keymap: crate::keymap::KeyMapper,
    keymap_names: Option<sliver_desktop::XkbNames>,
    icons: Icons,
    width: u32,
    height: u32,
    /// The context the last render resolved with (built from the world, plus `ctx_overrides`).
    ctx: Context,
    /// Applied on top of the world's context (simulate one condition over live state).
    ctx_overrides: Vec<(String, CtxValue)>,
    /// Whether this Mac has a physical Esc key (decides the `esc` zone).
    physical_esc: bool,
    /// Custom layers opened over `base` (last = on top).
    layers: Vec<String>,
    ui: HashMap<String, UiState>,
    display: DisplayState,
    /// sliverd's safe hatch is showing its own layer.
    hatch: bool,
    /// Fn is held: sliverd shows our `fn` frame.
    fn_held: bool,
    frames: [Option<Frame>; 2],
    /// A base-layer change animating (`-sliver-layer-transition`), and the layer depth the base
    /// frame was last drawn at (deeper = slide forward).
    layer_anim: Option<frame::LayerAnim>,
    /// Tests: make the next frame build panic (to exercise the guard in `render`).
    #[cfg(test)]
    panic_in_build: bool,
    base_depth: usize,
    touches: Touches,
    /// The top custom layer closes on its own: the layer, and when.
    closes: Option<(String, Closes)>,
    /// Macro steps and drawer closings due at a monotonic time (µs).
    scheduled: Vec<(u64, Scheduled)>,
    /// `on-drag` throttling by region: it outlives the touch, so the final value always runs.
    drag: HashMap<u32, DragState>,
    /// Problems resolving layers, for logging/status. Cleared on each render.
    errors: Vec<String>,
    /// `graph` modules' samples, by module id. Sampled in [`Agent::render`] while the bar is
    /// drawn.
    graphs: HashMap<String, Samples>,
    /// A menu or drawer sub-bar covering the bar.
    overlay: Option<overlay::Overlay>,
    /// Sliders under a finger, by cell key: the value there, and when it was let go. They show
    /// it at once (the system's level is set at most every [`DRAG_INTERVAL_US`] and reported back
    /// later), and after release (monotonic µs) until the system reports it or [`HELD_GRACE_US`]
    /// passes.
    under_finger: HashMap<String, (f32, Option<u64>)>,
    /// `sliverctl bar hide`: the base layer shows nothing (Fn → F-keys still works).
    hidden: bool,
    /// `sliverctl hide MODULE`: module ids/keys hidden at runtime (not saved).
    hidden_modules: std::collections::BTreeSet<String>,
    /// A layout shown on a layer whatever the rules say (`sliverctl layout`).
    forced: Option<Forced>,
    /// What the base layer resolved to last: its layout and the rule (index) that picked it.
    active: Option<(String, Option<usize>)>,
    /// `sliverctl update MODULE …`: output pushed to exec-less custom modules.
    pushed: HashMap<String, crate::scripts::Output>,
}

/// A placed `graph`'s history: oldest first, 0..=1, how many it keeps, and when the last
/// sample was taken.
#[derive(Debug, Default)]
struct Samples {
    values: std::collections::VecDeque<f32>,
    cap: usize,
    taken: Option<DateTime<Local>>,
}

/// The fingers on the bar: the gesture recognizer, the cells they press, and the sliders they
/// slide. All of it ends together when touches are cancelled (the display going off, the
/// hatch, the bar changing under them).
struct Touches {
    recognizer: Recognizer,
    /// Cell keys being touched (agent regions).
    pressed: HashSet<String>,
    /// Press-and-slide anchors per region: (finger x, slider value 0–100) when sliding began.
    slide: HashMap<u32, (f32, f64)>,
}

impl Touches {
    /// Ends every touch (the recognizer keeps its bar gestures). Returns whether anything was
    /// pressed.
    fn end(&mut self) -> bool {
        self.recognizer.cancel_all();
        self.slide.clear();
        !std::mem::take(&mut self.pressed).is_empty()
    }
}

impl Default for Touches {
    fn default() -> Touches {
        Touches {
            recognizer: Recognizer::new(),
            pressed: HashSet::new(),
            slide: HashMap::new(),
        }
    }
}

/// When a custom layer closes on its own.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Closes {
    /// A `hold` layer: when the finger on this region lets go.
    Release(u32),
    /// A `oneshot` layer: after the next action taken from it.
    NextAction,
}

/// A layout shown on a layer whatever the rules say (`Control1.SetLayout`).
#[derive(Debug, Clone, PartialEq)]
struct Forced {
    layer: String,
    layout: String,
}

#[derive(Debug, Clone)]
enum Scheduled {
    Do(Do),
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
        // tiny-dfr's heuristic: only the 2170 px bars lack a physical Esc.
        let physical_esc = width < 2170;
        let ctx = Context::new().with("physical_esc", physical_esc);
        Agent {
            ctx_overrides: Vec::new(),
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
            touches: Touches::default(),
            closes: None,
            scheduled: Vec::new(),
            drag: HashMap::new(),
            hidden: false,
            hidden_modules: Default::default(),
            forced: None,
            active: None,
            pushed: HashMap::new(),
            errors: Vec::new(),
            graphs: HashMap::new(),
            overlay: None,
            under_finger: HashMap::new(),
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
        use crate::modules::{graph_params, graph_sample};
        if !self.config.modules().keys().any(|id| id.kind == "graph") {
            self.graphs.clear();
            return;
        }
        let placed: Vec<_> = crate::modules::placed_modules(&self.config)
            .into_iter()
            .filter(|m| m.id.kind == "graph")
            .collect();
        self.graphs
            .retain(|k, _| placed.iter().any(|m| m.id.to_string() == *k));
        for m in placed {
            let (every, cap) = graph_params(&m);
            let g = self.graphs.entry(m.id.to_string()).or_default();
            g.cap = cap;
            let due = g.taken.is_none_or(|t| {
                (world.now.wall - t)
                    .to_std()
                    .is_ok_and(|d| d + std::time::Duration::from_millis(50) >= every)
            });
            if !due {
                continue;
            }
            if let Some(v) = graph_sample(&self.config, &m, world) {
                g.values.push_back(v);
                while g.values.len() > cap {
                    g.values.pop_front();
                }
                g.taken = Some(world.now.wall);
            }
        }
    }

    /// The samples a `graph` cell draws: by its key, which ends with the module id (keys of
    /// group members are prefixed with the group's).
    fn graph_for(graphs: &HashMap<String, Samples>, key: &str) -> Option<(Vec<f32>, usize)> {
        let (_, g) = graphs
            .iter()
            .find(|(id, _)| key == id.as_str() || key.ends_with(&format!("/{id}")))?;
        Some((g.values.iter().copied().collect(), g.cap))
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
        let mut ctx = crate::context::build(
            world.context.clone(),
            world.now.wall,
            self.physical_esc,
            self.base_layer(),
        );
        for (k, v) in &self.ctx_overrides {
            ctx.set(k, v.clone());
        }
        self.ctx = ctx;
        let bound = |g: crate::gesture::BarGesture| self.config.gestures.contains_key(g.name());
        use crate::gesture::BarGesture as BG;
        self.touches
            .recognizer
            .set_bar(crate::gesture::BarBindings {
                two_finger: bound(BG::TwoFingerSwipeLeft) || bound(BG::TwoFingerSwipeRight),
                edge_left: bound(BG::EdgeSwipeFromLeft),
                edge_right: bound(BG::EdgeSwipeFromRight),
                width: self.width as f32,
            });
        // Locked: modules only ever see the sanitized world (lock-screen privacy).
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
            let resolved = match &self.forced {
                Some(f) if f.layer == name => self.config.resolve_as(name, &f.layout, &self.ctx),
                _ => self.config.resolve(name, &self.ctx),
            };
            match resolved {
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
                        self.active = Some((r.layout.clone(), r.rule));
                    }
                    for key in modules::start_expanded_drawers(&r.zones) {
                        self.ui.entry(key).or_insert_with(|| UiState {
                            expanded: true,
                            ..UiState::default()
                        });
                    }
                    // A panic while building is caught: systemd would restart the agent into the
                    // same panic. The layer keeps its last good frame, and what a build changes
                    // besides caches (sliders under a finger) is restored.
                    let kept = self.under_finger.clone();
                    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.build(&r, world, id_base)
                    }));
                    let mut f = match built {
                        Ok(f) => f,
                        Err(p) => {
                            self.under_finger = kept;
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
                        self.animate_layer_change(&mut f, world.now.instant);
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
    fn animate_layer_change(&mut self, f: &mut Frame, now: std::time::Instant) {
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
        let placed = modules::Placed {
            pushed: self.pushed.clone(),
        };
        let ui = &self.ui;
        let graphs = &self.graphs;
        let lookup = |k: &str| {
            let mut s = ui.get(k).cloned().unwrap_or_default();
            if !graphs.is_empty() {
                s.graph = Self::graph_for(graphs, k);
            }
            s
        };
        // A menu or drawer covers all but the first zone.
        let mut cover = (id_base == 0)
            .then(|| {
                overlay::cover(
                    self.overlay.as_ref(),
                    &r.zones,
                    world,
                    &placed,
                    &mut self.icons,
                    &lookup,
                )
            })
            .flatten();
        let (zones, covered) = overlay::zones(&r.zones, cover.as_ref());
        let hidden = &self.hidden_modules;
        let mut per_zone: Vec<Vec<Cell>> = zones
            .iter()
            .enumerate()
            .map(|(zi, z)| {
                let mut cells = match cover.as_mut().filter(|_| covered == Some(zi)) {
                    Some(c) => std::mem::take(&mut c.cells),
                    None => {
                        let mut cells = Vec::new();
                        let mut b = modules::Build {
                            world,
                            placed: &placed,
                            icons: &mut self.icons,
                            icon_size: ICON_LOOKUP_SIZE,
                            ui: &lookup,
                        };
                        modules::cells(&z.modules, &mut b, &mut cells);
                        cells.retain(|c| !hidden.contains(&c.id) && !hidden.contains(&c.key));
                        cells
                    }
                };
                for c in &mut cells {
                    c.look.pressed = self.touches.pressed.contains(&c.key);
                    if let (Some(&(v, up)), sliver_render::Content::Slider { value, .. }) =
                        (self.under_finger.get(&c.key), &mut c.look.content)
                    {
                        let caught_up = (*value - v).abs() < 0.015;
                        let gone = |t: u64| world.now.mono_us.saturating_sub(t) > HELD_GRACE_US;
                        if up.is_some_and(|t| caught_up || gone(t)) {
                            self.under_finger.remove(&c.key);
                        } else {
                            *value = v;
                        }
                    }
                }
                cells
            })
            .collect();
        let classes = vec![format!("layer-{}", r.layer), format!("layout-{}", r.layout)];
        // Fast-path key regions (sliverd presses them itself) get layout-mapped keys too.
        if world.desktop.bar_keymap != self.keymap_names {
            // Built before either is replaced: the names always say what the keymap is.
            self.keymap = match &world.desktop.bar_keymap {
                Some(n) => crate::keymap::KeyMapper::new(n),
                None => crate::keymap::KeyMapper::positional(),
            };
            self.keymap_names = world.desktop.bar_keymap.clone();
        }
        for c in per_zone.iter_mut().flatten() {
            if let crate::modules::Touch::Keys(keys) = &mut c.touch {
                *keys = self.keymap.map_all(keys);
            }
        }
        if self.ctx.flag("locked") {
            for cells in &mut per_zone {
                cells.retain(crate::privacy::allowed);
            }
        }
        let scene = frame::scene(&zones, &per_zone, classes);
        let cells = per_zone.into_iter().flatten().collect();
        let mut f = frame::render(
            &mut self.renderer,
            &self.sheet,
            &scene,
            cells,
            (self.width, self.height),
            id_base,
            world.now.instant,
        );
        if self.touches.recognizer.bar_bound() {
            f.fill_gaps();
        }
        // Last, so the guard in `render` is tested against everything a build changes.
        #[cfg(test)]
        if self.panic_in_build {
            panic!("test panic in build");
        }
        f
    }

    /// Re-renders just the images of frames whose pressed cells changed (after
    /// [`Effect::RedrawPress`]). Returns the layers to present.
    pub fn render_press(&mut self, at: std::time::Instant) -> Vec<Layer> {
        // A touch ends a layer change at once: the press draws on the settled layer.
        self.layer_anim = None;
        let pressed = &self.touches.pressed;
        let is_pressed = |c: &Cell| pressed.contains(&c.key);
        let mut out = Vec::new();
        for (i, layer) in [Layer::Base, Layer::Fn].into_iter().enumerate() {
            if let Some(f) = &mut self.frames[i]
                && f.repress(&mut self.renderer, &self.sheet, &is_pressed, at)
            {
                out.push(layer);
            }
        }
        out
    }

    /// When some module's content changes on its own next (the earliest clock tick), if ever.
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

    /// The next monotonic time (µs) [`Agent::tick`] has work: holds, double taps, macro steps,
    /// drawer closings.
    pub fn next_deadline(&self) -> Option<u64> {
        let drag = self
            .drag
            .values()
            .filter(|d| d.pending.is_some())
            .filter_map(|d| d.last_run.map(|t| t + DRAG_INTERVAL_US));
        self.touches
            .recognizer
            .next_deadline()
            .into_iter()
            .chain(self.scheduled.iter().map(|s| s.0))
            .chain(drag)
            .min()
    }

    /// Runs whatever is due at `now` (monotonic µs).
    pub fn tick(&mut self, now: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        for (region, g) in self.touches.recognizer.tick(now) {
            self.dispatch(region, g, now, &mut fx);
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.scheduled)
            .into_iter()
            .partition(|s| s.0 <= now);
        self.scheduled = later;
        for (_, s) in due {
            match s {
                Scheduled::Do(d) => self.run(&d, None, None, now, &mut fx),
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
                    if matches!(self.overlay, Some(overlay::Overlay::Menu { .. })) {
                        self.overlay = None;
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

    /// A panic while building a frame is caught: the layer keeps its last good frame, the error
    /// is reported, and the next render works again.
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
