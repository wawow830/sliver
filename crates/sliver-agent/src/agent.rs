//! The agent core: config + context → resolved layers → frames, and touches → gestures →
//! actions. Sans-IO: the caller feeds it events and time, presents the frames it builds and
//! carries out the [`Effect`]s it returns (see `run` in main.rs and the tests).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local};
use sliver_config::{Action, Config, Context, CtxValue, Gesture as CGesture, LayerMode, Resolved};
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
    /// Not fatal, worth logging.
    Log(String),
    /// Change session state (volume, brightness, media, inhibitors). Run off the main loop.
    Session(SessionOp),
}

/// Session-side changes the built-in modules make.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionOp {
    ScreenBrightness(u8),
    KeyboardBrightness(u8),
    Volume { mic: bool, pct: u8 },
    ToggleMute { mic: bool },
    Media(sliver_session::mpris::Command),
    ToggleCaffeine,
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
    /// Applied on top of the world's context, e.g. the settings app's "Preview as".
    pub ctx_overrides: Vec<(String, CtxValue)>,
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
    /// Problems resolving layers, for logging/status. Cleared on each render.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
enum Scheduled {
    Action(Action),
    CloseDrawer(String),
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
            recognizer: Recognizer::new(),
            pressed: HashSet::new(),
            hold_layer: None,
            oneshot: None,
            scheduled: Vec::new(),
            drag: HashMap::new(),
            errors: Vec::new(),
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
        let mut ctx = crate::context::build(
            &world.desktop,
            &world.session,
            world.now,
            self.physical_esc,
            self.base_layer(),
        );
        for (k, v) in &self.ctx_overrides {
            ctx.set(k, v.clone());
        }
        self.ctx = ctx;
        let mut out = Vec::new();
        let base = self.base_layer().to_owned();
        for (layer, name) in [(Layer::Base, base.as_str()), (Layer::Fn, "fn")] {
            match self.config.resolve(name, &self.ctx) {
                Ok(r) => {
                    let id_base = if layer == Layer::Fn {
                        FN_REGION_BASE
                    } else {
                        0
                    };
                    let f = self.build(&r, world, id_base);
                    self.frames[slot(layer)] = Some(f);
                    out.push(layer);
                }
                Err(e) => self.errors.push(format!("layer {name}: {e}")),
            }
        }
        out
    }

    fn build(&mut self, r: &Resolved, world: &World, id_base: u32) -> Frame {
        let ui = &self.ui;
        let lookup = |k: &str| ui.get(k).cloned().unwrap_or_default();
        let per_zone: Vec<Vec<Cell>> = r
            .zones
            .iter()
            .map(|z| {
                let mut cells = Vec::new();
                modules::cells(
                    &z.modules,
                    world,
                    &mut self.icons,
                    ICON_LOOKUP_SIZE,
                    &lookup,
                    &mut cells,
                );
                for c in &mut cells {
                    c.pressed = self.pressed.contains(&c.key);
                }
                cells
            })
            .collect();
        let classes = vec![format!("layer-{}", r.layer), format!("layout-{}", r.layout)];
        let scene = frame::scene(&r.zones, &per_zone, classes);
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
            self.dispatch(r, g, time_us, &mut fx);
        }
        fx
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
        let want = match g {
            Gesture::Pressed(on) => {
                let changed = if on {
                    self.pressed.insert(cell.key.clone())
                } else {
                    self.pressed.remove(&cell.key)
                };
                if changed {
                    fx.push(Effect::Redraw);
                }
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
            Action::ModuleAction(name) => {
                let Some(cell) = region.and_then(|r| self.cell(r)).cloned() else {
                    return;
                };
                self.module_action(&cell, name, value, fx);
            }
            other => fx.push(Effect::Log(format!("action not supported yet: {other:?}"))),
        }
    }
}

impl Agent {
    /// A built-in module's own action (`actions = { on-hold = "…" }`, or its default bindings).
    fn module_action(&mut self, cell: &Cell, name: &str, value: Option<f64>, fx: &mut Vec<Effect>) {
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
            ("mpris", "play-pause", _) => SessionOp::Media(Command::PlayPause),
            ("mpris", "next", _) => SessionOp::Media(Command::Next),
            ("mpris", "previous", _) => SessionOp::Media(Command::Previous),
            ("idle-inhibitor", "toggle", _) => SessionOp::ToggleCaffeine,
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

/// What the recognizer should look for on this cell.
fn bindings(c: &Cell) -> Bindings {
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
        on_press: hold_layer
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
