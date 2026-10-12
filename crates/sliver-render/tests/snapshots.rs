//! Snapshot tests: render known scenes with the embedded font and compare against committed PNGs
//! in `tests/snapshots/`. Look at the PNGs when you change them.
//!
//! - Update after an intended change: `SLIVER_BLESS=1 cargo test -p sliver-render --test snapshots`
//! - On mismatch the actual image is written to `target/snapshot-failures/<name>.png`.

use std::path::PathBuf;

use sliver_render::{
    Color, Content, Fonts, Icon, Item, Pixmap, Renderer, Scene, Stylesheet, Width,
};

const W: u32 = 2008;
const H: u32 = 60;

fn css(text: &str) -> Stylesheet {
    let (sheet, diags) = Stylesheet::parse("test.css", text, None);
    assert!(diags.is_empty(), "{diags:?}");
    sheet
}

fn icon(name: &str) -> Icon {
    Icon::builtin(name).unwrap()
}

fn fkeys() -> Vec<Item> {
    let mut items = vec![Item::text("esc").fill(1)];
    items.extend((1..=12).map(|n| Item::text(format!("F{n}")).fill(1)));
    items
}

/// Compares with a little tolerance so harmless rasterizer differences across CPUs don't fail CI.
fn check(name: &str, scene: &Scene, sheet: &Stylesheet) {
    let mut renderer = Renderer::new(Fonts::embedded());
    let actual = renderer.render_new(scene, sheet, W, H);
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
    check("primary", &Scene::new(fkeys()), &Stylesheet::classic());
}

#[test]
fn primary_layer_pressed_no_outlines() {
    let mut items = fkeys();
    items[5].pressed = true;
    // tiny-dfr's ShowButtonOutlines = false.
    let sheet = css("@import \"sliver:classic\"; module { background: transparent }");
    check("primary-pressed-no-outlines", &Scene::new(items), &sheet);
}

#[test]
fn media_layer() {
    let mut items = vec![Item::text("esc").fill(1)];
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
        .map(|n| Item::icon(icon(n)).fill(1)),
    );
    items[8].pressed = true;
    check("media", &Scene::new(items), &Stylesheet::classic());
}

#[test]
fn mixed_stretch_widgets_and_fitting() {
    let items = vec![
        Item::text("esc").fill(1),
        Item::spacer().fill(1),
        Item::new(Content::IconText(icon("battery_charging_80"), "83%".into()))
            .fill(2)
            .id("battery")
            .class("charging"),
        Item::text("12:34 Mon 12 Oct").fill(3),
        Item::text("A label far too long for one slot").fill(1),
        Item::new(Content::Blank).fill(1),
        Item::text("Ünïcødé €…").fill(3),
        Item::icon(icon("volume_up"))
            .fill(1)
            .foreground(Color::rgb(255, 200, 0)),
    ];
    check("mixed", &Scene::new(items), &Stylesheet::classic());
}

#[test]
fn zones_with_auto_and_fixed_widths() {
    let scene = Scene::zoned(
        vec![Item::text("esc")],
        vec![
            Item::icon(icon("fast_rewind")),
            Item::icon(icon("play_pause")),
            Item::icon(icon("fast_forward")),
            Item::text("Now Playing - Artist").width(Width::Fixed(400.0)),
        ],
        vec![
            Item::icon(icon("brightness_high")),
            Item::icon(icon("volume_down")),
            Item::icon(icon("volume_up")),
            Item::text("12:34"),
        ],
    );
    check("zones", &scene, &Stylesheet::classic());
}

#[test]
fn overflow_shrinks_and_ellipsizes() {
    let words = [
        "Terminal",
        "Files",
        "Firefox",
        "Settings",
        "Calendar",
        "Music",
        "Photos",
        "Mail",
        "Calculator",
        "Weather",
        "Notes",
        "Maps",
        "Clock",
        "Software",
    ];
    let mut items: Vec<Item> = words.iter().map(|w| Item::text(*w)).collect();
    items.push(Item::text("Fill").fill(1));
    check("overflow", &Scene::new(items), &Stylesheet::classic());
}

#[test]
fn waybar_style_stylesheet() {
    let sheet = css(r#"
        @define-color accent #3584e4;
        bar { background: #101014; color: #e8e8ea; font-size: 26px; font-weight: 600;
              -sliver-icon-size: 34px; padding: 4px 6px; gap: 24px; }
        zone { gap: 6px; }
        .zone-main { background: alpha(white, 0.06); border-radius: 12px; padding: 0 6px; }
        module { background: #2a2a30; border-radius: 10px; padding: 0 18px; min-width: 64px; }
        module:active { background: @accent; }
        #esc { background: transparent; border: 2px solid alpha(@accent, 0.8); }
        #battery.warning { color: #f5c211; }
        #battery.critical { background: #c01c28; }
        .muted { opacity: 0.4; }
        .zone-controls module:last-child { font-family: "Noto Sans"; font-weight: normal; }
        "#);
    let scene = Scene::zoned(
        vec![Item::text("esc").id("esc")],
        vec![
            Item::icon(icon("fast_rewind")),
            Item::icon(icon("play_pause")).pressed(true),
            Item::icon(icon("fast_forward")),
            Item::text("Song - Artist").width(Width::Fill(1)),
        ],
        vec![
            Item::new(Content::IconText(icon("battery_2_bar"), "18%".into()))
                .id("battery")
                .class("warning"),
            Item::icon(icon("volume_off")).class("muted"),
            Item::text("12:34").id("clock"),
        ],
    );
    check("stylesheet", &scene, &sheet);
}

fn theme_fkeys() -> Scene {
    let mut fkeys = fkeys();
    fkeys[3].pressed = true;
    Scene::new(fkeys)
}

fn theme_controls() -> Scene {
    Scene::zoned(
        vec![Item::text("esc").id("esc")],
        vec![
            Item::icon(icon("brightness_high")).class("on"),
            Item::new(Content::Slider {
                icon: Some(icon("brightness_low")),
                value: 0.6,
            })
            .id("backlight-slider"),
            Item::new(Content::Slider {
                icon: Some(icon("volume_up")),
                value: 0.25,
            })
            .id("audio-slider")
            .pressed(true),
        ],
        vec![
            Item::new(Content::IconText(icon("battery_1_bar"), "9%".into()))
                .id("battery")
                .class("critical"),
            Item::icon(icon("mic_off")).disabled(true),
            Item::text("12:34").id("clock"),
        ],
    )
}

#[test]
fn sliver_theme() {
    check("sliver-fkeys", &theme_fkeys(), &Stylesheet::sliver());
    check("sliver-controls", &theme_controls(), &Stylesheet::sliver());
}

#[test]
fn light_and_high_contrast_themes() {
    for name in ["light", "high-contrast"] {
        let (sheet, warning) = Stylesheet::theme(Some(name), None);
        assert_eq!(warning, None);
        check(&format!("{name}-fkeys"), &theme_fkeys(), &sheet);
        check(&format!("{name}-controls"), &theme_controls(), &sheet);
    }
    let (_, warning) = Stylesheet::theme(Some("nope"), None);
    assert!(
        warning
            .unwrap()
            .contains("sliver, light, high-contrast, classic")
    );
}

#[test]
fn slider_track_is_in_layout() {
    let scene = Scene::new(vec![
        Item::text("a"),
        Item::new(Content::Slider {
            icon: None,
            value: 0.5,
        }),
    ]);
    let mut r = Renderer::new(Fonts::embedded());
    let layout = r.layout(&scene, &Stylesheet::sliver(), W, H);
    assert_eq!(layout.tracks[0], None);
    let t = layout.tracks[1].expect("slider has a track");
    let item = layout.items[1];
    assert!(
        t.x >= item.x && t.right() <= item.right() && t.h == 8.0,
        "{t:?} in {item:?}"
    );
}

#[test]
fn empty_zones_take_no_space() {
    let scene = Scene::zoned(vec![], vec![Item::text("a").fill(1)], vec![]);
    let mut r = Renderer::new(Fonts::embedded());
    let layout = r.layout(&scene, &Stylesheet::sliver(), W, H);
    assert_eq!((layout.items[0].x, layout.items[0].w), (0.0, W as f32));
    assert_eq!(layout.zones[0].w, 0.0);
    // An empty Fill zone still pushes the zones after it to the right.
    let scene = Scene::zoned(vec![], vec![], vec![Item::text("b")]);
    let layout = r.layout(&scene, &Stylesheet::sliver(), W, H);
    assert_eq!(layout.items[0].right(), W as f32);
}

fn markup_items() -> Vec<Item> {
    vec![
        Item::text("plain").fill(1),
        Item::markup("<b>bold</b> <span foreground=\"#ff4040\">red</span>").fill(1),
        Item::markup("<u>under</u> <s>strike</s>").fill(1),
        Item::markup("<big>big</big> <small>small</small>").fill(1),
        Item::markup("<span foreground=\"#40ff80\" size=\"x-large\">&lt;5&gt;</span>").fill(1),
        Item::new(Content::IconMarkup(
            icon("play_pause"),
            "<i>song</i> <b>title that is much too long to fit</b>".into(),
        ))
        .fill(1),
    ]
}

#[test]
fn markup_labels() {
    check(
        "markup",
        &Scene::new(markup_items()),
        &Stylesheet::classic(),
    );
}

/// Pixel-level facts independent of the snapshot: colours apply, tags don't print, bold is wider.
#[test]
fn markup_semantics() {
    let render = |item: Item| {
        let mut r = Renderer::new(Fonts::embedded());
        r.render_new(
            &Scene::new(vec![item.fill(1)]),
            &Stylesheet::classic(),
            400,
            60,
        )
    };
    let ink = |p: &Pixmap| -> (usize, Vec<[u8; 3]>) {
        let px: Vec<[u8; 4]> = p
            .data()
            .chunks(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        let lit: Vec<[u8; 3]> = px
            .iter()
            .filter(|c| c[0] > 100 || c[1] > 100 || c[2] > 100)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        (lit.len(), lit)
    };
    let plain = render(Item::text("red"));
    let tagged = render(Item::markup("<span foreground=\"#ff0000\">red</span>"));
    let (_, plain_lit) = ink(&plain);
    let (n_tagged, tagged_lit) = ink(&tagged);
    assert!(n_tagged > 0);
    assert!(
        plain_lit.iter().any(|c| c[1] > 100),
        "classic text is white"
    );
    assert!(
        tagged_lit.iter().all(|c| c[1] < 100 && c[2] < 100),
        "markup colour reaches the glyphs: no green/blue ink"
    );
    // literal text: an escaped `<` shows, a real tag does not
    let lt = render(Item::markup("a&lt;b"));
    let literal = render(Item::text("a<b"));
    assert_eq!(lt.data(), literal.data(), "&lt; renders as the character");
    let tag_text = render(Item::markup("<i>a</i>"));
    let bare = render(Item::markup("a"));
    assert_eq!(ink(&tag_text).0 > 0, ink(&bare).0 > 0);
    // underline adds ink under the text
    let u = render(Item::markup("<u>aaaa</u>"));
    let nu = render(Item::markup("aaaa"));
    assert!(ink(&u).0 > ink(&nu).0, "an underline draws extra pixels");
}

/// COLRv1 emoji from a vendored subset of Noto Color Emoji (the renderer paints the paint graph
/// itself; swash can't).
fn emoji_fonts() -> Fonts {
    let mut fonts = Fonts::embedded();
    fonts
        .add_font_data(include_bytes!("../assets/fonts/NotoColorEmoji-COLRv1-subset.ttf").to_vec());
    fonts
}

fn emoji_scene() -> Scene {
    Scene::new(vec![
        Item::text("😀").fill(1),
        Item::text("❤️").fill(1),
        Item::text("🔥").fill(1),
        Item::text("🎵").fill(1),
        Item::text("👍").fill(1),
        Item::text("🌈").fill(1),
        Item::text("hi 😀 ok").fill(1),
        Item::markup("<b>🔥</b> <span foreground=\"#ff4040\">x</span>").fill(1),
    ])
}

#[test]
fn colr_emoji_snapshot() {
    let mut renderer = Renderer::new(emoji_fonts());
    let actual = renderer.render_new(&emoji_scene(), &Stylesheet::classic(), W, H);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("tests/snapshots/emoji.png");
    if std::env::var_os("SLIVER_BLESS").is_some() {
        actual.save_png(&path).unwrap();
        return;
    }
    let expected = Pixmap::load_png(&path).expect("run with SLIVER_BLESS=1");
    let mismatched = expected
        .data()
        .chunks(4)
        .zip(actual.data().chunks(4))
        .filter(|(e, a)| e.iter().zip(a.iter()).any(|(e, a)| e.abs_diff(*a) > 2))
        .count();
    assert!(
        mismatched <= 20,
        "{mismatched} pixels differ from emoji.png"
    );
}

/// The emoji are really coloured (not white glyph masks, not blank): count distinct hues.
#[test]
fn colr_emoji_has_colour() {
    let mut renderer = Renderer::new(emoji_fonts());
    let p = renderer.render_new(
        &Scene::new(vec![Item::text("🔥").fill(1)]),
        &Stylesheet::classic(),
        120,
        60,
    );
    let saturated = p
        .data()
        .chunks(4)
        .filter(|c| {
            let (mx, mn) = (c[..3].iter().max().unwrap(), c[..3].iter().min().unwrap());
            c[3] > 200 && mx - mn > 80
        })
        .count();
    assert!(
        saturated > 100,
        "{saturated} saturated pixels: the flame should be orange/red"
    );
}

#[test]
fn between_widths_and_zone_overflow() {
    let mut r = Renderer::new(Fonts::embedded());
    let long = "a label that is far wider than eighty pixels";
    let scene = Scene::new(vec![
        Item::text(long).width(Width::Between(0.0, 80.0)),
        Item::text("x").width(Width::Between(120.0, f32::INFINITY)),
        Item::text(long),
    ]);
    let layout = r.layout(&scene, &Stylesheet::sliver(), W, H);
    assert!(layout.items[0].w <= 80.0, "capped: {:?}", layout.items[0]);
    assert!(layout.items[1].w >= 120.0, "floored: {:?}", layout.items[1]);
    assert!(layout.items[2].w > 200.0, "free: {:?}", layout.items[2]);
    assert_eq!(layout.overflow, [false]);
    let crowded = Scene::new(
        (0..40)
            .map(|i| Item::text(format!("item {i} {long}")))
            .collect(),
    );
    assert_eq!(
        r.layout(&crowded, &Stylesheet::sliver(), W, H).overflow,
        [true]
    );
}

#[test]
fn extra_inset_keeps_content_clear_of_the_edges() {
    let scene = Scene::new(fkeys());
    let mut r = Renderer::new(Fonts::embedded());
    let plain = r.layout(&scene, &Stylesheet::sliver(), W, H);
    r.set_extra_inset([0.0, 11.0, 0.0, 11.0]);
    let inset = r.layout(&scene, &Stylesheet::sliver(), W, H);
    let (first, last) = (inset.items[0], *inset.items.last().unwrap());
    assert!(first.x >= plain.items[0].x + 11.0 - 0.5, "{first:?}");
    assert!(
        last.right() <= plain.items.last().unwrap().right() - 11.0 + 0.5,
        "{last:?}"
    );
    // The render uses the same geometry: nothing drawn in the 11 px edge strips.
    let mut px = Pixmap::new(W, H).unwrap();
    r.render(&scene, &Stylesheet::sliver(), &mut px);
    let bg = px.pixel(0, 0).unwrap();
    for x in (0..11).chain(W - 11..W) {
        for y in 0..H {
            assert_eq!(px.pixel(x, y).unwrap(), bg, "drawn at ({x}, {y})");
        }
    }
}

#[test]
fn indeterminate_progress_is_muted_in_every_theme() {
    // A full track with `.indeterminate` must not look like a full (100%) one.
    let track = |class: Option<&str>| {
        let mut it = Item::new(Content::Slider {
            icon: None,
            value: 1.0,
        });
        if let Some(c) = class {
            it = it.class(c);
        }
        Scene::new(vec![it])
    };
    for (name, _) in sliver_render::style::BUILTIN_THEMES {
        let sheet = Stylesheet::builtin(name).unwrap();
        let mut r = Renderer::new(Fonts::embedded());
        let full = r.render_new(&track(None), &sheet, W, H);
        let muted = r.render_new(&track(Some("indeterminate")), &sheet, W, H);
        assert_ne!(
            full.data(),
            muted.data(),
            "theme {name}: indeterminate looks full"
        );
    }
}
