//! Untrusted input never panics the renderer. Script output and app-published labels arrive
//! as markup, and users write stylesheets; a panic in the agent would crash-loop it (the bar
//! falls back to sliverd's safe layer, but the user's bar is gone). Deterministic pseudo-random
//! inputs (fixed seeds) so a failure reproduces; the failing input is printed.

use sliver_render::{Content, Fonts, Item, Pixmap, Renderer, Scene, Stylesheet, markup};

/// xorshift64*: small, deterministic, good enough to shake parsers.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

/// Pieces that tend to break parsers and text layout.
const TRICKY: &[&str] = &[
    "<",
    ">",
    "&",
    ";",
    "&amp;",
    "&#",
    "&#x",
    "&#1114112;",
    "&#xD800;",
    "&lt",
    "\"",
    "'",
    "=",
    "<b>",
    "</b>",
    "<i>",
    "</span>",
    "<span ",
    "foreground=",
    "font_weight=\"bold\"",
    "size=\"999999\"",
    "size=\"-5\"",
    "bgcolor=\"#zzz\"",
    "color=\"#12345678\"",
    "<big>",
    "<sub>",
    "<tt>",
    "<u>",
    "<s>",
    "<small>",
    "<sup>",
    "</>",
    "<>",
    "< b>",
    "{",
    "}",
    "{}",
    "%",
    "\\",
    "\n",
    "\t",
    "\r",
    "\0",
    "\u{200d}",
    "\u{fe0f}",
    "\u{202e}",
    "\u{0301}",
    "\u{1f44d}\u{1f3fd}",
    "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
    "\u{fffd}",
    "\u{10ffff}",
    "שלום",
    "مرحبا",
    "漢字",
    "a",
    "Z",
    " ",
    "0",
    "-1",
    "1e309",
    "NaN",
    "999999999999999999999",
];

fn random_text(rng: &mut Rng, max_parts: usize) -> String {
    let n = rng.below(max_parts) + 1;
    (0..n).map(|_| rng.pick(TRICKY)).collect()
}

fn render(r: &mut Renderer, sheet: &Stylesheet, items: Vec<Item>) {
    let mut p = Pixmap::new(400, 60).unwrap();
    r.render(&Scene::new(items), sheet, &mut p);
}

#[test]
fn random_markup_and_text_never_panic() {
    let mut rng = Rng(0x5eed_0001);
    let mut r = Renderer::new(Fonts::embedded());
    let sheet = Stylesheet::sliver();
    for i in 0..4000 {
        let s = random_text(&mut rng, 12);
        let _ = markup::parse(&s);
        let _ = markup::escape(&s);
        // Rendering is the expensive part: every 20th input goes all the way to pixels.
        if i % 20 == 0 {
            let r2 = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render(
                    &mut r,
                    &sheet,
                    vec![
                        Item::new(Content::Markup(s.clone())).id("m"),
                        Item::new(Content::Text(s.clone())).id("t"),
                    ],
                )
            }));
            assert!(r2.is_ok(), "panicked on {s:?}");
        }
    }
    // Very long input: still fine (and clipped).
    let long = "<b>x</b>&amp;".repeat(5000);
    render(&mut r, &sheet, vec![Item::new(Content::Markup(long))]);
}

#[test]
fn mangled_stylesheets_never_panic() {
    let mut rng = Rng(0x5eed_0002);
    let mut r = Renderer::new(Fonts::embedded());
    let themes = [
        include_str!("../assets/themes/sliver.css"),
        include_str!("../assets/themes/classic.css"),
    ];
    let css_bits: &[&str] = &[
        "{",
        "}",
        ";",
        ":",
        "/*",
        "*/",
        "@define-color",
        "@import",
        "\"",
        "url(",
        ")",
        "(",
        "#",
        ".",
        ":active",
        ":checked",
        "module",
        "bar",
        "zone",
        ">",
        "*",
        "!important",
        "alpha(",
        "shade(",
        "mix(",
        "@accent",
        "px",
        "%",
        "em",
        "-1",
        "1e40",
        "transition:",
        "-sliver-layer-transition:",
        "min-width:",
        "padding:",
        "border:",
        "0",
        "rgba(1,2,3",
        "\u{0}",
        "\n",
    ];
    for i in 0..600 {
        let css = if i % 2 == 0 {
            // Random CSS-ish token soup.
            let n = rng.below(40) + 1;
            (0..n)
                .map(|_| rng.pick(css_bits))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            // A real theme with a few bytes cut, duplicated or replaced (on char boundaries).
            let mut chars: Vec<char> = themes[rng.below(2)].chars().collect();
            for _ in 0..rng.below(6) + 1 {
                let at = rng.below(chars.len());
                match rng.below(3) {
                    0 => {
                        chars.remove(at);
                    }
                    1 => chars.insert(at, chars[rng.below(chars.len())]),
                    _ => chars[at] = rng.pick(css_bits).chars().next().unwrap_or('x'),
                }
            }
            chars.into_iter().collect()
        };
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (sheet, _diags) = Stylesheet::parse("fuzz.css", &css, None);
            if i % 6 == 0 {
                render(
                    &mut r,
                    &sheet,
                    vec![
                        Item::text("x").id("a").pressed(true),
                        Item::new(Content::Slider {
                            icon: None,
                            value: 0.5,
                        }),
                    ],
                );
            }
        }));
        assert!(ok.is_ok(), "panicked on stylesheet:\n{css}");
    }
}

/// A glyph no font has makes the font system unload every font it loaded while searching
/// (memory, see `Fonts::trim_loaded_fonts`). Labels shaped before and after keep drawing exactly
/// as a fresh renderer draws them.
#[test]
fn unloading_fonts_after_a_missing_glyph_keeps_drawing_right() {
    let sheet = Stylesheet::sliver();
    let scene = |t: &str| {
        Scene::new(vec![
            Item::text("Hello").id("a"),
            Item::new(Content::Markup(t.into())).id("b"),
        ])
    };
    let draw = |r: &mut Renderer, t: &str| {
        let mut p = Pixmap::new(400, 60).unwrap();
        r.render(&scene(t), &sheet, &mut p);
        p
    };
    let mut fresh = Renderer::new(Fonts::embedded());
    let want = draw(&mut fresh, "F1");
    let mut r = Renderer::new(Fonts::embedded());
    draw(&mut r, "F1");
    // \u{10fffd} is in no font: shaping it triggers the unload.
    draw(&mut r, "x\u{10fffd}");
    draw(&mut r, "x\u{10fffd}"); // cached: no new search
    assert_eq!(draw(&mut r, "F1").data(), want.data());
}

/// A spectrum's bars grow out from the middle line; a silent one is a row of dots. Odd levels
/// (out of range, NaN, none, hundreds) never panic.
#[test]
fn spectrum_bars_grow_from_the_middle() {
    let mut r = Renderer::new(Fonts::embedded());
    let (sheet, _) = Stylesheet::parse(
        "t.css",
        "module { background: transparent; padding: 0; color: white; }",
        None,
    );
    let mut item = Item::new(Content::Bars(vec![0.0, 1.0]));
    item.width = sliver_render::Width::Fixed(100.0);
    let mut p = Pixmap::new(100, 60).unwrap();
    r.render(&Scene::new(vec![item]), &sheet, &mut p);
    let lit = |x: u32, y: u32| p.pixel(x, y).unwrap().red() > 128;
    assert!(lit(25, 30) && !lit(25, 10), "a silent bar is a dot");
    assert!(
        lit(75, 30) && lit(75, 5) && lit(75, 55),
        "a full bar spans the height"
    );
    for levels in [vec![], vec![f32::NAN, -1.0, 9.0], vec![0.5; 500]] {
        render(&mut r, &sheet, vec![Item::new(Content::Bars(levels))]);
    }
}

/// An item's spectrum is drawn under its content, in its `spectrum` node's colour (else the
/// item's): along a slider's rail with the rail and fill on top, or across a button's content
/// box with its icon or label on top.
#[test]
fn a_spectrum_sits_under_the_content() {
    let mut r = Renderer::new(Fonts::embedded());
    let (sheet, _) = Stylesheet::parse(
        "t.css",
        "module { background: transparent; padding: 0; color: #00ff00; } trough { background: #0000ff; min-width: 100px; min-height: 10px; } highlight { background: #ff0000; } #b spectrum { color: #ff00ff; }",
        None,
    );
    let mut slider = Item::new(Content::Slider {
        icon: None,
        value: 0.5,
    });
    slider.width = sliver_render::Width::Fixed(100.0);
    slider.spectrum = vec![1.0; 4];
    let mut p = Pixmap::new(100, 60).unwrap();
    r.render(&Scene::new(vec![slider]), &sheet, &mut p);
    let px = |p: &Pixmap, x: u32, y: u32| {
        let c = p.pixel(x, y).unwrap();
        (c.red(), c.green(), c.blue())
    };
    assert!(
        px(&p, 12, 8).1 > 128 && px(&p, 87, 8).1 > 128,
        "bars above the rail, in the item's colour"
    );
    assert!(px(&p, 12, 30).0 > 128, "the fill up to the value");
    assert!(px(&p, 87, 30).2 > 128, "the rail after it");

    let mut button = Item::text("").id("b");
    button.width = sliver_render::Width::Fixed(100.0);
    button.spectrum = vec![1.0, 0.0];
    let mut p = Pixmap::new(100, 60).unwrap();
    r.render(&Scene::new(vec![button]), &sheet, &mut p);
    assert_eq!(
        px(&p, 25, 8),
        (255, 0, 255),
        "a full bar, in the spectrum's colour"
    );
    assert_eq!(px(&p, 75, 8), (0, 0, 0), "a silent one is a dot");

    // The node's padding narrows the bars to a band: here the bottom third.
    let (sheet, _) = Stylesheet::parse(
        "t.css",
        "module { background: transparent; padding: 0; } spectrum { color: white; padding: 40px 0 0 0; }",
        None,
    );
    let mut button = Item::text("");
    button.width = sliver_render::Width::Fixed(100.0);
    button.spectrum = vec![1.0];
    let mut p = Pixmap::new(100, 60).unwrap();
    r.render(&Scene::new(vec![button]), &sheet, &mut p);
    assert_eq!(px(&p, 50, 30), (0, 0, 0), "nothing above the band");
    assert_eq!(px(&p, 50, 50), (255, 255, 255), "the band");
}
