//! `Control1` requests (sliverctl and friends) and the other changes from outside. Each method
//! applies its own consequences (a redraw, cancelled touches).

use super::*;
use crate::api::{Control, ControlReply, Prop, Props, Reply};

impl Agent {
    /// Answers `req` from the agent's own state. `Err(req)` hands back what needs the outside
    /// world (config files, module processes); the caller answers that. A request either applies
    /// whole or changes nothing.
    pub fn control(
        &mut self,
        req: Control,
        now: u64,
        fx: &mut Vec<Effect>,
    ) -> Result<Reply, Control> {
        let layer = |name: String, mode| Action::Layer { name, mode };
        let done = Ok(ControlReply::Done);
        let before = fx.len();
        let reply = match req {
            Control::SwitchLayer(name) | Control::PushLayer(name, _) => {
                self.run_action(&layer(name, LayerMode::Toggle), now, fx);
                if fx[before..].contains(&Effect::Redraw) {
                    done
                } else {
                    Err("no such layer".into())
                }
            }
            Control::ToggleLayer(name) => {
                self.run_action(&layer(name, LayerMode::Toggle), now, fx);
                done
            }
            Control::PopLayer => {
                self.run_action(&Action::CloseLayer, now, fx);
                done
            }
            Control::Signal(n) => {
                fx.push(Effect::Signal(n));
                done
            }
            Control::Trigger(module, g) => {
                self.trigger(&module, g, now, fx);
                done
            }
            Control::Bar(what) => {
                self.hidden = match what.as_str() {
                    "show" => false,
                    "hide" => true,
                    _ => !self.hidden,
                };
                fx.push(Effect::Redraw);
                done
            }
            Control::SetVariable(k, v) => {
                self.ctx_overrides.retain(|(x, _)| *x != k);
                let value = match v {
                    Prop::Bool(b) => Some(CtxValue::Bool(b)),
                    Prop::Str(s) => Some(CtxValue::Str(s)),
                    Prop::Num(n) => Some(CtxValue::Num(n)),
                    Prop::Int(n) => Some(CtxValue::Num(n as f64)),
                    _ => None,
                };
                if let Some(v) = value {
                    self.ctx_overrides.push((k, v));
                }
                fx.push(Effect::Redraw);
                done
            }
            // No config: what the bar shows now. A config is drawn by a new agent (the caller's).
            Control::RenderPreview(toml, _) if toml.is_empty() => self
                .frame(if self.fn_held { Layer::Fn } else { Layer::Base })
                .ok_or_else(|| "nothing drawn yet".to_string())
                .and_then(|f| f.image.encode_png().map_err(|e| e.to_string()))
                .map(ControlReply::Png),
            Control::ListModules => Ok(ControlReply::Modules(self.list_modules())),
            Control::SetLayout(name) => self.set_layout(&name).map(|()| {
                fx.push(Effect::Redraw);
                ControlReply::Done
            }),
            Control::Hide(m) | Control::Show(m) | Control::Toggle(m) if m.is_empty() => {
                Err("which module?".into())
            }
            Control::Hide(m) => {
                self.hidden_modules.insert(m);
                fx.push(Effect::Redraw);
                done
            }
            Control::Show(m) => {
                self.hidden_modules.remove(&m);
                fx.push(Effect::Redraw);
                done
            }
            Control::Toggle(m) => {
                if !self.hidden_modules.remove(&m) {
                    self.hidden_modules.insert(m);
                }
                fx.push(Effect::Redraw);
                done
            }
            Control::ResetVisibility => {
                self.hidden_modules.clear();
                fx.push(Effect::Redraw);
                done
            }
            Control::Update(module, props) => self.update(module, props).map(|()| {
                fx.push(Effect::Redraw);
                ControlReply::Done
            }),
            req @ (Control::Refresh(_)
            | Control::RenderPreview(..)
            | Control::SetProfile(_)
            | Control::ReloadConfig) => return Err(req),
        };
        Ok(reply)
    }

    /// `sliverctl update MODULE KEY=VALUE…`: output for an exec-less custom module. All of it or
    /// none of it: a mistyped field leaves the module as it was.
    fn update(&mut self, module: String, props: Props) -> Result<(), String> {
        let mut out = self.pushed.get(&module).cloned().unwrap_or_default();
        for (k, v) in props {
            match (k.as_str(), v) {
                ("text", Prop::Str(s)) => out.text = s,
                ("icon", Prop::Str(s)) => out.icon = Some(s),
                ("alt", Prop::Str(s)) => out.alt = Some(s),
                ("class", Prop::Str(s)) => {
                    out.classes = s.split_whitespace().map(str::to_owned).collect()
                }
                ("percentage", Prop::Num(n)) => out.percentage = Some(n),
                ("percentage", Prop::Int(n)) => out.percentage = Some(n as f64),
                ("value", Prop::Num(n)) => out.value = Some(n),
                ("value", Prop::Int(n)) => out.value = Some(n as f64),
                ("hidden", Prop::Bool(b)) => out.hidden = b,
                ("tooltip" | "detail", Prop::Str(s)) => out.detail = Some(s),
                (k, _) => return Err(format!("unknown or mistyped output field {k:?}")),
            }
        }
        self.pushed.insert(module, out);
        Ok(())
    }

    /// sliverd turned the bar on, dimmed it or turned it off. Waking redraws (nothing was drawn
    /// while off); turning off lets go of every finger.
    pub fn set_display(&mut self, state: DisplayState) -> Vec<Effect> {
        let was_off = self.display == DisplayState::Off;
        self.display = state;
        if state == DisplayState::Off {
            self.cancel_touches()
        } else if was_off {
            vec![Effect::Redraw]
        } else {
            Vec::new()
        }
    }

    /// The hatch (sliverd's own layer behind Fn) opened or closed: it takes the fingers.
    pub fn set_hatch(&mut self, open: bool) -> Vec<Effect> {
        self.hatch = open;
        if open {
            self.cancel_touches()
        } else {
            Vec::new()
        }
    }

    /// Fn is held: sliverd shows our `fn` frame.
    pub fn set_fn_held(&mut self, held: bool) {
        self.fn_held = held;
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn display(&self) -> DisplayState {
        self.display
    }

    pub fn fn_held(&self) -> bool {
        self.fn_held
    }

    /// The bar's size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Problems resolving layers in the last render.
    pub fn errors(&self) -> &[String] {
        &self.errors
    }
}
