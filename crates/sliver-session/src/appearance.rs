//! Desktop appearance: the accent colour and light/dark preference, from the XDG settings portal
//! (`org.freedesktop.appearance`), with KDE's `kdeglobals` as a fallback for the accent.
//!
//! Reads never start the portal (`NoAutoStart`); a portal that is not running just leaves the
//! values unknown. Changes arrive as `SettingChanged` signals.

use crate::props;
use serde::{Deserialize, Serialize};
use std::path::Path;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Settings";
const NS: &str = "org.freedesktop.appearance";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Appearance {
    /// The desktop accent colour, sRGB.
    pub accent: Option<[u8; 3]>,
    /// `Some(true)` = the user prefers a dark theme, `Some(false)` = light, `None` = no preference.
    pub dark: Option<bool>,
    /// The desktop turned animations off: GNOME's `enable-animations = false` (through the
    /// portal), or KDE's `AnimationDurationFactor=0`. `None` = unknown (animate).
    #[serde(default)]
    pub reduce_motion: Option<bool>,
}

/// The portal's accent value: `(ddd)` with each channel in 0..=1. Anything else, or a channel out
/// of range ("no accent set"), is `None`.
pub fn parse_accent(v: &Value<'_>) -> Option<[u8; 3]> {
    let v = unwrap_variants(v);
    let Value::Structure(s) = v else { return None };
    let f: Vec<f64> = s
        .fields()
        .iter()
        .filter_map(|x| match unwrap_variants(x) {
            Value::F64(d) => Some(*d),
            _ => None,
        })
        .collect();
    let [r, g, b] = f[..] else { return None };
    if ![r, g, b].iter().all(|c| (0.0..=1.0).contains(c)) {
        return None;
    }
    let ch = |c: f64| (c * 255.0).round() as u8;
    Some([ch(r), ch(g), ch(b)])
}

fn unwrap_variants<'a, 'b>(mut v: &'a Value<'b>) -> &'a Value<'b> {
    while let Value::Value(inner) = v {
        v = inner;
    }
    v
}

/// KDE: `[General]` → `AccentColor=61,174,233` in `kdeglobals`.
pub fn kde_accent(kdeglobals: &str) -> Option<[u8; 3]> {
    let mut in_general = false;
    for line in kdeglobals.lines() {
        let line = line.trim();
        if let Some(sec) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_general = sec == "General";
        } else if in_general {
            if let Some(v) = line.strip_prefix("AccentColor=") {
                let n: Vec<u8> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                if let [r, g, b] = n[..] {
                    return Some([r, g, b]);
                }
            }
        }
    }
    None
}

fn read_setting(conn: &Connection, key: &str) -> Option<OwnedValue> {
    read_ns(conn, NS, key)
}

fn read_ns(conn: &Connection, ns: &str, key: &str) -> Option<OwnedValue> {
    // ReadOne (portal >= 1.?) returns the value; Read wraps it in one more variant.
    props::call_no_autostart::<_, OwnedValue>(conn, DEST, PATH, IFACE, "ReadOne", &(ns, key))
        .or_else(|| {
            props::call_no_autostart::<_, OwnedValue>(conn, DEST, PATH, IFACE, "Read", &(ns, key))
        })
}

/// KDE: `[KDE]` → `AnimationDurationFactor=0` means animations are off.
pub fn kde_reduce_motion(kdeglobals: &str) -> Option<bool> {
    let mut in_kde = false;
    for line in kdeglobals.lines() {
        let line = line.trim();
        if let Some(sec) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_kde = sec == "KDE";
        } else if in_kde && let Some(v) = line.strip_prefix("AnimationDurationFactor=") {
            return v.trim().parse::<f64>().ok().map(|f| f <= 0.0);
        }
    }
    None
}

/// Read both settings; `kdeglobals` fills in a missing accent.
pub fn read_into(conn: &Connection, a: &mut Appearance, kdeglobals: Option<&Path>) {
    a.accent = read_setting(conn, "accent-color").and_then(|v| parse_accent(&v));
    a.dark = read_setting(conn, "color-scheme").and_then(|v| match unwrap_variants(&v) {
        Value::U32(1) => Some(true),
        Value::U32(2) => Some(false),
        _ => None,
    });
    // GNOME's switch, exposed by the GNOME/GTK portals (the GTK settings namespace).
    a.reduce_motion =
        read_ns(conn, "org.gnome.desktop.interface", "enable-animations").and_then(|v| {
            match unwrap_variants(&v) {
                Value::Bool(on) => Some(!on),
                _ => None,
            }
        });
    let kde = kdeglobals.and_then(|p| std::fs::read_to_string(p).ok());
    if a.accent.is_none() {
        a.accent = kde.as_deref().and_then(kde_accent);
    }
    if a.reduce_motion.is_none() {
        a.reduce_motion = kde.as_deref().and_then(kde_reduce_motion);
    }
}

/// The commands that switch the colour scheme, in the order to try them. KDE has its own
/// tool; elsewhere the GNOME setting is what the GTK/GNOME portals report as `color-scheme`.
pub fn dark_commands(desktop: &str, dark: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    let kde = desktop.split(':').any(|d| d.eq_ignore_ascii_case("KDE"));
    let mut v = Vec::new();
    if kde {
        v.push((
            "plasma-apply-colorscheme",
            vec![if dark { "BreezeDark" } else { "BreezeLight" }],
        ));
    }
    v.push((
        "gsettings",
        vec![
            "set",
            "org.gnome.desktop.interface",
            "color-scheme",
            if dark { "prefer-dark" } else { "default" },
        ],
    ));
    v
}

/// Switch between the dark and light colour scheme. The new value comes back through the
/// portal's `SettingChanged` (so the toggle shows what the desktop really did).
pub fn set_dark(dark: bool) -> Result<(), String> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    for (prog, args) in dark_commands(&desktop, dark) {
        let mut cmd = std::process::Command::new(prog);
        cmd.args(args);
        if let Some(o) = crate::audio::output_with_timeout(cmd, std::time::Duration::from_secs(3)) {
            if o.status.success() {
                return Ok(());
            }
        }
    }
    Err("could not switch the colour scheme (needs gsettings or plasma-apply-colorscheme)".into())
}

impl crate::Handle {
    /// Track the accent colour and colour scheme. `conn` is a session-bus connection dedicated
    /// to this. Missing portal: values stay unknown (`kdeglobals` still supplies a KDE accent).
    pub fn start_appearance(&self, conn: Connection, kdeglobals: Option<std::path::PathBuf>) {
        let signals = crate::subscribe(
            &conn,
            &[
                "type='signal',sender='org.freedesktop.portal.Desktop',interface='org.freedesktop.portal.Settings',member='SettingChanged'",
            ],
        );
        let hub = self.hub.clone();
        let mut a = Appearance::default();
        read_into(&conn, &mut a, kdeglobals.as_deref());
        hub.update(|s| s.appearance = a);
        std::thread::Builder::new()
            .name("sliver-session-appearance".into())
            .spawn(move || {
                for _ in signals {
                    let mut a = Appearance::default();
                    read_into(&conn, &mut a, kdeglobals.as_deref());
                    if !hub.update(|s| s.appearance = a) {
                        return;
                    }
                }
            })
            .expect("spawn thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::StructureBuilder;

    fn rgb(r: f64, g: f64, b: f64) -> Value<'static> {
        Value::Structure(
            StructureBuilder::new()
                .add_field(r)
                .add_field(g)
                .add_field(b)
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn dark_mode_commands() {
        let c = dark_commands("GNOME", true);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1.last(), Some(&"prefer-dark"));
        let c = dark_commands("KDE", false);
        assert_eq!(
            (c[0].0, c[0].1[0]),
            ("plasma-apply-colorscheme", "BreezeLight")
        );
        assert_eq!(c[1].1.last(), Some(&"default"), "gsettings as a fallback");
        assert_eq!(dark_commands("ubuntu:GNOME", false).len(), 1);
    }

    #[test]
    fn portal_accent() {
        assert_eq!(
            parse_accent(&rgb(0.2078, 0.5176, 0.8941)),
            Some([53, 132, 228])
        );
        assert_eq!(
            parse_accent(&Value::Value(Box::new(rgb(1.0, 0.0, 0.5)))),
            Some([255, 0, 128]),
            "nested variant"
        );
        assert_eq!(
            parse_accent(&rgb(-1.0, 0.0, 0.0)),
            None,
            "out of range = unset"
        );
        assert_eq!(parse_accent(&Value::U32(1)), None);
    }

    #[test]
    fn kde_fallback() {
        assert_eq!(
            kde_accent("[Other]\nAccentColor=1,2,3\n[General]\nAccentColor=61, 174,233\n"),
            Some([61, 174, 233])
        );
        assert_eq!(kde_accent("[General]\nColorScheme=Breeze\n"), None);
        assert_eq!(kde_accent("[General]\nAccentColor=bad\n"), None);
    }

    #[test]
    fn kde_animation_speed() {
        assert_eq!(
            kde_reduce_motion(
                "[General]\nAnimationDurationFactor=1\n[KDE]\nAnimationDurationFactor=0\n"
            ),
            Some(true)
        );
        assert_eq!(
            kde_reduce_motion("[KDE]\nAnimationDurationFactor=0.5\n"),
            Some(false)
        );
        assert_eq!(kde_reduce_motion("[KDE]\nLookAndFeelPackage=x\n"), None);
    }
}
