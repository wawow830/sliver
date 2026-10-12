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
    // ReadOne (portal >= 1.?) returns the value; Read wraps it in one more variant.
    props::call_no_autostart::<_, OwnedValue>(conn, DEST, PATH, IFACE, "ReadOne", &(NS, key))
        .or_else(|| {
            props::call_no_autostart::<_, OwnedValue>(conn, DEST, PATH, IFACE, "Read", &(NS, key))
        })
}

/// Read both settings; `kdeglobals` fills in a missing accent.
pub fn read_into(conn: &Connection, a: &mut Appearance, kdeglobals: Option<&Path>) {
    a.accent = read_setting(conn, "accent-color").and_then(|v| parse_accent(&v));
    a.dark = read_setting(conn, "color-scheme").and_then(|v| match unwrap_variants(&v) {
        Value::U32(1) => Some(true),
        Value::U32(2) => Some(false),
        _ => None,
    });
    if a.accent.is_none() {
        a.accent = kdeglobals
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| kde_accent(&t));
    }
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
}
