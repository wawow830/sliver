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
pub fn power_menu(dangerous: bool) -> Vec<MenuItem> {
    let entry = |label: &str, icon: &str, action: Action| MenuItem {
        label: label.into(),
        icon: Some(icon.into()),
        on_tap: Do::Run(action),
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
pub fn lock_action() -> Action {
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
pub fn screenshot_action() -> Action {
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

/// The cells of an open `menu` sub-bar: a ✕, then one per entry (label and/or icon, as wide as
/// that needs). The ✕ comes first and, given the opening button's `slot` (offset, width), takes
/// its place, so tapping there again dismisses instead of acting; so does any space around the
/// entries.
pub fn menu_cells(
    items: &[MenuItem],
    slot: Option<(f32, f32)>,
    icons: &mut Icons,
    icon_size: u32,
) -> Vec<Cell> {
    let cell = |key: &str, content: Content, width: Width, class: &str, tap: Act| {
        let mut c = Cell::new(key, "menu", "menu", "menu", content).tapped(tap);
        c.classes = vec![class.to_owned()];
        c.width = width;
        c
    };
    let x = icons
        .get("window-close-symbolic", None, icon_size)
        .map_or_else(|| Content::markup("✕"), Content::icon);
    let mut out = Vec::new();
    if let Some((offset, _)) = slot.filter(|s| s.0 > 0.0) {
        out.push(cell(
            "menu/before",
            Content::Spacer,
            Width::Fixed(offset),
            "menu-spacer",
            Act::Close,
        ));
    }
    out.push(cell(
        "menu/close",
        x,
        Width::Fixed(slot.map_or(72.0, |s| s.1)),
        "menu-close",
        Act::Close,
    ));
    for (i, e) in items.iter().enumerate() {
        let icon = e
            .icon
            .as_deref()
            .and_then(|n| icons.get(n, None, icon_size));
        out.push(cell(
            &format!("menu/{i}"),
            Content::label(icon, sliver_render::markup::escape(&e.label)),
            Width::Auto,
            "menu-item",
            Act::Choose(Box::new(e.on_tap.clone())),
        ));
    }
    if !items.is_empty() {
        out.push(cell(
            "menu/after",
            Content::Spacer,
            Width::Fill(1),
            "menu-spacer",
            Act::Close,
        ));
    }
    out
}

/// A button's `menu`, its labels filled in with the button's own placeholders, so one entry
/// names what it acts on ("Close {name}" reads "Close Firefox"), and its actions bound like the
/// button's own (a module action is one of the button's verbs). Labels stay plain text.
pub(super) fn button_menu(
    menu: &[sliver_config::MenuEntry],
    values: &dyn sliver_config::Values,
    kind: &str,
    verbs: &[(&'static str, Do)],
) -> Vec<MenuItem> {
    menu.iter()
        .map(|e| MenuItem {
            label: match sliver_config::Template::parse(&e.label) {
                Ok(t) if e.label.contains('{') => {
                    sliver_render::markup::parse(&t.render(values)).plain()
                }
                _ => e.label.clone(),
            },
            icon: e.icon.clone(),
            on_tap: Do::bind(&e.on_tap, kind, verbs),
        })
        .collect()
}
