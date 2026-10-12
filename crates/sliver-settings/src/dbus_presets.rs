//! Common `{ dbus = {...} }` actions, offered by the module inspector (features.md D-Bus row).
//! Each entry is the exact TOML a user would otherwise type; the tests parse every one with the
//! real config parser so a preset can't drift out of the schema.

/// One ready-made D-Bus call.
pub struct Preset {
    pub label: &'static str,
    /// Inline TOML of the action.
    pub toml: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        label: "Lock all sessions",
        toml: r#"{ dbus = { bus = "system", dest = "org.freedesktop.login1", path = "/org/freedesktop/login1", interface = "org.freedesktop.login1.Manager", method = "LockSessions" } }"#,
    },
    Preset {
        label: "Suspend",
        toml: r#"{ dbus = { bus = "system", dest = "org.freedesktop.login1", path = "/org/freedesktop/login1", interface = "org.freedesktop.login1.Manager", method = "Suspend", args = [false] } }"#,
    },
    Preset {
        label: "Reboot",
        toml: r#"{ dbus = { bus = "system", dest = "org.freedesktop.login1", path = "/org/freedesktop/login1", interface = "org.freedesktop.login1.Manager", method = "Reboot", args = [false] } }"#,
    },
    Preset {
        label: "Power off",
        toml: r#"{ dbus = { bus = "system", dest = "org.freedesktop.login1", path = "/org/freedesktop/login1", interface = "org.freedesktop.login1.Manager", method = "PowerOff", args = [false] } }"#,
    },
    Preset {
        label: "Spotify: play/pause",
        toml: r#"{ dbus = { dest = "org.mpris.MediaPlayer2.spotify", path = "/org/mpris/MediaPlayer2", interface = "org.mpris.MediaPlayer2.Player", method = "PlayPause" } }"#,
    },
    Preset {
        label: "Spotify: next track",
        toml: r#"{ dbus = { dest = "org.mpris.MediaPlayer2.spotify", path = "/org/mpris/MediaPlayer2", interface = "org.mpris.MediaPlayer2.Player", method = "Next" } }"#,
    },
    Preset {
        label: "VLC: play/pause",
        toml: r#"{ dbus = { dest = "org.mpris.MediaPlayer2.vlc", path = "/org/mpris/MediaPlayer2", interface = "org.mpris.MediaPlayer2.Player", method = "PlayPause" } }"#,
    },
    Preset {
        label: "dunst: close all notifications",
        toml: r#"{ dbus = { dest = "org.freedesktop.Notifications", path = "/org/dunstproject/cmd0", interface = "org.dunstproject.cmd0", method = "NotificationCloseAll" } }"#,
    },
    Preset {
        label: "dunst: show last notification",
        toml: r#"{ dbus = { dest = "org.freedesktop.Notifications", path = "/org/dunstproject/cmd0", interface = "org.dunstproject.cmd0", method = "NotificationShow" } }"#,
    },
    Preset {
        label: "GNOME: lock screen",
        toml: r#"{ dbus = { dest = "org.gnome.ScreenSaver", path = "/org/gnome/ScreenSaver", interface = "org.gnome.ScreenSaver", method = "Lock" } }"#,
    },
    Preset {
        label: "KDE: lock screen",
        toml: r#"{ dbus = { dest = "org.freedesktop.ScreenSaver", path = "/ScreenSaver", interface = "org.freedesktop.ScreenSaver", method = "Lock" } }"#,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use sliver_config::Action;

    const MPRIS_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

    #[test]
    fn every_preset_parses_as_a_dbus_action() {
        for p in PRESETS {
            let t: toml::Table = format!("a = {}", p.toml)
                .parse()
                .unwrap_or_else(|e| panic!("{}: {e}", p.label));
            let mut d = sliver_config::Diagnostics::default();
            let a = Action::parse(&t["a"], "a", &mut d);
            assert!(d.ok(), "{}: {d:?}", p.label);
            let Some(Action::DBus(c)) = a else {
                panic!("{}: not a dbus action: {a:?}", p.label)
            };
            assert!(!c.dest.is_empty() && c.path.starts_with('/'), "{}", p.label);
            if c.dest.starts_with("org.mpris.") {
                assert_eq!(c.interface, MPRIS_INTERFACE, "{}", p.label);
            }
        }
    }

    #[test]
    fn labels_are_unique() {
        let mut l: Vec<_> = PRESETS.iter().map(|p| p.label).collect();
        l.sort();
        l.dedup();
        assert_eq!(l.len(), PRESETS.len());
    }
}
