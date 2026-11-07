//! What actions do inside the agent: layouts and layers, module actions and menus.

use super::*;
use crate::act::{Act, UiChange};

impl Agent {
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
            .is_some_and(|f| !c.layouts().contains_key(&f.layout))
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
        });
        Ok(())
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
    pub fn trigger(&mut self, module: &str, g: CGesture, now: u64, fx: &mut Vec<Effect>) {
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
            CGesture::Tap => Gesture::Tap,
            CGesture::Hold => Gesture::Hold,
            CGesture::SwipeLeft => Gesture::SwipeLeft,
            CGesture::SwipeRight => Gesture::SwipeRight,
            CGesture::DoubleTap => Gesture::DoubleTap,
            _ => return,
        };
        self.dispatch(region, g, now, fx);
    }

    /// What's on the bar now, for `Control1.ListModules` / `sliverctl modules`.
    pub fn list_modules(&self) -> Vec<crate::api::Props> {
        use crate::api::Prop;
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
        }
        fx.push(Effect::Redraw);
    }
}
