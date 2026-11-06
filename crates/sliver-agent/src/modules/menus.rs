//! Sub-bars: a button's `menu`, the `power` module's menus, and the one-shot actions behind
//! `lock` and `screenshot`.

use super::*;

/// A logind `Manager` call on the system bus.
pub(super) fn logind(method: &str, args: Vec<toml::Value>) -> Action {
    Action::DBus(sliver_config::DBusCall {
        system_bus: true,
        dest: "org.freedesktop.login1".into(),
        path: "/org/freedesktop/login1".into(),
        interface: "org.freedesktop.login1.Manager".into(),
        method: method.into(),
        args,
    })
}

/// The `power` module's sub-bars: `dangerous` = the long-press one (reboot, power off).
pub fn power_menu(dangerous: bool) -> Vec<sliver_config::MenuEntry> {
    let entry = |label: &str, icon: &str, on_tap: Action| sliver_config::MenuEntry {
        label: label.into(),
        icon: Some(icon.into()),
        on_tap,
    };
    let interactive = vec![toml::Value::Boolean(false)];
    if dangerous {
        vec![
            entry(
                "Reboot",
                "system-reboot-symbolic",
                logind("Reboot", interactive.clone()),
            ),
            entry(
                "Power off",
                "system-shutdown-symbolic",
                logind("PowerOff", interactive),
            ),
        ]
    } else {
        vec![
            entry("Lock", "system-lock-screen-symbolic", lock_action()),
            entry(
                "Suspend",
                "media-playback-pause-symbolic",
                logind("Suspend", interactive),
            ),
        ]
    }
}

/// Lock this session the way `loginctl lock-session` does: logind tells the locker.
pub(super) fn lock_action() -> Action {
    Action::DBus(sliver_config::DBusCall {
        system_bus: true,
        dest: "org.freedesktop.login1".into(),
        path: "/org/freedesktop/login1/session/auto".into(),
        interface: "org.freedesktop.login1.Session".into(),
        method: "Lock".into(),
        args: Vec::new(),
    })
}

/// The portal's interactive screenshot (the user picks area/window in the portal's own UI).
pub(super) fn screenshot_action() -> Action {
    let mut opts = toml::Table::new();
    opts.insert("interactive".into(), toml::Value::Boolean(true));
    Action::DBus(sliver_config::DBusCall {
        system_bus: false,
        dest: "org.freedesktop.portal.Desktop".into(),
        path: "/org/freedesktop/portal/desktop".into(),
        interface: "org.freedesktop.portal.Screenshot".into(),
        method: "Screenshot".into(),
        args: vec![toml::Value::String(String::new()), toml::Value::Table(opts)],
    })
}

/// What the `screenshot` and `lock` modules do on tap.
pub fn oneshot_action(name: &str) -> Option<Action> {
    match name {
        "screenshot" => Some(screenshot_action()),
        "lock" => Some(lock_action()),
        _ => None,
    }
}

/// The cells of an open `menu` sub-bar: a ✕, then one per entry (label and/or icon, as wide as
/// that needs). The ✕ comes first and, given the opening button's `slot` (offset, width), takes
/// its place, so tapping there again dismisses instead of acting; so does any space around the
/// entries.
pub fn menu_cells(
    entries: &[sliver_config::MenuEntry],
    slot: Option<(f32, f32)>,
    icons: &mut Icons,
    icon_size: u32,
) -> Vec<Cell> {
    let cell = |key: String, content: Content, width: Width, classes: &[&str], tap: String| Cell {
        key,
        id: "menu".into(),
        kind: "menu".into(),
        mic: false,
        slider: false,
        css_id: "menu".into(),
        classes: classes.iter().map(|c| c.to_string()).collect(),
        width,
        content,
        touch: Touch::Agent,
        on: vec![(Gesture::Tap, Action::ModuleAction(tap))],
        next_change: None,
        pressed: false,
        checked: false,
        drawer: None,
        disabled: false,
        foreground: None,
        background: None,
        published: None,
        pass_through: false,
        target: None,
        timer_total: None,
        menu: Vec::new(),
        slot: None,
        spectrum: Vec::new(),
    };
    let x = icons
        .get("window-close-symbolic", None, icon_size)
        .map_or_else(|| Content::markup("✕"), Content::icon);
    let mut out = Vec::new();
    if let Some((offset, _)) = slot.filter(|s| s.0 > 0.0) {
        out.push(cell(
            "menu/before".into(),
            Content::Spacer,
            Width::Fixed(offset),
            &["menu-spacer"],
            "menu:close".into(),
        ));
    }
    out.push(cell(
        "menu/close".into(),
        x,
        Width::Fixed(slot.map_or(72.0, |s| s.1)),
        &["menu-close"],
        "menu:close".into(),
    ));
    for (i, e) in entries.iter().enumerate() {
        let icon = e
            .icon
            .as_deref()
            .and_then(|n| icons.get(n, None, icon_size));
        let label = sliver_render::markup::escape(&e.label);
        let content = match (icon, label.is_empty()) {
            (Some(ic), false) => Content::label(Some(ic), label),
            (Some(ic), true) => Content::icon(ic),
            (None, _) => Content::markup(label),
        };
        out.push(cell(
            format!("menu/{i}"),
            content,
            Width::Auto,
            &["menu-item"],
            format!("menu:pick:{i}"),
        ));
    }
    if !entries.is_empty() {
        out.push(cell(
            "menu/after".into(),
            Content::Spacer,
            Width::Fill(1),
            &["menu-spacer"],
            "menu:close".into(),
        ));
    }
    out
}

/// A button's `menu`, its labels filled in with the button's own placeholders, so one entry
/// names what it acts on ("Close {name}" reads "Close Firefox"). Labels stay plain text.
pub(super) fn button_menu(
    menu: &[sliver_config::MenuEntry],
    values: &dyn sliver_config::Values,
) -> Vec<sliver_config::MenuEntry> {
    menu.iter()
        .map(|e| match sliver_config::Template::parse(&e.label) {
            Ok(t) if e.label.contains('{') => sliver_config::MenuEntry {
                label: sliver_render::markup::parse(&t.render(values)).plain(),
                ..e.clone()
            },
            _ => e.clone(),
        })
        .collect()
}
