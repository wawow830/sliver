//! tiny-dfr 0.3.7 config import (features.md §1, docs/hardware.md "Config").
//!
//! tiny-dfr merges `/usr/share/tiny-dfr/config.toml` with `/etc/tiny-dfr/config.toml` per
//! top-level key (the /etc one wins, and a list replaces the whole list). We do the same, then
//! translate it:
//!
//! - `PrimaryLayerKeys` → layout `tiny-dfr`, `MediaLayerKeys` → layout `tiny-dfr-media`. Each
//!   has an `esc` zone with `key/Esc#auto` (shown only without a physical Esc, which is exactly
//!   tiny-dfr's "≥ 2170 px" rule) and a `main` zone where every button is `Fill(Stretch)`.
//! - `MediaLayerDefault` picks which of the two is `base` and which is `fn`.
//! - Brightness and pixel shift go to `[display]`. Outlines and font go to the stylesheet.

use toml::{Table, Value};

use crate::keys::Key;

pub const SYSTEM_BASE: &str = "/usr/share/tiny-dfr/config.toml";
pub const SYSTEM_OVERLAY: &str = "/etc/tiny-dfr/config.toml";

/// The result of an import: files to write into `~/.config/sliver/`.
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    /// `config.toml` contents.
    pub config: String,
    /// `style.css` contents.
    pub stylesheet: String,
    /// Things that didn't translate exactly.
    pub warnings: Vec<String>,
}

/// Imports this machine's tiny-dfr config, or returns `Ok(None)` if tiny-dfr isn't installed.
pub fn import_system() -> Result<Option<Import>, String> {
    let base = match std::fs::read_to_string(SYSTEM_BASE) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{SYSTEM_BASE}: {e}")),
    };
    let overlay = std::fs::read_to_string(SYSTEM_OVERLAY).ok();
    import(&base, overlay.as_deref()).map(Some)
}

/// Translates tiny-dfr config text. `overlay` is the `/etc` file, if any.
pub fn import(base: &str, overlay: Option<&str>) -> Result<Import, String> {
    let mut dfr: Table = base
        .parse()
        .map_err(|e| format!("tiny-dfr base config: {e}"))?;
    if let Some(o) = overlay {
        let o: Table = o
            .parse()
            .map_err(|e| format!("tiny-dfr /etc config: {e}"))?;
        dfr.extend(o); // per top-level key, like tiny-dfr
    }
    let mut w = Vec::new();
    let flag = |k: &str, default: bool, w: &mut Vec<String>| match dfr.get(k) {
        None => default,
        Some(Value::Boolean(b)) => *b,
        Some(_) => {
            w.push(format!("{k} isn't true/false; using {default}"));
            default
        }
    };
    let media_default = flag("MediaLayerDefault", false, &mut w);
    let outlines = flag("ShowButtonOutlines", true, &mut w);
    let pixel_shift = flag("EnablePixelShift", false, &mut w);
    let adaptive = flag("AdaptiveBrightness", true, &mut w);
    let active = match dfr.get("ActiveBrightness") {
        None => 128,
        Some(Value::Integer(n)) if (0..=255).contains(n) => *n,
        Some(_) => {
            w.push("ActiveBrightness must be 0-255; using 128".into());
            128
        }
    };
    let font = dfr
        .get("FontTemplate")
        .and_then(Value::as_str)
        .unwrap_or(":bold")
        .to_owned();
    for k in dfr.keys() {
        if !KNOWN.contains(&k.as_str()) {
            w.push(format!("unknown tiny-dfr key {k} (ignored)"));
        }
    }

    let primary = layer(dfr.get("PrimaryLayerKeys"), "PrimaryLayerKeys", &mut w)?;
    let media = layer(dfr.get("MediaLayerKeys"), "MediaLayerKeys", &mut w)?;

    let (base_l, fn_l) = if media_default {
        ("tiny-dfr-media", "tiny-dfr")
    } else {
        ("tiny-dfr", "tiny-dfr-media")
    };
    let mut config = format!(
        "# Imported from tiny-dfr by sliver. Edit freely; see docs/customization.md.\n\
         # As in tiny-dfr, the bar shows {} and holding Fn shows {}.\n\
         version = 1\n\
         stylesheet = \"style.css\"\n\n\
         [display]\n\
         brightness = \"{}\"\n\
         max-brightness = {active}\n\
         pixel-shift = {pixel_shift}\n\n\
         [layers]\n\
         base = \"{base_l}\"\n\
         fn = \"{fn_l}\"\n",
        if media_default {
            "media keys"
        } else {
            "F-keys"
        },
        if media_default {
            "F-keys"
        } else {
            "media keys"
        },
        if adaptive { "adaptive" } else { "fixed" },
    );
    emit_layout(&mut config, "tiny-dfr", "PrimaryLayerKeys", &primary);
    emit_layout(&mut config, "tiny-dfr-media", "MediaLayerKeys", &media);
    Ok(Import {
        config,
        stylesheet: stylesheet(outlines, &font, &mut w),
        warnings: w,
    })
}

const KNOWN: &[&str] = &[
    "MediaLayerDefault",
    "ShowButtonOutlines",
    "EnablePixelShift",
    "FontTemplate",
    "AdaptiveBrightness",
    "ActiveBrightness",
    "PrimaryLayerKeys",
    "MediaLayerKeys",
];

fn layer(v: Option<&Value>, key: &str, w: &mut Vec<String>) -> Result<Vec<Value>, String> {
    let buttons = match v {
        Some(Value::Array(a)) if !a.is_empty() => a,
        Some(Value::Array(_)) => return Err(format!("{key} is empty (tiny-dfr refuses this too)")),
        Some(_) => return Err(format!("{key} must be an array of buttons")),
        None => return Err(format!("{key} is missing")),
    };
    let modules: Vec<Value> = buttons
        .iter()
        .enumerate()
        .map(|(i, b)| match b.as_table() {
            Some(t) => button(t, &format!("{key}[{i}]"), w),
            None => {
                w.push(format!("{key}[{i}] isn't a table; left as a spacer"));
                "spacer".into()
            }
        })
        .collect();
    Ok(modules)
}

/// Writes one layout by hand: `toml`'s serializer would turn the module list into
/// `[[...modules]]` sections, and this file is meant to be read and edited.
fn emit_layout(out: &mut String, name: &str, from: &str, modules: &[Value]) {
    out.push_str(&format!(
        "\n# tiny-dfr {from}\n\
         [[layouts.{name}.zones]]\n\
         name = \"esc\"\n\
         modules = [\"key/Esc#auto\"]   # only shown on Macs without a physical Esc, like tiny-dfr\n\n\
         [[layouts.{name}.zones]]\n\
         name = \"main\"\n\
         width = \"fill\"\n\
         module-width = \"fill\"\n\
         modules = [\n"
    ));
    for m in modules {
        out.push_str(&format!("    {},\n", inline(m)));
    }
    out.push_str("]\n");
}

/// A value as inline TOML, with `type` first in tables.
fn inline(v: &Value) -> String {
    match v {
        Value::Table(t) => {
            let mut parts: Vec<String> = t
                .get("type")
                .map(|ty| format!("type = {}", inline(ty)))
                .into_iter()
                .collect();
            parts.extend(
                t.iter()
                    .filter(|(k, _)| *k != "type")
                    .map(|(k, v)| format!("{} = {}", key_str(k), inline(v))),
            );
            format!("{{ {} }}", parts.join(", "))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(inline).collect::<Vec<_>>().join(", ")),
        scalar => scalar.to_string(),
    }
}

fn key_str(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_owned()
    } else {
        Value::String(k.to_owned()).to_string()
    }
}

fn button(t: &Table, path: &str, w: &mut Vec<String>) -> Value {
    const KEYS: &[&str] = &[
        "Text", "Icon", "Svg", "Theme", "Time", "Locale", "Battery", "Action", "Stretch",
    ];
    for k in t.keys() {
        if !KEYS.contains(&k.as_str()) {
            w.push(format!("{path}.{k}: unknown button key (ignored)"));
        }
    }
    let s = |k: &str| t.get(k).and_then(Value::as_str);
    let stretch = match t.get("Stretch") {
        None => 1,
        Some(Value::Integer(n)) if *n >= 1 => *n,
        Some(_) => {
            w.push(format!("{path}.Stretch must be >= 1; using 1"));
            1
        }
    };
    let mut m = Table::new();
    if stretch != 1 {
        let mut f = Table::new();
        f.insert("fill".into(), Value::Integer(stretch));
        m.insert("width".into(), Value::Table(f));
    }
    let icon = s("Icon").or_else(|| s("Svg"));
    let content = [
        s("Text").is_some(),
        icon.is_some(),
        t.contains_key("Time"),
        t.contains_key("Battery"),
    ];
    if content.iter().filter(|b| **b).count() > 1 {
        w.push(format!("{path}: more than one of Text/Icon/Time/Battery (undefined in tiny-dfr); using the first"));
    }

    if let Some(time) = t.get("Time") {
        m.insert("type".into(), "clock".into());
        let fmt = match time.as_str() {
            Some("24hr") => "{:%H:%M}".to_owned(),
            Some("12hr") => "{:%-I:%M %p}".to_owned(),
            Some(custom) => format!("{{:{custom}}}"),
            None => {
                w.push(format!("{path}.Time must be a string"));
                "{:%H:%M}".to_owned()
            }
        };
        m.insert("format".into(), fmt.into());
        if let Some(l) = s("Locale") {
            m.insert("locale".into(), l.into());
        }
        return Value::Table(m);
    }
    if let Some(b) = t.get("Battery") {
        m.insert("type".into(), "battery".into());
        let fmt = match b.as_str() {
            Some("percentage") => "{capacity}%",
            Some("icon") => "{icon}",
            Some("both") => "{icon} {capacity}%",
            _ => {
                w.push(format!(
                    "{path}.Battery must be percentage, icon or both; using both"
                ));
                "{icon} {capacity}%"
            }
        };
        m.insert("format".into(), fmt.into());
        m.insert("format-icons".into(), battery_icons());
        let mut states = Table::new();
        states.insert("critical".into(), Value::Integer(10));
        m.insert("states".into(), Value::Table(states));
        return Value::Table(m);
    }

    let keys: Option<Vec<Key>> = match t.get("Action") {
        None => None,
        Some(Value::String(k)) => key(k, path, w).map(|k| vec![k]),
        Some(Value::Array(a)) => {
            let ks: Vec<Key> = a
                .iter()
                .filter_map(|v| v.as_str().and_then(|k| key(k, path, w)))
                .collect();
            (ks.len() == a.len() && !ks.is_empty()).then_some(ks)
        }
        Some(_) => {
            w.push(format!(
                "{path}.Action must be a key name or an array of key names"
            ));
            None
        }
    };
    let text = s("Text");
    if text.is_none() && icon.is_none() {
        // A spacer: tiny-dfr ignores any action on it.
        if stretch == 1 {
            return "spacer".into();
        }
        m.insert("type".into(), "spacer".into());
        return Value::Table(m);
    }
    match keys.as_deref() {
        Some([k])
            if text.is_some_and(|t| t.eq_ignore_ascii_case(k.name()))
                && icon.is_none()
                && stretch == 1 =>
        {
            return format!("key/{}", k.name()).into();
        }
        Some([k]) => {
            m.insert("type".into(), format!("key/{}", k.name()).into());
        }
        Some(combo) => {
            m.insert("type".into(), "custom".into());
            let mut tap = Table::new();
            tap.insert(
                "keys".into(),
                Value::Array(combo.iter().map(|k| k.name().into()).collect()),
            );
            m.insert("on-tap".into(), Value::Table(tap));
        }
        None => {
            m.insert("type".into(), "custom".into());
        }
    }
    // Set both, so key/* defaults (an icon for media keys, a label for others) don't leak through.
    m.insert("format".into(), text.unwrap_or("").into());
    let icon_value = match (icon, s("Theme")) {
        (Some(i), Some(theme)) => {
            m.insert("icon-theme".into(), theme.into());
            i.to_owned()
        }
        (Some(i), None) => format!("tiny-dfr:{i}"),
        (None, _) => String::new(),
    };
    m.insert("icon".into(), icon_value.into());
    Value::Table(m)
}

fn key(name: &str, path: &str, w: &mut Vec<String>) -> Option<Key> {
    let k = Key::from_name(name);
    if k.is_none() {
        w.push(format!(
            "{path}.Action: unknown key {name:?}; the button won't press anything"
        ));
    }
    k
}

fn battery_icons() -> Value {
    let icons = |names: &[&str]| -> Value {
        Value::Array(
            names
                .iter()
                .map(|n| {
                    let mut t = Table::new();
                    t.insert("icon".into(), format!("tiny-dfr:{n}").into());
                    Value::Table(t)
                })
                .collect(),
        )
    };
    let mut t = Table::new();
    t.insert(
        "default".into(),
        icons(&[
            "battery_0_bar",
            "battery_1_bar",
            "battery_2_bar",
            "battery_3_bar",
            "battery_4_bar",
            "battery_5_bar",
            "battery_6_bar",
            "battery_full",
        ]),
    );
    t.insert(
        "charging".into(),
        icons(&[
            "battery_charging_20",
            "battery_charging_30",
            "battery_charging_50",
            "battery_charging_60",
            "battery_charging_80",
            "battery_charging_90",
            "battery_charging_full",
        ]),
    );
    Value::Table(t)
}

/// tiny-dfr's look as a stylesheet: its theme plus the user's font and outline choices.
fn stylesheet(outlines: bool, font: &str, w: &mut Vec<String>) -> String {
    let mut css = String::from(
        "/* Imported from tiny-dfr by sliver. */\n\
         @import \"themes/tiny-dfr.css\";\n",
    );
    let (family, style) = font.split_once(':').unwrap_or((font, ""));
    let mut rules = Vec::new();
    if !family.trim().is_empty() {
        rules.push(format!("font-family: \"{}\";", family.trim()));
    }
    for tok in style.split(':').filter(|t| !t.is_empty()) {
        match tok {
            "bold" => rules.push("font-weight: bold;".into()),
            "italic" | "oblique" => rules.push("font-style: italic;".into()),
            other => w.push(format!(
                "FontTemplate part {other:?} has no stylesheet equivalent (ignored)"
            )),
        }
    }
    if !rules.is_empty() {
        css.push_str(&format!("\n* {{ {} }}\n", rules.join(" ")));
    }
    if !outlines {
        css.push_str("\n/* ShowButtonOutlines = false */\n.module { border-width: 0; }\n");
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Context, Gesture, Hardware, Width, load_str};

    const DFR_DEFAULT: &str = include_str!("../tests/fixtures/tiny-dfr-0.3.7.toml");

    #[test]
    fn default_config_golden() {
        let imp = import(DFR_DEFAULT, None).unwrap();
        assert!(imp.warnings.is_empty(), "{:#?}", imp.warnings);
        let golden = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/tiny-dfr-0.3.7.sliver.toml"
        );
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(golden, &imp.config).unwrap();
        }
        assert_eq!(
            imp.config,
            std::fs::read_to_string(golden).unwrap(),
            "rerun with UPDATE_GOLDEN=1 if intended"
        );
        assert!(imp.stylesheet.contains("font-weight: bold;"));
        assert!(!imp.stylesheet.contains("border-width"));
    }

    #[test]
    fn default_config_behaves_like_tiny_dfr() {
        let imp = import(DFR_DEFAULT, None).unwrap();
        let l = load_str(&imp.config, std::path::Path::new("."));
        assert!(
            l.diagnostics.ok() && l.diagnostics.warnings.is_empty(),
            "{:#?}",
            l.diagnostics
        );
        for hw in [
            Hardware { physical_esc: true },
            Hardware {
                physical_esc: false,
            },
        ] {
            let d = l.config.validate(hw);
            assert!(d.ok() && d.warnings.is_empty(), "{hw:?} {d:#?}");
        }
        let here = Context::new().with("physical_esc", true);
        let base = l.config.resolve("base", &here).unwrap();
        assert_eq!(base.layout, "tiny-dfr");
        assert!(
            base.zones[0].modules.is_empty(),
            "2008 px bar: no Esc, like tiny-dfr"
        );
        let main = &base.zones[1].modules;
        assert_eq!(main.len(), 12);
        for (i, m) in main.iter().enumerate() {
            assert_eq!(m.common.width, Width::Fill(1));
            assert_eq!(
                m.common.on.get(&Gesture::Tap),
                Some(&Action::Key(Key::function(i as u8 + 1).unwrap()))
            );
        }
        let fnl = l.config.resolve("fn", &here).unwrap();
        assert_eq!(fnl.layout, "tiny-dfr-media");
        let first = &fnl.zones[1].modules[0];
        assert_eq!(
            first.common.icon.as_deref(),
            Some("tiny-dfr:brightness_low")
        );
        assert_eq!(
            first.common.on.get(&Gesture::Tap),
            Some(&Action::Key(Key::from_name("BrightnessDown").unwrap()))
        );

        let old_mac = Context::new().with("physical_esc", false);
        let base = l.config.resolve("base", &old_mac).unwrap();
        assert_eq!(
            base.zones[0].modules[0].id.to_string(),
            "key/Esc#auto",
            "2170 px bar: Esc first"
        );
    }

    #[test]
    fn overlay_and_every_button_kind() {
        let overlay = r#"
            MediaLayerDefault = true
            ShowButtonOutlines = false
            FontTemplate = "Inter:bold"
            AdaptiveBrightness = false
            ActiveBrightness = 200
            PrimaryLayerKeys = [
                { Text = "F1", Action = "F1", Stretch = 2 },
                { Text = "TTY1", Action = ["LeftCtrl", "LeftAlt", "F1"] },
                { Time = "12hr", Locale = "en_IN", Action = "Time", Stretch = 3 },
                { Battery = "both", Action = "Battery" },
                { Stretch = 3 },
                { Icon = "audio-volume-high", Theme = "breeze-dark", Action = "VolumeUp" },
                { Text = "Oops", Action = "NotAKey" },
            ]
        "#;
        let imp = import(DFR_DEFAULT, Some(overlay)).unwrap();
        assert_eq!(imp.warnings.len(), 1, "{:#?}", imp.warnings);
        assert!(imp.warnings[0].contains("NotAKey"));
        assert!(
            imp.stylesheet.contains("font-family: \"Inter\";")
                && imp.stylesheet.contains("border-width: 0")
        );
        let l = load_str(&imp.config, std::path::Path::new("."));
        assert!(l.diagnostics.ok(), "{:#?}\n{}", l.diagnostics, imp.config);
        let c = &l.config;
        assert_eq!(
            c.layers["base"], "tiny-dfr-media",
            "MediaLayerDefault swaps the layers"
        );
        assert!(!c.display.adaptive);
        assert_eq!(c.display.max_brightness, 200);
        let ctx = Context::new().with("physical_esc", true);
        let fnl = c.resolve("fn", &ctx).unwrap();
        let m = &fnl.zones[1].modules;
        assert_eq!(m.len(), 7);
        assert_eq!(
            (m[0].id.to_string().as_str(), m[0].common.width),
            ("key/F1#tiny-dfr-main-0", Width::Fill(2))
        );
        assert!(matches!(m[1].common.on.get(&Gesture::Tap), Some(Action::Keys(k)) if k.len() == 3));
        assert_eq!(m[1].common.format.as_deref(), Some("TTY1"));
        assert_eq!(
            (m[2].id.kind.as_str(), m[2].common.width),
            ("clock", Width::Fill(3))
        );
        assert_eq!(
            m[2].config.get("locale").and_then(Value::as_str),
            Some("en_IN")
        );
        assert_eq!(m[3].id.kind, "battery");
        assert_eq!(
            (m[4].id.kind.as_str(), m[4].common.width),
            ("spacer", Width::Fill(3))
        );
        assert_eq!(m[5].common.icon_theme.as_deref(), Some("breeze-dark"));
        assert_eq!(m[5].common.icon.as_deref(), Some("audio-volume-high"));
        assert!(m[6].common.on.is_empty(), "unknown key: no action");
        let d = c.validate(Hardware { physical_esc: true });
        assert!(d.ok(), "{d:#?}");
        assert!(
            d.warnings.iter().any(|w| w.message.contains("F2, F3")),
            "{d:#?}"
        );
    }

    #[test]
    fn broken_configs() {
        assert!(
            import(
                "PrimaryLayerKeys = []\nMediaLayerKeys = [{Text=\"a\", Action=\"A\"}]",
                None
            )
            .is_err()
        );
        assert!(import("[[[", None).is_err());
    }
}
