//! `Control1` requests from D-Bus (sliverctl and friends), answered by the core.

use super::*;

/// A `Control1` request from D-Bus (sliverctl and friends), answered by the core.
pub(super) fn control(
    agent: &mut Agent,
    live: &mut Live,
    reloader: &mut Reloader,
    req: Control,
) -> (Reply, Vec<Effect>) {
    use sliver_config::{Action, LayerMode};
    let layer = |name: String, mode| Action::Layer { name, mode };
    let mut fx = Vec::new();
    let done = Ok(ControlReply::Done);
    let reply = match req {
        Control::SwitchLayer(name) | Control::PushLayer(name, _) => {
            agent.run_action(
                &layer(name, LayerMode::Toggle),
                Now::read().mono_us,
                &mut fx,
            );
            if !fx.contains(&Effect::Redraw) {
                Err("no such layer".into())
            } else {
                done
            }
        }
        Control::ToggleLayer(name) => {
            agent.run_action(
                &layer(name, LayerMode::Toggle),
                Now::read().mono_us,
                &mut fx,
            );
            done
        }
        Control::PopLayer => {
            agent.run_action(&Action::CloseLayer, Now::read().mono_us, &mut fx);
            done
        }
        Control::Signal(n) => {
            fx.push(Effect::Signal(n));
            done
        }
        Control::Refresh(m) => match live.modules().map(|rt| rt.trigger(&m)) {
            Some(true) => done,
            _ => Err(format!("no user module {m:?}")),
        },
        Control::Trigger(module, g) => {
            agent.trigger(&module, g, Now::read().mono_us, &mut fx);
            done
        }
        Control::Bar(what) => {
            agent.hidden = match what.as_str() {
                "show" => false,
                "hide" => true,
                _ => !agent.hidden,
            };
            fx.push(Effect::Redraw);
            done
        }
        Control::SetVariable(k, v) => {
            agent.ctx_overrides.retain(|(x, _)| *x != k);
            let value = match v {
                sliver_api::item::Prop::Bool(b) => Some(sliver_config::CtxValue::Bool(b)),
                sliver_api::item::Prop::Str(s) => Some(sliver_config::CtxValue::Str(s)),
                sliver_api::item::Prop::Num(n) => Some(sliver_config::CtxValue::Num(n)),
                sliver_api::item::Prop::Int(n) => Some(sliver_config::CtxValue::Num(n as f64)),
                _ => None,
            };
            if let Some(v) = value {
                agent.ctx_overrides.push((k, v));
            }
            fx.push(Effect::Redraw);
            done
        }
        Control::Preview(..) => {
            Err("Preview isn't implemented; RenderPreview draws a config".into())
        }
        // No config: what the bar shows now. Otherwise that config, drawn offscreen with the
        // live state.
        Control::RenderPreview(toml, _) if toml.is_empty() => agent
            .frame(if agent.fn_held {
                Layer::Fn
            } else {
                Layer::Base
            })
            .ok_or_else(|| "nothing drawn yet".to_string())
            .and_then(|f| f.image.encode_png().map_err(|e| e.to_string()))
            .map(ControlReply::Png),
        Control::RenderPreview(toml, _) => match reloader.load_text(&toml) {
            Ok(a) => {
                let size = (agent.width, agent.height);
                let mut p = new_agent(&a, size);
                p.render(&live.world(Now::read()));
                p.frame(Layer::Base)
                    .ok_or_else(|| "nothing to draw".to_string())
                    .and_then(|f| f.image.encode_png().map_err(|e| e.to_string()))
                    .map(ControlReply::Png)
            }
            Err(errors) => Err(errors.join("\n")),
        },
        Control::ValidateConfig(toml) => Ok(ControlReply::Errors(
            reloader.load_text(&toml).err().unwrap_or_default(),
        )),
        Control::ListModules => Ok(ControlReply::Modules(agent.list_modules())),
        Control::HoldLayout {
            layout,
            select_only,
        } => match agent.hold_layout(&layout, select_only) {
            Ok(()) => {
                fx.push(Effect::Redraw);
                done
            }
            Err(e) => Err(e),
        },
        Control::SetLayout(name) => match agent.set_layout(&name) {
            Ok(()) => {
                fx.push(Effect::Redraw);
                done
            }
            Err(e) => Err(e),
        },
        Control::Hide(m) | Control::Show(m) | Control::Toggle(m) if m.is_empty() => {
            Err("which module?".into())
        }
        Control::Hide(m) => {
            agent.hidden_modules.insert(m);
            fx.push(Effect::Redraw);
            done
        }
        Control::Show(m) => {
            agent.hidden_modules.remove(&m);
            fx.push(Effect::Redraw);
            done
        }
        Control::Toggle(m) => {
            if !agent.hidden_modules.remove(&m) {
                agent.hidden_modules.insert(m);
            }
            fx.push(Effect::Redraw);
            done
        }
        Control::ResetVisibility => {
            agent.hidden_modules.clear();
            fx.push(Effect::Redraw);
            done
        }
        Control::Update(module, props) => {
            use sliver_api::item::Prop;
            let out = agent.pushed.entry(module).or_default();
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
                    (k, _) => {
                        return (Err(format!("unknown or mistyped output field {k:?}")), fx);
                    }
                }
            }
            fx.push(Effect::Redraw);
            done
        }
        Control::SetProfile(name) => {
            if reloader.explicit() {
                Err("the agent runs with --config FILE, so profiles are off".into())
            } else {
                let before = sliver_config::profile::active();
                match sliver_config::profile::set_active(&name) {
                    Err(e) => Err(e),
                    Ok(()) => {
                        let errors = reloader.reload(Some(agent), live);
                        fx.push(Effect::Redraw);
                        if errors.is_empty() {
                            Ok(ControlReply::Done)
                        } else {
                            // Keep the profile that works, and say why the new one didn't.
                            let _ = sliver_config::profile::set_active(&before);
                            reloader.reload(Some(agent), live);
                            Err(format!(
                                "profile {name:?} not applied: {}",
                                errors.join("; ")
                            ))
                        }
                    }
                }
            }
        }
        Control::ReloadConfig => {
            let errors = reloader.reload(Some(agent), live);
            fx.push(Effect::Redraw);
            Ok(ControlReply::Errors(errors))
        }
    };
    (reply, fx)
}

/// The `Control1` properties while connected to sliverd (api.md §7). `Healthy`: connected and
/// driving the bar (sliverd's active agent, not standby for another seat user); problems with
/// the config, stylesheet or modules are `Errors`.
pub(super) fn control_state(agent: &Agent, reloader: &Reloader, role: Role) -> ControlState {
    let (active_layout, rule, active_rule_name, source) = agent.active_layout().unwrap_or_default();
    ControlState {
        active_layer: agent.base_layer().to_owned(),
        active_layout,
        active_rule: rule.map_or(-1, |i| i as i32),
        active_rule_name,
        layout_source: source.to_owned(),
        layers: agent.config.layers.keys().cloned().collect(),
        active_profile: reloader.profile.0.clone(),
        profiles: reloader.profile.1.clone(),
        variables: Default::default(),
        healthy: role == Role::Active,
        errors: reloader
            .errors
            .iter()
            .chain(&agent.errors)
            .cloned()
            .collect(),
    }
}
