//! Snapshot tests: render known scenes with the embedded font and compare against committed PNGs
//! in `tests/snapshots/`. Look at the PNGs when you change them.
//!
//! - Update after an intended change: `SLIVER_BLESS=1 cargo test -p sliver-render --test snapshots`
//! - On mismatch the actual image is written to `target/snapshot-failures/<name>.png`.

use std::path::PathBuf;

use sliver_render::{Color, Content, Fonts, Icon, Item, Pixmap, Renderer, Scene, Theme};

const W: u32 = 2008;
const H: u32 = 60;

fn icon(name: &str) -> Icon {
    Icon::builtin(name).unwrap()
}

fn fkeys() -> Vec<Item> {
    let mut items = vec![Item::text("esc")];
    items.extend((1..=12).map(|n| Item::text(format!("F{n}"))));
    items
}

/// Compares with a little tolerance so harmless rasterizer differences across CPUs don't fail CI.
fn check(name: &str, scene: &Scene, theme: &Theme) {
    let mut renderer = Renderer::new(Fonts::embedded());
    let actual = renderer.render_new(scene, theme, W, H);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let expected_path = root.join("tests/snapshots").join(format!("{name}.png"));
    if std::env::var_os("SLIVER_BLESS").is_some() {
        actual.save_png(&expected_path).unwrap();
        return;
    }
    let expected = Pixmap::load_png(&expected_path).unwrap_or_else(|e| {
        panic!("{name}: no snapshot at {expected_path:?} ({e}); run with SLIVER_BLESS=1")
    });
    let mismatched = if (expected.width(), expected.height()) != (W, H) {
        usize::MAX
    } else {
        expected
            .data()
            .chunks(4)
            .zip(actual.data().chunks(4))
            .filter(|(e, a)| e.iter().zip(a.iter()).any(|(e, a)| e.abs_diff(*a) > 2))
            .count()
    };
    if mismatched > 20 {
        let dir = root.join("../../target/snapshot-failures");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join(format!("{name}.png"));
        actual.save_png(&out).unwrap();
        panic!("{name}: {mismatched} pixels differ from {expected_path:?}; actual: {out:?}");
    }
}

#[test]
fn primary_layer() {
    check("primary", &Scene::new(fkeys()), &Theme::tiny_dfr());
}

#[test]
fn primary_layer_pressed_no_outlines() {
    let mut items = fkeys();
    items[5].pressed = true;
    let theme = Theme {
        outlines: false,
        ..Theme::tiny_dfr()
    };
    check("primary-pressed-no-outlines", &Scene::new(items), &theme);
}

#[test]
fn media_layer() {
    let mut items = vec![Item::text("esc")];
    items.extend(
        [
            "brightness_low",
            "brightness_high",
            "mic_off",
            "search",
            "backlight_low",
            "backlight_high",
            "fast_rewind",
            "play_pause",
            "fast_forward",
            "volume_off",
            "volume_down",
            "volume_up",
        ]
        .map(|n| Item::icon(icon(n))),
    );
    items[8].pressed = true;
    check("media", &Scene::new(items), &Theme::tiny_dfr());
}

#[test]
fn mixed_stretch_widgets_and_fitting() {
    let items = vec![
        Item::text("esc"),
        Item::spacer(1),
        Item::new(Content::IconText(icon("battery_charging_80"), "83%".into()))
            .stretch(2)
            .background(Color::rgb(0, 102, 0)),
        Item::text("12:34 Mon 12 Oct").stretch(3),
        Item::text("A label far too long for one slot"),
        Item::new(Content::Blank),
        Item::text("Ünïcødé €…").stretch(3),
        Item::icon(icon("volume_up")).foreground(Color::rgb(255, 200, 0)),
    ];
    check("mixed", &Scene::new(items), &Theme::tiny_dfr());
}
