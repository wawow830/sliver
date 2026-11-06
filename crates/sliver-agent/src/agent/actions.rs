//! What actions do inside the agent: layouts and layers, module actions, menus, published
//! items, and the emoji picker.

use super::*;

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
        self.held = None;
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

    /// `Control1.HoldLayout…` (Mirror): [`Agent::set_layout`], and every touch on it is reported
    /// as [`Effect::HeldPressed`]. With `select_only`, touching only selects (nothing fires).
    pub fn hold_layout(&mut self, layout: &str, select_only: bool) -> Result<(), String> {
        self.set_layout(layout)?;
        if !layout.is_empty() {
            self.held = Some(select_only);
        }
        Ok(())
    }

    /// Is `region` an item of the held layout as the bar shows it now (not another layout, a
    /// sub-bar, a popover or the Fn layer)?
    pub(super) fn on_held(&self, region: u32) -> bool {
        self.held.is_some()
            && region <= FN_REGION_BASE
            && self.menu.is_none()
            && self.popover.is_none()
            && self.subbar.is_none()
            && self.layout_override.as_ref().map(|o| &o.0) == self.active.as_ref().map(|a| &a.0)
    }

    /// What a touch on `region` selects in the held layout (`HeldPressed`): the cell's key, its
    /// zone's name, and the module's index in that zone's `modules`. `None` when nothing is
    /// held, or the bar shows something else (another layout, a sub-bar, a popover, Fn).
    pub(super) fn held_target(&self, region: u32) -> Option<(String, String, u32)> {
        if !self.on_held(region) {
            return None;
        }
        let (name, config) = self.layout_override.as_ref()?;
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
        let zones = config.layout_zones(name).ok()?;
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
        let source = match (&self.layout_override, rule) {
            (Some(_), _) => "forced",
            (None, Some(_)) => "rule",
            (None, None) => "default",
        };
        Some((layout, rule, name, source))
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
    /// Opens `entries` as `cell`'s menu: their module actions act on it, the ✕ takes its place
    /// if the menu covers it, and it closes on its own after [`MENU_TIMEOUT_US`].
    pub(super) fn open_menu(
        &mut self,
        cell: &Cell,
        entries: Vec<sliver_config::MenuEntry>,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
        self.scheduled
            .retain(|s| !matches!(s.1, Scheduled::CloseMenu));
        self.scheduled
            .push((now + MENU_TIMEOUT_US, Scheduled::CloseMenu));
        self.menu_slot = self.frame(Layer::Base).and_then(|f| {
            let i = f.cells.iter().position(|c| c.key == cell.key)?;
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
        self.menu = Some(entries);
        self.menu_owner = Some(cell.clone());
        fx.extend(self.cancel_touches());
        fx.push(Effect::Redraw);
    }

    pub(super) fn module_action(
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
            ("backlight", "set", Some(p)) => SessionOp::ScreenBrightness(p),
            ("kbd-backlight", "set", Some(p)) => SessionOp::KeyboardBrightness(p),
            ("audio", "set", Some(p)) => SessionOp::Volume {
                mic: cell.mic,
                pct: p,
            },
            ("audio", "toggle-mute", _) => SessionOp::ToggleMute { mic: cell.mic },
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
            ("clock", "tz-next" | "tz-prev", _) => {
                let key = cell.key.clone();
                let step = if name == "tz-next" { 1 } else { -1 };
                let ui = self.ui(&key);
                ui.tz = ui.tz.wrapping_add(step);
                fx.push(Effect::Redraw);
                return;
            }
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
                    self.open_menu(cell, cell.menu.clone(), now, fx);
                }
                return;
            }
            ("menu", "menu:close", _) => {
                self.menu = None;
                self.menu_owner = None;
                self.menu_slot = None;
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
                    self.menu_slot = None;
                    fx.extend(self.cancel_touches());
                    fx.push(Effect::Redraw);
                    // The `language` picker's entries are not config actions.
                    if let sliver_config::Action::ModuleAction(a) = &entry.on_tap
                        && let Some(id) = a.strip_prefix("language:im:")
                    {
                        fx.push(Effect::Session(SessionOp::InputMethod(Some(id.to_owned()))));
                        return;
                    }
                    if let sliver_config::Action::ModuleAction(a) = &entry.on_tap
                        && let Some(i) =
                            a.strip_prefix("language:set:").and_then(|n| n.parse().ok())
                    {
                        fx.push(Effect::Desktop(sliver_desktop::control::Action::SetLayout(
                            i,
                        )));
                        return;
                    }
                    match (&entry.on_tap, self.menu_owner.take()) {
                        (sliver_config::Action::ModuleAction(name), Some(owner)) => {
                            self.module_action(&owner, name, None, now, fx)
                        }
                        (a, _) => self.execute(a, None, None, now, fx),
                    }
                }
                return;
            }
            ("emoji", n, _) => {
                self.emoji_action(&cell.key, n, now, fx);
                return;
            }
            ("screenshot" | "lock", n, _) if crate::modules::oneshot_action(n).is_some() => {
                if let Some(act) = crate::modules::oneshot_action(n) {
                    self.execute(&act, None, None, now, fx);
                }
                return;
            }
            ("power", "power:hold", _) => {
                self.open_menu(cell, crate::modules::power_menu(true), now, fx);
                return;
            }
            ("language", "im-toggle", _) => {
                fx.push(Effect::Session(SessionOp::InputMethod(None)));
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
    pub(super) fn emoji_action(
        &mut self,
        cell_key: &str,
        name: &str,
        now: u64,
        fx: &mut Vec<Effect>,
    ) {
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
        } else if name == "search" {
            let ui = self.ui(&strip);
            ui.emoji.search = Some(String::new());
            ui.emoji.offset = 0;
            ui.emoji.tones = None;
            fx.push(Effect::Redraw);
        } else if let Some(c) = name.strip_prefix("s:").and_then(|c| c.chars().next()) {
            let ui = self.ui(&strip);
            let q = ui.emoji.search.get_or_insert_with(String::new);
            // Letters and single spaces only; the query stays short enough to mean something.
            if (c.is_ascii_lowercase() || (c == ' ' && !q.is_empty() && !q.ends_with(' ')))
                && q.len() < 24
            {
                q.push(c);
            }
            ui.emoji.offset = 0;
            fx.push(Effect::Redraw);
        } else if name == "s-back" || name == "s-clear" {
            let ui = self.ui(&strip);
            if let Some(q) = ui.emoji.search.as_mut() {
                if name == "s-clear" {
                    q.clear();
                } else {
                    q.pop();
                }
            }
            ui.emoji.offset = 0;
            fx.push(Effect::Redraw);
        } else if name == "s-close" {
            let ui = self.ui(&strip);
            ui.emoji.search = None;
            ui.emoji.offset = 0;
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
    pub(super) fn type_text(&mut self, method: &str, text: &str, now: u64, fx: &mut Vec<Effect>) {
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
pub(super) fn tool_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}
