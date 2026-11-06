//! Specs for modules showing the compositor's state: keyboard layout, window, workspaces, taskbar.

use super::*;
use crate::agent::SessionOp;
use sliver_desktop::control::Action as Control;

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
    let mut sp = Spec::new(v, "{}").does(
        Gesture::Tap,
        "im-toggle",
        Act::Session(SessionOp::InputMethod(None)),
    );
    if im.group.len() > 1 {
        sp.menu = im
            .group
            .iter()
            .map(|i| MenuItem {
                label: format!(
                    "{}{} {}",
                    if i.id == im.current.id { "✓ " } else { "" },
                    i.label,
                    i.name
                ),
                icon: None,
                on_tap: Act::Session(SessionOp::InputMethod(Some(i.id.clone()))).into(),
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
    let mut sp = Spec::new(v, "{}").does(Gesture::Tap, "next", Act::Desktop(Control::NextLayout));
    // Hold lists the configured layouts (when the compositor says what they are).
    let d = &world.desktop;
    if d.keyboard_layouts.len() > 1 {
        sp.menu = d
            .keyboard_layouts
            .iter()
            .enumerate()
            .map(|(i, name)| MenuItem {
                label: if d.keyboard_layout_index == Some(i) {
                    format!("✓ {name}")
                } else {
                    name.clone()
                },
                icon: None,
                on_tap: Act::Desktop(Control::SetLayout(i)).into(),
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

pub(super) fn workspaces(world: &World, item: Option<Part>) -> Option<Spec> {
    let ws = world.desktop.workspaces.get(item?.nth()?)?;
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
    let focus = Act::Desktop(Control::FocusWorkspace(ws.clone()));
    let mut sp = Spec::new(v, "{name}").does(Gesture::Tap, "focus", focus);
    sp.status = Some(status);
    sp.checked = ws.active;
    Some(sp)
}

pub(super) fn taskbar(m: &ModuleInstance, world: &World, item: Option<Part>) -> Option<Spec> {
    let i = match item? {
        Part::Pin(p) => return pinned(m, world, p),
        Part::Nth(i) => i,
    };
    let w = world.desktop.windows.get(i)?;
    let entry = world.apps.find_by_app_id(&w.class);
    let v = ValueMap::new()
        .with("title", esc(&w.title))
        .with("app_id", esc(&w.class))
        .with("name", esc(entry.map_or(&w.class, |e| &e.name)))
        .with("", esc(&w.title));
    // Icon only by default: the bar is too narrow for titles.
    let address = w.address.clone();
    let mut sp = Spec::new(v, "")
        .does(
            Gesture::Tap,
            "focus",
            Act::Desktop(Control::FocusWindow {
                address: address.clone(),
            }),
        )
        .does(
            Gesture::Hold,
            "close",
            Act::Desktop(Control::CloseWindow { address }),
        );
    // A window whose app has no icon: its name's first letters.
    sp.fallback_text = Some(w.class.chars().take(2).collect());
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

/// A launcher for the module's `p`th pinned app: tap focuses its window when it is running, else
/// starts it (`gtk-launch`); hold closes the running window. The same `format` as the window
/// buttons (icon only by default).
fn pinned(m: &ModuleInstance, world: &World, p: usize) -> Option<Spec> {
    let id = pinned_ids(m).into_iter().nth(p)?;
    let entry = world.apps.get(&id);
    let name = entry.map_or(id.as_str(), |e| e.name.as_str());
    let win = pinned_window(world, &id).map(|i| &world.desktop.windows[i]);
    let focused = win.is_some_and(|w| w.focused);
    let v = ValueMap::new()
        .with("name", esc(name))
        .with("title", esc(win.map_or("", |w| w.title.as_str())))
        .with("app_id", esc(win.map_or(id.as_str(), |w| w.class.as_str())));
    let mut sp = Spec::new(v, "");
    sp.status = focused.then_some("focused");
    sp.checked = focused;
    sp.default_icon = Some(
        entry
            .and_then(|e| e.icon.clone())
            .unwrap_or_else(|| id.clone()),
    );
    sp.fallback_text = Some(name.chars().take(2).collect());
    sp.extra_classes = vec![
        "pinned".into(),
        app_class(&id),
        if win.is_some() { "running" } else { "idle" }.into(),
    ];
    sp.extra_classes.extend(
        [
            (focused, "focused"),
            (win.is_some_and(|w| w.urgent), "urgent"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, c)| c.to_owned()),
    );
    Some(match win {
        Some(w) => {
            let address = w.address.clone();
            let focus = Control::FocusWindow {
                address: address.clone(),
            };
            sp.does(Gesture::Tap, "focus", Act::Desktop(focus)).does(
                Gesture::Hold,
                "close",
                Act::Desktop(Control::CloseWindow { address }),
            )
        }
        None => sp.does(Gesture::Tap, "focus", Do::Run(Action::Open(id))),
    })
}

/// A taskbar's items: its pinned apps first (launchers, or the running app's window), then the
/// other windows it lists.
pub(super) fn taskbar_items(m: &ModuleInstance, world: &World) -> Vec<(String, Part)> {
    let pins = pinned_ids(m);
    let shown: Vec<usize> = pins
        .iter()
        .filter_map(|p| pinned_window(world, p))
        .collect();
    let windows = listed(m, world).into_iter().filter(|i| !shown.contains(i));
    (0..pins.len())
        .map(|p| (format!("pin{p}"), Part::Pin(p)))
        .chain(windows.map(|i| (world.desktop.windows[i].address.clone(), Part::Nth(i))))
        .collect()
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
