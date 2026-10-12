use super::*;

const SAMPLE: &str = r##"
// typical waybar config
{
  "layer": "top",
  "position": "top",
  "height": 30,
  "spacing": 6,
  "name": "main",
  "modules-left": ["hyprland/workspaces", "hyprland/window", "cava"],
  "modules-center": ["clock"],
  "modules-right": ["pulseaudio", "custom/vpn", "battery#bat2", "tray", "mpd", "group/power"],
  "hyprland/workspaces": { "format": "{name}", "on-click": "activate", },
  "clock": { "format": "{:%H:%M}", "tooltip-format": "{:%A}", "on-click-middle": "foot", "interval": 30 },
  "pulseaudio": { "format": "{icon} {volume}%", "format-icons": ["a", "b", "c"], "on-click": "pavucontrol", "scroll-step": 5 },
  "custom/vpn": { "exec": "~/bin/vpn --json", "return-type": "json", "interval": 5, "signal": 8, "on-click": "toggle-vpn" },
  "battery#bat2": { "bat": "BAT1", "states": { "warning": 30, "critical": 15 } },
  "group/power": { "orientation": "inherit", "modules": ["custom/lock", "cffi/foo"] },
  "custom/lock": { "format": "L", "on-click": "swaylock" },
  "mpd": { "format": "x" },
  "rotate_nonsense": 5
}
"##;

fn import() -> Imported {
    import_str(
        SAMPLE,
        &Options {
            stylesheet: Some("style.css".into()),
        },
    )
    .unwrap()
}

#[test]
fn builds_a_layout_with_the_esc_zone_and_filtered_modules() {
    let i = import();
    let cfg = sliver_config::load_str(&i.toml, Path::new(".")).config;
    assert_eq!(cfg.layers["base"], "waybar");
    let r = cfg.resolve("base", &sliver_config::Context::new()).unwrap();
    assert_eq!(r.layout, "waybar");
    let names: Vec<&str> = r.zones.iter().map(|z| z.name.as_str()).collect();
    assert_eq!(names, ["esc", "left", "center", "right"]);
    let right: Vec<String> = r.zones[3]
        .modules
        .iter()
        .map(|m| m.id.to_string())
        .collect();
    assert_eq!(
        right,
        [
            "audio",
            "custom/vpn",
            "battery#bat2",
            "status-items",
            "group/power"
        ]
    );
    // the fn layer is still the built-in F-keys
    assert_eq!(cfg.layers["fn"], "fkeys");
    assert_eq!(
        i.dropped
            .iter()
            .filter(|d| *d == "cava" || *d == "mpd" || *d == "cffi/foo")
            .count(),
        3
    );
    assert!(i.toml.contains("stylesheet = \"style.css\""));
}

#[test]
fn custom_block_and_aliases_survive_verbatim() {
    let i = import();
    let cfg = sliver_config::load_str(&i.toml, Path::new(".")).config;
    let vpn = &cfg.modules[&ModuleId::parse("custom/vpn").unwrap()];
    assert_eq!(vpn["exec"].as_str(), Some("~/bin/vpn --json"));
    assert_eq!(vpn["return-type"].as_str(), Some("json"));
    assert_eq!(vpn["signal"].as_integer(), Some(8));
    let r = cfg.resolve("base", &sliver_config::Context::new()).unwrap();
    let inst = r.zones[3]
        .modules
        .iter()
        .find(|m| m.id.kind == "custom")
        .unwrap();
    assert!(
        inst.common.on.contains_key(&sliver_config::Gesture::Tap),
        "on-click became a tap action"
    );
    let group = r.zones[3]
        .modules
        .iter()
        .find(|m| m.id.kind == "group")
        .unwrap();
    assert_eq!(
        group.children.len(),
        1,
        "cffi/foo was removed from the group"
    );
}

#[test]
fn reports_what_does_not_apply() {
    let i = import();
    let has = |needle: &str| {
        i.notes
            .iter()
            .any(|n| format!("{} {}", n.path, n.message).contains(needle))
    };
    assert!(has("layer"), "{:#?}", i.notes);
    assert!(has("height"));
    assert!(has("cava"));
    assert!(has("cffi/foo"));
    assert!(has("mpd"));
    assert!(has("rotate_nonsense"));
    assert!(
        has("on-click-middle"),
        "sliver-config's own warning about mouse-only gestures: {:#?}",
        i.notes
    );
}

#[test]
fn multiple_bars_import_the_first() {
    let i = import_str(
        r#"[{"modules-left":["clock"]}, {"modules-left":["battery"]}]"#,
        &Options::default(),
    )
    .unwrap();
    assert_eq!(i.modules, ["clock"]);
    assert!(i.notes.iter().any(|n| n.message.contains("2 bars")));
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    assert!(import_str("{ nope", &Options::default()).is_err());
    assert!(import_str("[]", &Options::default()).is_err());
    assert!(import_str("42", &Options::default()).is_err());
}

#[test]
fn empty_bar_still_yields_a_valid_config_with_esc() {
    let i = import_str("{}", &Options::default()).unwrap();
    let cfg = sliver_config::load_str(&i.toml, Path::new(".")).config;
    let r = cfg.resolve("base", &sliver_config::Context::new()).unwrap();
    assert_eq!(r.zones.len(), 1);
    assert_eq!(r.zones[0].name, "esc");
}

#[test]
fn null_values_are_dropped_with_a_note() {
    let i = import_str(
        r#"{"modules-left":["clock"],"clock":{"format":null,"interval":5}}"#,
        &Options::default(),
    )
    .unwrap();
    assert!(i.notes.iter().any(|n| n.message.contains("null")));
    assert!(!i.toml.contains("format"));
}

#[test]
fn includes_are_followed_and_the_including_file_wins() {
    let d = std::env::temp_dir().join(format!("sliver-waybar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("main.jsonc"), r#"{"include":["mods.jsonc","missing.jsonc"],"modules-left":["clock"],"clock":{"format":"main"}}"#).unwrap();
    std::fs::write(
        d.join("mods.jsonc"),
        r#"{"modules-left":["battery"],"clock":{"format":"included"},"battery":{"format":"B"}}"#,
    )
    .unwrap();
    let i = import_path(&d.join("main.jsonc"), &Options::default()).unwrap();
    assert_eq!(i.modules, ["clock"], "main file's list wins");
    assert!(i.toml.contains("format = \"main\""));
    assert!(
        i.toml.contains("[modules.battery]"),
        "blocks only in the include are kept"
    );
    assert!(
        i.notes
            .iter()
            .any(|n| n.path == "include" && n.message.contains("missing.jsonc"))
    );
    let _ = std::fs::remove_dir_all(&d);
}
