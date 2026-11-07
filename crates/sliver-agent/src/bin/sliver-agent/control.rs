//! `Control1` requests from D-Bus (sliverctl and friends), answered by the core.

use super::*;

/// Answers what [`Driver::control`] can't: requests that need the config files or the module
/// processes. Changes reach the agent as `inputs`.
pub(super) fn control(
    driver: &mut Driver,
    live: &mut Live,
    reloader: &mut Reloader,
    req: Control,
    inputs: &mut Vec<Input>,
) -> Reply {
    let (answer, fx) = driver.control(req, &Now::read());
    inputs.push(Input::Effects(fx));
    let req = match answer {
        Ok(reply) => return reply,
        Err(req) => req,
    };
    // The agent gets what loaded, the caller what didn't.
    fn reload(reloader: &mut Reloader, live: &mut Live, inputs: &mut Vec<Input>) -> Vec<String> {
        let (applied, errors) = reloader.reload(live);
        inputs.extend(applied.map(Input::Config));
        errors
    }
    match req {
        Control::Refresh(m) => match live.modules().map(|rt| rt.trigger(&m)) {
            Some(true) => Ok(ControlReply::Done),
            _ => Err(format!("no user module {m:?}")),
        },
        // That config, drawn offscreen with the live state.
        Control::RenderPreview(toml, _) => match reloader.load_text(&toml) {
            Ok(a) => {
                let mut p = new_agent(&a, driver.agent().size());
                p.render(&live.world_reading(Now::read(), &sliver_agent::stats::wants(p.config())));
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
        Control::SetProfile(_) if reloader.explicit() => {
            Err("the agent runs with --config FILE, so profiles are off".into())
        }
        Control::SetProfile(name) => {
            let before = sliver_config::profile::active();
            sliver_config::profile::set_active(&name)?;
            let errors = reload(reloader, live, inputs);
            if errors.is_empty() {
                Ok(ControlReply::Done)
            } else {
                let _ = sliver_config::profile::set_active(&before);
                reload(reloader, live, inputs);
                Err(format!(
                    "profile {name:?} not applied: {}",
                    errors.join("; ")
                ))
            }
        }
        Control::ReloadConfig => Ok(ControlReply::Errors(reload(reloader, live, inputs))),
        other => Err(format!("{other:?}: the agent answers this itself")),
    }
}

/// The `Control1` properties while connected to sliverd. `Healthy` means sliverd's active agent,
/// not standby for another seat user; config, stylesheet and module problems are `Errors`.
pub(super) fn control_state(agent: &Agent, reloader: &Reloader, role: Role) -> ControlState {
    let (active_layout, rule, active_rule_name, source) = agent.active_layout().unwrap_or_default();
    ControlState {
        active_layer: agent.base_layer().to_owned(),
        active_layout,
        active_rule: rule.map_or(-1, |i| i as i32),
        active_rule_name,
        layout_source: source.to_owned(),
        layers: agent.config().layers.keys().cloned().collect(),
        active_profile: reloader.profile.0.clone(),
        profiles: reloader.profile.1.clone(),
        variables: Default::default(),
        healthy: role == Role::Active,
        errors: reloader
            .errors
            .iter()
            .chain(agent.errors())
            .cloned()
            .collect(),
    }
}
