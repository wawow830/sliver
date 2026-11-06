//! Touches in: sliverd's touch events become gestures, gestures become the bound actions, and
//! drags and holds keep their value until the system reports it.

use super::*;
use crate::modules::Opens;

impl Agent {
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
            // Empty bar space binds nothing itself, but a touch there still makes bar gestures
            // and settles a tap waiting on a module. Under a select-only hold nothing fires.
            let background = self
                .frame_for(region)
                .is_some_and(|f| f.is_background(region));
            if background && self.held() != Some(true) {
                let recognized = self.touches.recognizer.touch(
                    region,
                    Bindings::default(),
                    slot,
                    phase,
                    x,
                    time_us,
                );
                for (r, g) in recognized {
                    self.gesture(r, g, time_us, &mut fx);
                }
            }
            return fx;
        };
        // Mirror: the editor selects what was touched; select only fires nothing.
        if let Some(select_only) = self.held()
            && self.on_held(region)
        {
            let key = cell.key.clone();
            if phase == TouchPhase::Down
                && let Some((module, zone, index)) = self.held_target(region)
            {
                fx.push(Effect::HeldPressed {
                    module,
                    zone,
                    index,
                });
            }
            if select_only {
                let changed = if phase == TouchPhase::Down {
                    self.touches.pressed.insert(key)
                } else if matches!(phase, TouchPhase::Up | TouchPhase::Cancel) {
                    self.touches.pressed.remove(&key)
                } else {
                    false
                };
                if changed {
                    fx.push(Effect::RedrawPress);
                }
                return fx;
            }
        }
        let Some(cell) = self.cell(region) else {
            return fx;
        };
        let b = bindings(cell);
        for (r, g) in self
            .touches
            .recognizer
            .touch(region, b, slot, phase, x, time_us)
        {
            self.gesture(r, g, time_us, &mut fx);
        }
        fx
    }

    /// Runs a recognized gesture: a bar gesture's `[gestures]` action, else the module's.
    pub(super) fn gesture(&mut self, region: u32, g: Gesture, now: u64, fx: &mut Vec<Effect>) {
        match g {
            Gesture::Bar(bg) => self.bar_gesture(bg, now, fx),
            g => self.dispatch(region, g, now, fx),
        }
    }

    /// A bar-wide gesture: run its `[gestures]` action.
    pub(super) fn bar_gesture(
        &mut self,
        g: crate::gesture::BarGesture,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        if let Some(a) = self.config.gestures.get(g.name()).cloned() {
            self.execute(&a, None, None, now, fx);
        }
    }

    /// Display off, hatch, layer switch: every touch ends. A published slider being dragged is
    /// told the drag ended, and a `hold` layer no longer waits for its finger.
    pub fn cancel_touches(&mut self) -> Vec<Effect> {
        let mut fx = Vec::new();
        let (pressed, dragged) = self.touches.end();
        for (region, value) in dragged {
            if let Some(p) = self.cell(region).and_then(|c| c.published.clone()) {
                fx.push(Effect::Api {
                    context: p.context,
                    item: p.item,
                    what: sliver_api::registry::Interaction::Slider {
                        value,
                        phase: sliver_api::registry::Phase::End,
                    },
                });
            }
        }
        if matches!(self.closes, Some((_, Closes::Release(_)))) {
            self.closes = None;
        }
        if pressed {
            fx.push(Effect::Redraw);
        }
        fx
    }

    pub(super) fn frame_for(&self, region: u32) -> Option<&Frame> {
        let slot = if region > FN_REGION_BASE { 1 } else { 0 };
        self.frames[slot].as_ref()
    }

    pub(super) fn cell(&self, region: u32) -> Option<&Cell> {
        let f = self.frame_for(region)?;
        f.cells.get(f.cell_index(region)?)
    }

    pub(super) fn dispatch(&mut self, region: u32, g: Gesture, now: u64, fx: &mut Vec<Effect>) {
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
                    self.touches.pressed.insert(cell.key.clone())
                } else {
                    self.touches.pressed.remove(&cell.key)
                };
                if changed {
                    fx.push(Effect::RedrawPress);
                }
                if !on {
                    self.touches.slide.remove(&region);
                    if let Some(h) = self.under_finger.get_mut(&cell.key) {
                        h.1 = Some(now);
                    }
                }
                return;
            }
            Gesture::Slide(x) => {
                // The slider opened under the finger (press on its icon). Move the value by
                // how far the finger slid, so touching it never jumps the value.
                let f = self.frame_for(region).expect("cell came from a frame");
                let i = f.cell_index(region).expect("cell came from a frame");
                let (Some(t), sliver_render::Content::Slider { value, .. }) = (
                    f.layout.tracks.get(i).copied().flatten(),
                    &cell.look.content,
                ) else {
                    return; // Not open (yet): nothing to slide.
                };
                if t.w <= 0.0 {
                    return;
                }
                let (x0, v0) = *self
                    .touches
                    .slide
                    .entry(region)
                    .or_insert((x, (*value as f64 * 100.0).round()));
                let v = (v0 + ((x - x0) / t.w) as f64 * 100.0)
                    .clamp(0.0, 100.0)
                    .round();
                self.hold_value(&cell.key, v, fx);
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
                        let v = (((x - t.x) / t.w).clamp(0.0, 1.0) * 100.0).round() as f64;
                        self.hold_value(&cell.key, v, fx);
                        v
                    }
                    _ => x.round() as f64,
                };
                self.keep_drawer_open(&cell, now);
                self.run_drag(region, Some(value), now, false, fx);
                return;
            }
            Gesture::Release => {
                if let Some((name, Closes::Release(r))) = &self.closes
                    && *r == region
                {
                    if self.layers.last() == Some(name) {
                        self.layers.pop();
                        fx.extend(self.cancel_touches());
                        fx.push(Effect::Redraw);
                    }
                    self.closes = None;
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
        match (cell.binding(want).cloned(), &cell.drawer) {
            (Some(d), _) => {
                let layer_before = self.base_layer().to_owned();
                self.run(&d, None, Some(region), now, fx);
                if cell.kind == "custom" {
                    fx.push(Effect::ModuleFired(cell.id.clone()));
                }
                // A oneshot layer closes after the first action taken from it.
                if self.closes == Some((layer_before.clone(), Closes::NextAction))
                    && region <= FN_REGION_BASE
                    && !matches!(d, Do::Run(Action::Layer { .. }))
                {
                    self.closes = None;
                    if self.layers.last() == Some(&layer_before) {
                        self.layers.pop();
                        fx.extend(self.cancel_touches());
                        fx.push(Effect::Redraw);
                    }
                }
            }
            (None, Some(d)) if want == CGesture::Tap && d.opens == Opens::SubBar => {
                self.overlay = Some(overlay::Overlay::Drawer(d.group.clone()));
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

    /// Shows `v` (0–100) on the slider `key` at once, while the finger is on it.
    pub(super) fn hold_value(&mut self, key: &str, v: f64, fx: &mut Vec<Effect>) {
        let v = (v / 100.0) as f32;
        if self
            .under_finger
            .insert(key.to_owned(), (v, None))
            .map(|h| h.0)
            != Some(v)
        {
            fx.push(Effect::Redraw);
        }
    }

    /// An auto-closing drawer (an opened collapsed slider) stays open while it's being used.
    pub(super) fn keep_drawer_open(&mut self, cell: &Cell, now: u64) {
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
    pub(super) fn run_drag(
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
        if let Some(d) = self
            .cell(region)
            .and_then(|c| c.binding(CGesture::Drag).cloned())
        {
            self.run(&d, Some(v), Some(region), now, fx);
        }
    }

    /// Does what a binding says. `value` is a drag's (0–100 on a slider), `region` the cell it
    /// was bound on.
    pub(super) fn run(
        &mut self,
        d: &Do,
        value: Option<f64>,
        region: Option<u32>,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        match d {
            Do::Run(a) => self.execute(a, value, region, now, fx),
            Do::Agent(act) => self.act(act, value, now, fx),
            Do::Steps(steps) => {
                let mut at = now;
                for s in steps {
                    match s {
                        Do::Run(Action::Sleep(d)) => at += d.as_micros() as u64,
                        s if at == now => self.run(s, value, region, now, fx),
                        s => self.scheduled.push((at, Scheduled::Do(s.clone()))),
                    }
                }
            }
        }
    }

    /// Runs a config action. One that names a module's verb needs that module: from a cell's
    /// bindings it was resolved when the cell was built ([`crate::act`]).
    pub(super) fn execute(
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
            Action::Key(k) => fx.push(Effect::EmitKeys(
                vec![self.keymap.map(k.code())],
                KeyMode::Tap,
            )),
            Action::Keys(ks) => fx.push(Effect::EmitKeys(
                ks.iter().map(|k| self.keymap.map(k.code())).collect(),
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
                    LayerMode::Hold => {
                        self.closes = region.map(|r| (name.clone(), Closes::Release(r)))
                    }
                    LayerMode::Oneshot => self.closes = Some((name.clone(), Closes::NextAction)),
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
            Action::Macro(_) => self.run(&Do::bind(a, "", &[]), value, region, now, fx),
            Action::Sleep(_) => {}
            Action::DBus(c) => fx.push(Effect::Session(SessionOp::DBus(c.clone()))),
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
            Action::ModuleAction(name) => fx.push(Effect::Log(format!(
                "module action {name:?} outside a module's bindings does nothing"
            ))),
        }
    }
}

/// A slider shown as just its icon until opened (`collapsed = true`).
pub(super) fn collapsed_slider(c: &Cell) -> bool {
    c.drawer.as_ref().is_some_and(|d| d.opens == Opens::Slider)
        && !matches!(c.look.content, sliver_render::Content::Slider { .. })
}

/// What the recognizer should look for on this cell.
pub(super) fn bindings(c: &Cell) -> Bindings {
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
    let hold_layer = matches!(
        c.binding(CGesture::Tap).and_then(Do::action),
        Some(Action::Layer {
            mode: LayerMode::Hold,
            ..
        })
    );
    let swipe = has(CGesture::SwipeLeft) || has(CGesture::SwipeRight);
    // Keys fire on press, for latency, unless the module waits to see what the finger does.
    let waits = swipe || has(CGesture::Hold) || has(CGesture::DoubleTap) || has(CGesture::Drag);
    Bindings {
        tap: has(CGesture::Tap) || c.drawer.is_some(),
        double_tap: has(CGesture::DoubleTap),
        hold: has(CGesture::Hold),
        release: has(CGesture::Release),
        swipe,
        drag: has(CGesture::Drag),
        // A collapsed slider opens on press, so the same touch can slide it.
        slide: collapsed_slider(c),
        on_press: collapsed_slider(c)
            || hold_layer
            || !waits
                && matches!(
                    c.binding(CGesture::Tap).and_then(Do::action),
                    Some(Action::Key(_) | Action::Keys(_))
                ),
        swipe_threshold: None,
    }
}

pub(super) fn slot(layer: Layer) -> usize {
    match layer {
        Layer::Base => 0,
        Layer::Fn => 1,
    }
}
