//! What actions do inside the agent: layouts and layers, module actions, menus, published
//! items, and the emoji picker.

use super::*;
use crate::act::{Act, EmojiOp, Insert, UiChange};

impl Agent {
    /// Share the emoji picker's recents with the worlds this agent draws (main does this once).
    pub fn set_emoji(&mut self, handle: modules::EmojiHandle) {
        self.emoji = handle;
    }

    /// Relative icon paths in the config resolve against this (the config file's directory).
    pub fn set_config_dir(&mut self, dir: Option<&std::path::Path>) {
        self.icons.set_base_dir(dir);
    }

    /// Shows a newly loaded config and stylesheet. Opened layers, and a forced layout, that the
    /// config no longer has close.
    pub fn apply(&mut self, a: &crate::reload::Applied) -> Vec<Effect> {
        self.config = a.config.clone();
        self.sheet = a.sheet.clone();
        self.set_config_dir(a.config_path.as_deref().and_then(std::path::Path::parent));
        let c = &self.config;
        self.layers.retain(|l| c.layers.contains_key(l));
        if self
            .forced
            .as_ref()
            .is_some_and(|f| !c.layouts.contains_key(&f.layout))
        {
            self.forced = None;
        }
        vec![Effect::Redraw]
    }

    /// The desktop's reduce-motion setting: transitions (and layer slides) jump to their end.
    pub fn set_reduce_motion(&mut self, on: bool) {
        self.renderer.set_reduce_motion(on);
    }

    /// `Control1.SetLayout`: show `layout` on the current base layer until the next call (an
    /// empty name goes back to the rules).
    pub fn set_layout(&mut self, layout: &str) -> Result<(), String> {
        if layout.is_empty() {
            self.forced = None;
            return Ok(());
        }
        self.config
            .layout_zones(layout)
            .map_err(|e| e.to_string())?;
        self.forced = Some(Forced {
            layer: self.base_layer().to_owned(),
            layout: layout.to_owned(),
            held: None,
        });
        Ok(())
    }

    /// Whether a client holds the forced layout (`Some(true)`: select only).
    pub(super) fn held(&self) -> Option<bool> {
        self.forced.as_ref().and_then(|f| f.held)
    }

    /// `Control1.HoldLayout…` (Mirror): [`Agent::set_layout`], and every touch on it is reported
    /// as [`Effect::HeldPressed`]. With `select_only`, touching only selects (nothing fires).
    pub fn hold_layout(&mut self, layout: &str, select_only: bool) -> Result<(), String> {
        self.set_layout(layout)?;
        if let Some(f) = &mut self.forced {
            f.held = Some(select_only);
        }
        Ok(())
    }

    /// Is `region` an item of the held layout as the bar shows it now (not another layout, a
    /// sub-bar, a popover or the Fn layer)?
    pub(super) fn on_held(&self, region: u32) -> bool {
        self.held().is_some()
            && region <= FN_REGION_BASE
            && self.overlay.is_none()
            && self.forced.as_ref().map(|f| &f.layout) == self.active.as_ref().map(|a| &a.0)
    }

    /// What a touch on `region` selects in the held layout (`HeldPressed`): the cell's key, its
    /// zone's name, and the module's index in that zone's `modules`. `None` when nothing is
    /// held, or the bar shows something else (another layout, a sub-bar, a popover, Fn).
    pub(super) fn held_target(&self, region: u32) -> Option<(String, String, u32)> {
        if !self.on_held(region) {
            return None;
        }
        let name = &self.forced.as_ref()?.layout;
        let f = self.frames[0].as_ref()?;
        let i = f.cell_index(region)?;
        let center = |j: usize| f.layout.items.get(j).map(|r| r.x + r.w / 2.0);
        let c = center(i)?;
        let zone = f
            .layout
            .zones
            .iter()
            .position(|z| c >= z.x && c <= z.x + z.w)?;
        let z = &f.layout.zones[zone];
        let in_zone: Vec<usize> = (0..f.cells.len())
            .filter(|j| center(*j).is_some_and(|c| c >= z.x && c <= z.x + z.w))
            .collect();
        let keys: Vec<String> = in_zone.iter().map(|j| f.cells[*j].key.clone()).collect();
        let pos = in_zone.iter().position(|j| *j == i)?;
        let zones = self.config.layout_zones(name).ok()?;
        let zc = zones.get(zone)?;
        let ids: Vec<String> = zc.modules.iter().map(|m| m.id().to_string()).collect();
        let index = crate::modules::module_index(&ids, &keys, pos)?;
        Some((f.cells[i].key.clone(), zc.name.clone(), index as u32))
    }

    /// What the base layer shows and why: `(layout, rule index, rule name, source)`, the source
    /// being `rule`, `default` or `forced` (`Control1.ActiveLayout…`).
    pub fn active_layout(&self) -> Option<(String, Option<usize>, String, &'static str)> {
        let (layout, rule) = self.active.clone()?;
        let name = rule
            .and_then(|i| self.config.rules.get(i))
            .and_then(|r| r.name.clone())
            .unwrap_or_default();
        let source = match (&self.forced, rule) {
            (Some(_), _) => "forced",
            (None, Some(_)) => "rule",
            (None, None) => "default",
        };
        Some((layout, rule, name, source))
    }

    /// The layout forced by [`Agent::set_layout`], if any.
    pub fn layout_override(&self) -> Option<&str> {
        self.forced.as_ref().map(|f| f.layout.as_str())
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
                            Prop::List(c.look.classes.iter().cloned().map(Prop::Str).collect()),
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
    pub(super) fn dispatch_published(
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
            let phase = if self.touches.pub_drag.contains_key(&region) {
                Phase::Change
            } else {
                Phase::Begin
            };
            if self.touches.pub_drag.insert(region, v) != Some(v) || phase == Phase::Begin {
                send(I::Slider { value: v, phase }, fx);
            }
            return;
        }
        if g == Gesture::Release
            && let Some(v) = self.touches.pub_drag.remove(&region)
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
        if let Some(d) = cell.binding(want).cloned() {
            self.run(&d, None, Some(region), now, fx);
            if !p.pass_through {
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
                    .look
                    .classes
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "app".into());
                self.overlay = Some(overlay::Overlay::Popover {
                    context: p.context,
                    item: p.item.clone(),
                    class,
                });
                fx.extend(self.cancel_touches());
                fx.push(Effect::Redraw);
                send(I::Popover(true), fx);
            }
            (PubKind::PopoverClose, AG::Tap) => {
                self.overlay = None;
                fx.extend(self.cancel_touches());
                fx.push(Effect::Redraw);
                send(I::Popover(false), fx);
            }
            (PubKind::PopoverClose, _) => {}
            (PubKind::Slider { .. }, AG::Tap) => {}
            _ => send(I::Gesture(ag), fx),
        }
    }

    /// Opens `items` as the menu of the cell `key`: the ✕ takes that cell's place if the menu
    /// covers it, and the menu closes on its own after [`MENU_TIMEOUT_US`].
    pub(super) fn open_menu(
        &mut self,
        key: &str,
        items: Vec<crate::act::MenuItem>,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        self.scheduled
            .retain(|s| !matches!(s.1, Scheduled::CloseMenu));
        self.scheduled
            .push((now + MENU_TIMEOUT_US, Scheduled::CloseMenu));
        let slot = self.frame(Layer::Base).and_then(|f| {
            let i = f.cells.iter().position(|c| c.key == key)?;
            let (r, zones) = (*f.layout.items.get(i)?, &f.layout.zones);
            let zi = zones.iter().position(|z| r.x >= z.x && r.x < z.x + z.w)?;
            // The menu covers every zone but the first, and the last when there are more.
            let covered = zi >= 1 && (zi + 1 < zones.len() || zones.len() <= 2);
            // The menu's items are spaced like the button's own zone.
            let gap = f.layout.items[..i]
                .iter()
                .rev()
                .find(|p| p.x >= zones[zi].x && p.x < r.x)
                .map_or(0.0, |p| r.x - (p.x + p.w));
            covered.then(|| ((r.x - zones[1].x - gap).max(0.0), r.w))
        });
        self.overlay = Some(overlay::Overlay::Menu { items, slot });
        fx.extend(self.cancel_touches());
        fx.push(Effect::Redraw);
    }

    /// Closes the menu or drawer covering the bar.
    fn close_overlay(&mut self, fx: &mut Vec<Effect>) {
        self.overlay = None;
        fx.extend(self.cancel_touches());
        fx.push(Effect::Redraw);
    }

    /// One of the agent's own operations ([`crate::act`]). `value` is a drag's, 0–100.
    pub(super) fn act(&mut self, act: &Act, value: Option<f64>, now: u64, fx: &mut Vec<Effect>) {
        let pct = value.map(|v| v.clamp(0.0, 100.0).round() as u8);
        match act {
            Act::Session(op) => fx.push(Effect::Session(op.clone())),
            Act::Desktop(a) => fx.push(Effect::Desktop(a.clone())),
            Act::Set(level) => fx.extend(pct.map(|p| Effect::Session(level.set(p)))),
            Act::Seek { player } => fx.extend(pct.map(|pct| {
                Effect::Session(SessionOp::Seek {
                    player: player.clone(),
                    pct,
                })
            })),
            Act::Ui { key, change } => self.change_ui(key, change, now, fx),
            Act::Menu { key, items } => self.open_menu(key, items.clone(), now, fx),
            Act::Choose(d) => {
                self.close_overlay(fx);
                self.run(d, None, None, now, fx);
            }
            Act::Close => self.close_overlay(fx),
            Act::Unknown { kind, name } => {
                fx.push(Effect::Log(format!("{kind}: no action {name:?}")))
            }
        }
    }

    /// The agent-side state of the cell `key` changes.
    fn change_ui(&mut self, key: &str, change: &UiChange, now: u64, fx: &mut Vec<Effect>) {
        let ui = self.ui(key);
        match change {
            UiChange::Expand => ui.expanded = !ui.expanded,
            UiChange::Zone(step) => ui.tz = ui.tz.wrapping_add(*step),
            UiChange::Player(p) => ui.player = Some(p.clone()),
            UiChange::TimerReset => ui.timer = None,
            UiChange::Timer { total } => {
                let t = ui
                    .timer
                    .get_or_insert_with(|| modules::TimerState::new(*total));
                if t.finished {
                    *t = modules::TimerState::new(*total);
                } else if t.running_since.is_some() {
                    t.remaining = t.left(now);
                    t.running_since = None;
                    t.generation += 1;
                } else {
                    t.running_since = Some(now);
                    t.generation += 1;
                    let due = now + t.remaining.as_micros() as u64;
                    let generation = t.generation;
                    let key = key.to_owned();
                    self.scheduled
                        .push((due, Scheduled::TimerDone { key, generation }));
                }
            }
            UiChange::Emoji(op) => return self.emoji(key, op, now, fx),
        }
        fx.push(Effect::Redraw);
    }
}

impl Agent {
    /// The emoji strip `strip`'s controls.
    fn emoji(&mut self, strip: &str, op: &EmojiOp, now: u64, fx: &mut Vec<Effect>) {
        let ui = &mut self.ui(strip).emoji;
        match op {
            EmojiOp::Pick { insert, text } => {
                ui.tones = None;
                self.emoji.push(text);
                self.type_text(*insert, text, now, fx);
            }
            EmojiOp::Tones(e) => ui.tones = e.clone(),
            EmojiOp::Search => {
                ui.search = Some(String::new());
                ui.offset = 0;
                ui.tones = None;
            }
            EmojiOp::Type(c) => {
                let q = ui.search.get_or_insert_with(String::new);
                // Letters and single spaces only; the query stays short enough to mean something.
                let c = *c;
                if (c.is_ascii_lowercase() || (c == ' ' && !q.is_empty() && !q.ends_with(' ')))
                    && q.len() < 24
                {
                    q.push(c);
                }
                ui.offset = 0;
            }
            EmojiOp::Erase { all } => {
                if let Some(q) = ui.search.as_mut() {
                    if *all {
                        q.clear();
                    } else {
                        q.pop();
                    }
                }
                ui.offset = 0;
            }
            EmojiOp::EndSearch => {
                ui.search = None;
                ui.offset = 0;
            }
            EmojiOp::Tab(t) => {
                ui.tab = *t;
                ui.offset = 0;
                ui.tones = None;
            }
            EmojiOp::Page(d) => ui.offset = (ui.offset as i64 + d).max(0) as usize,
        }
        fx.push(Effect::Redraw);
    }

    /// Type `text` into the focused app. `auto` = wtype if installed, else copy to the clipboard;
    /// `wtype` types directly (virtual keyboard); `paste` copies and presses Ctrl+V through
    /// sliverd (works anywhere on Wayland, replaces the clipboard); `copy` only copies.
    fn type_text(&mut self, insert: Insert, text: &str, now: u64, fx: &mut Vec<Effect>) {
        let wtype = match insert {
            Insert::Auto => tool_exists("wtype"),
            i => i == Insert::Wtype,
        };
        let spawn = |argv: [&str; 3]| Effect::Spawn(argv.map(String::from).into());
        if wtype {
            fx.push(spawn(["wtype", "--", text]));
            return;
        }
        fx.push(spawn(["wl-copy", "--", text]));
        if insert == Insert::Paste
            && let (Some(ctrl), Some(v)) = (Key::from_name("LeftCtrl"), Key::from_name("V"))
        {
            // Give wl-copy a moment to own the selection before pasting.
            let paste = Do::Run(Action::Keys(vec![ctrl, v]));
            self.scheduled.push((now + 120_000, Scheduled::Do(paste)));
        }
    }
}

/// Is `name` an executable on `$PATH`?
pub(super) fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}
