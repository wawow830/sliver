//! Specs for modules showing the compositor's state: keyboard layout, window, workspaces, taskbar.

use super::*;

/// fcitx5 (CJK input methods) takes over while it is running and has a current method:
/// the label is the method's own (拼, あ, en); the compositor's layout shows otherwise.
pub(super) fn language_input_method(world: &World) -> Option<Spec> {
    let s = &world.session;
    let im = s.input_method.as_ref()?;
    let v = ValueMap::new()
        .with("", esc(&im.current.label))
        .with("short", esc(&im.current.label))
        .with("input_method", esc(&im.current.name))
        .with("long", esc(&im.current.name))
        .with("id", esc(&im.current.id));
    let mut sp = Spec::new(v, "{}").on(Gesture::Tap, "im-toggle");
    if im.group.len() > 1 {
        sp.menu = im
            .group
            .iter()
            .map(|i| sliver_config::MenuEntry {
                label: format!(
                    "{}{} {}",
                    if i.id == im.current.id { "✓ " } else { "" },
                    i.label,
                    i.name
                ),
                icon: None,
                on_tap: Action::ModuleAction(format!("language:im:{}", i.id)),
            })
            .collect();
    }
    Some(sp)
}

pub(super) fn language(world: &World) -> Option<Spec> {
    let l = world.desktop.keyboard_layout.as_deref()?;
    // The same names as the fcitx5 variant, so one `format` works whichever is running.
    let v = ValueMap::new()
        .with("", esc(l))
        .with("long", esc(l))
        .with("short", esc(l))
        .with("input_method", esc(l));
    let mut sp = Spec::new(v, "{}").on(Gesture::Tap, "next");
    sp.target = Some(sliver_desktop::control::Action::NextLayout);
    // Hold lists the configured layouts (when the compositor says what they are).
    let d = &world.desktop;
    if d.keyboard_layouts.len() > 1 {
        sp.menu = d
            .keyboard_layouts
            .iter()
            .enumerate()
            .map(|(i, name)| sliver_config::MenuEntry {
                label: if d.keyboard_layout_index == Some(i) {
                    format!("✓ {name}")
                } else {
                    name.clone()
                },
                icon: None,
                on_tap: Action::ModuleAction(format!("language:set:{i}")),
            })
            .collect();
    }
    Some(sp)
}

pub(super) fn window(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let w = world.desktop.window.as_ref()?;
    // waybar's `rewrite`; bad rules were already reported by the config validator.
    let title = match m.config.get("rewrite") {
        Some(r) => sliver_config::Rewrite::parse(r, "", &mut Default::default()).apply(&w.title),
        None => w.title.clone(),
    };
    Some(Spec::new(
        ValueMap::new()
            .with("title", esc(&title))
            .with("app_id", esc(&w.class))
            .with("", esc(&title)),
        "{title}",
    ))
}

pub(super) fn mode(world: &World) -> Option<Spec> {
    let submap = world.desktop.submap.as_deref()?;
    Some(Spec::new(ValueMap::new().with("", esc(submap)), "{}"))
}

pub(super) fn workspaces(world: &World, item: Option<usize>) -> Option<Spec> {
    let ws = world.desktop.workspaces.get(item?)?;
    let status = if ws.urgent {
        "urgent"
    } else if ws.active {
        "active"
    } else if ws.windows == 0 {
        "empty"
    } else {
        "default"
    };
    let v = ValueMap::new()
        .with("name", esc(&ws.name))
        .with("id", ws.id)
        .with("windows", ws.windows as i64)
        .with("", esc(&ws.name));
    let mut sp = Spec::new(v, "{name}").on(Gesture::Tap, "focus");
    sp.status = Some(status);
    sp.checked = ws.active;
    sp.target = Some(sliver_desktop::control::Action::FocusWorkspace(ws.clone()));
    Some(sp)
}

pub(super) fn taskbar(world: &World, item: Option<usize>) -> Option<Spec> {
    let w = world.desktop.windows.get(item?)?;
    let v = ValueMap::new()
        .with("title", esc(&w.title))
        .with("app_id", esc(&w.class))
        .with("", esc(&w.title));
    // Icon only by default: the bar is too narrow for titles.
    let mut sp = Spec::new(v, "")
        .on(Gesture::Tap, "focus")
        .on(Gesture::Hold, "close");
    let entry = world.apps.find_by_app_id(&w.class);
    sp.default_icon = Some(
        entry
            .and_then(|e| e.icon.clone())
            .unwrap_or_else(|| w.class.clone()),
    );
    sp.checked = w.focused;
    sp.extra_classes = [(w.focused, "focused"), (w.urgent, "urgent")]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, c)| c.to_string())
        .chain([super::app_class(entry.map_or(&w.class, |e| &e.id))])
        .collect();
    sp.target = Some(sliver_desktop::control::Action::FocusWindow {
        address: w.address.clone(),
    });
    Some(sp)
}
