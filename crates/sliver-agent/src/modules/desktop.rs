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
    let entry = world.apps.find_by_app_id(&w.class);
    let v = ValueMap::new()
        .with("title", esc(&w.title))
        .with("app_id", esc(&w.class))
        .with("name", esc(entry.map_or(&w.class, |e| &e.name)))
        .with("", esc(&w.title));
    // Icon only by default: the bar is too narrow for titles.
    let mut sp = Spec::new(v, "")
        .on(Gesture::Tap, "focus")
        .on(Gesture::Hold, "close");
    // `format-focused` labels just the window you're in.
    sp.status = w.focused.then_some("focused");
    sp.default_icon = Some(
        entry
            .and_then(|e| e.icon.clone())
            .unwrap_or_else(|| w.class.clone()),
    );
    sp.checked = w.focused;
    sp.extra_classes = [(w.urgent, "urgent")]
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

/// `pinned = ["org.mozilla.firefox", "kitty"]`: desktop ids (with or without `.desktop`).
pub(super) fn pinned_ids(m: &ModuleInstance) -> Vec<String> {
    m.config
        .get("pinned")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(|s| s.strip_suffix(".desktop").unwrap_or(s).to_string())
        .collect()
}

/// The first window (any workspace) that belongs to the pinned app `id`.
pub(super) fn pinned_window(world: &World, id: &str) -> Option<usize> {
    world.desktop.windows.iter().position(|w| {
        world
            .apps
            .find_by_app_id(&w.class)
            .is_some_and(|e| e.id == id)
    })
}

/// The class a taskbar button gets for its app, so a stylesheet can treat one app's icon
/// differently: `app-` and its desktop id (else its app id), lowercased, other characters as
/// `-` (`app-org-kde-dolphin`). Pinned or not, an app gets the same one.
pub(crate) fn app_class(id: &str) -> String {
    let id: String = id
        .trim_end_matches(".desktop")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("app-{id}")
}

/// A launcher for a pinned app: tap focuses its window when it is running, else starts it
/// (`gtk-launch`); hold closes the running window.
#[allow(clippy::too_many_arguments)]
pub(super) fn pinned_cell(
    m: &ModuleInstance,
    key: &str,
    world: &World,
    icons: &mut Icons,
    icon_size: u32,
    index: usize,
    id: &str,
    window: Option<usize>,
) -> Cell {
    let entry = world.apps.get(id);
    let icon = entry
        .and_then(|e| e.icon.as_deref())
        .or(Some(id))
        .and_then(|n| {
            icons.get(
                n,
                m.common.icon_theme.as_deref(),
                m.common.icon_size.unwrap_or(icon_size),
            )
        });
    let name = entry.map_or(id, |e| e.name.as_str());
    let win = window.map(|i| &world.desktop.windows[i]);
    let focused = win.is_some_and(|w| w.focused);
    // The same `format` as the window buttons (icon only by default).
    let esc = |s: &str| FmtValue::Text(sliver_render::markup::escape(s));
    let values = ValueMap::new()
        .with("name", esc(name))
        .with("title", esc(win.map_or("", |w| w.title.as_str())))
        .with("app_id", esc(win.map_or(id, |w| w.class.as_str())));
    let st = State {
        status: focused.then_some("focused"),
        ..Default::default()
    };
    let label = format_module(&m.common, "", &st, &values).text;
    let content = match icon {
        Some(icon) if label.is_empty() => Content::icon(icon),
        Some(icon) => Content::label(Some(icon), label),
        None if label.is_empty() => Content::markup(sliver_render::markup::escape(
            &name.chars().take(2).collect::<String>(),
        )),
        None => Content::markup(label),
    };
    let target = win.map(|w| sliver_desktop::control::Action::FocusWindow {
        address: w.address.clone(),
    });
    let mut classes = vec!["pinned".to_string(), app_class(id)];
    classes.push(if win.is_some() { "running" } else { "idle" }.to_string());
    if focused {
        classes.push("focused".into());
    }
    if win.is_some_and(|w| w.urgent) {
        classes.push("urgent".into());
    }
    classes.extend(m.classes.iter().cloned());
    let tap = if win.is_some() {
        Action::ModuleAction("focus".into())
    } else {
        Action::Open(id.to_string())
    };
    let mut defaults = vec![(Gesture::Tap, tap)];
    if win.is_some() {
        defaults.push((Gesture::Hold, Action::ModuleAction("close".into())));
    }
    // The module's own bindings and `menu` apply to its launchers too.
    let on = bindings(m, defaults, !m.common.menu.is_empty());
    Cell {
        key: format!("{key}/pin{index}"),
        id: m.id.to_string(),
        kind: "taskbar".into(),
        mic: false,
        slider: false,
        css_id: m.css_id.clone(),
        classes,
        width: Width::Auto,
        content,
        touch: Touch::Agent,
        on,
        next_change: None,
        pressed: false,
        checked: win.is_some_and(|w| w.focused),
        drawer: None,
        disabled: false,
        foreground: None,
        background: None,
        published: None,
        pass_through: false,
        target,
        timer_total: None,
        menu: button_menu(&m.common.menu, &values),
        slot: None,
        spectrum: Vec::new(),
    }
}

/// Which workspaces/windows a `workspaces` / `taskbar` module shows (indexes into the lists).
/// Waybar semantics: workspaces of the focused output unless `all-outputs`; windows of the
/// active workspace unless `all-workspaces`.
pub(super) fn listed(m: &ModuleInstance, world: &World) -> Vec<usize> {
    let flag = |k: &str| m.config.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let d = &world.desktop;
    if m.id.kind == "workspaces" {
        let here = d.monitor.as_deref();
        return (0..d.workspaces.len())
            .filter(|&i| {
                flag("all-outputs")
                    || here.is_none()
                    || d.workspaces[i].monitor.is_none()
                    || d.workspaces[i].monitor.as_deref() == here
            })
            .collect();
    }
    let active = d.workspace.as_ref().map(|w| w.id);
    // niri's window workspace ids are niri ids: match against the active workspace's info.
    let active = d
        .workspaces
        .iter()
        .find(|w| w.active)
        .map(|w| w.id)
        .or(active);
    (0..d.windows.len())
        .filter(|&i| {
            flag("all-workspaces") || active.is_none() || Some(d.windows[i].workspace) == active
        })
        .collect()
}
