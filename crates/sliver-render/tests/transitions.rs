//! `transition` through the real renderer: parse a stylesheet, change a module's state, and
//! check the pixels in between.

use sliver_render::{Fonts, Item, Pixmap, Renderer, Scene, Stylesheet, TransitionProp};
use std::time::{Duration, Instant};

fn sheet(css: &str) -> Stylesheet {
    let (s, d) = Stylesheet::parse("t.css", css, None);
    assert!(d.is_empty(), "{d:?}");
    s
}

fn scene(pressed: bool) -> Scene {
    Scene::new(vec![Item::text("x").id("btn").pressed(pressed).fill(1)])
}

/// Colour of a pixel inside the button but away from the glyph.
fn bg(p: &Pixmap) -> [u8; 3] {
    let d = p.data();
    let i = (5 * p.width() as usize + 8) * 4;
    [d[i], d[i + 1], d[i + 2]]
}

fn render(r: &mut Renderer, css: &Stylesheet, pressed: bool, t: Instant) -> Pixmap {
    let mut p = Pixmap::new(200, 60).unwrap();
    r.render_at(&scene(pressed), css, &mut p, t);
    p
}

const CSS: &str = "module { background-color: #000000; transition: background-color 200ms linear }
module:active { background-color: #ff0000 }";

#[test]
fn pressed_fades_in_and_the_bar_goes_idle() {
    let css = sheet(CSS);
    let mut r = Renderer::new(Fonts::embedded());
    let t0 = Instant::now();
    assert_eq!(bg(&render(&mut r, &css, false, t0)), [0, 0, 0]);
    assert!(
        r.animation_deadline().is_none(),
        "at rest: no frames needed"
    );

    let t1 = t0 + Duration::from_secs(1);
    let p = render(&mut r, &css, true, t1);
    assert_eq!(bg(&p), [0, 0, 0], "the press starts from the old colour");
    let next = r.animation_deadline().expect("animating");
    assert!(
        next > t1 && next <= t1 + Duration::from_millis(40),
        "next frame within ~30 fps"
    );

    let mid = bg(&render(&mut r, &css, true, t1 + Duration::from_millis(100)));
    assert!(
        (120..=136).contains(&mid[0]) && mid[1] == 0 && mid[2] == 0,
        "halfway to red: {mid:?}"
    );
    let end = bg(&render(&mut r, &css, true, t1 + Duration::from_millis(250)));
    assert_eq!(end, [255, 0, 0]);
    assert!(
        r.animation_deadline().is_none(),
        "settled: the loop can sleep again"
    );

    // and back, with a release transition
    let t2 = t1 + Duration::from_secs(1);
    render(&mut r, &css, false, t2);
    let back = bg(&render(
        &mut r,
        &css,
        false,
        t2 + Duration::from_millis(100),
    ));
    assert!((120..=136).contains(&back[0]), "{back:?}");
}

#[test]
fn without_transition_changes_are_instant() {
    let css =
        sheet("module { background-color: #000000 } module:active { background-color: #ff0000 }");
    let mut r = Renderer::new(Fonts::embedded());
    let t0 = Instant::now();
    render(&mut r, &css, false, t0);
    assert_eq!(
        bg(&render(&mut r, &css, true, t0 + Duration::from_millis(10))),
        [255, 0, 0]
    );
    assert!(r.animation_deadline().is_none());
}

#[test]
fn transition_property_lists_and_unknowns() {
    let css = sheet("module { transition: color 100ms, opacity 1s ease-in; }");
    let mut r = Renderer::new(Fonts::embedded());
    let mut p = Pixmap::new(100, 60).unwrap();
    r.render_at(&scene(false), &css, &mut p, Instant::now());
    // an unknown property name is a diagnostic, not a crash
    let (_, d) = Stylesheet::parse("t.css", "module { transition: sparkle 1s }", None);
    assert_eq!(d.len(), 1, "{d:?}");
    // GTK-isms that cannot animate parse silently
    let (_, d) = Stylesheet::parse(
        "t.css",
        "module { transition: min-width 200ms, transform 1s }",
        None,
    );
    assert!(d.is_empty(), "{d:?}");
    let _ = TransitionProp::All;
}

/// `transition: width`: when an item grows (a collapsed slider opening), its neighbour glides
/// over instead of jumping, the regions (the returned layout) are final at once, and frames
/// stop when it lands. Reduce-motion makes it jump.
#[test]
fn width_transition_glides_boxes() {
    const GLIDE: &str = "module { background-color: #000000; transition: width 200ms linear }
#b { background-color: #ff0000 }";
    let scene = |w: f32| {
        Scene::new(vec![
            Item::text("").id("a").width(sliver_render::Width::Fixed(w)),
            Item::text("")
                .id("b")
                .width(sliver_render::Width::Fixed(40.0)),
        ])
    };
    // Where `b`'s red starts on row 30.
    let red_x =
        |p: &Pixmap| (0..p.width()).find(|&x| p.data()[((30 * p.width() + x) * 4) as usize] > 200);
    let draw = |r: &mut Renderer, w: f32, t: Instant| {
        let mut p = Pixmap::new(400, 60).unwrap();
        let l = r.render_at(&scene(w), &sheet(GLIDE), &mut p, t);
        (p, l)
    };
    let mut r = Renderer::new(Fonts::embedded());
    let t0 = Instant::now();
    let (p, l0) = draw(&mut r, 50.0, t0);
    let start = red_x(&p).unwrap();
    assert!(r.animation_deadline().is_none());
    let (_, l1) = draw(&mut r, 150.0, t0 + Duration::from_secs(1));
    let end_layout = l1.items[1].x;
    assert!(
        end_layout > l0.items[1].x + 90.0,
        "layout is final immediately"
    );
    assert!(r.animation_deadline().is_some(), "gliding");
    let (p, _) = draw(&mut r, 150.0, t0 + Duration::from_millis(1100));
    let mid = red_x(&p).unwrap();
    assert!(
        mid > start + 30 && mid + 30 < end_layout as u32,
        "halfway: {start} < {mid} < {end_layout}"
    );
    let (p, _) = draw(&mut r, 150.0, t0 + Duration::from_millis(1250));
    assert_eq!(red_x(&p).unwrap(), end_layout.round() as u32);
    assert!(r.animation_deadline().is_none(), "landed: idle again");

    // Reduce motion: the same change jumps.
    r.set_reduce_motion(true);
    let (p, _) = draw(&mut r, 50.0, t0 + Duration::from_secs(3));
    assert_eq!(red_x(&p).unwrap(), start);
    assert!(r.animation_deadline().is_none());

    // `all` stays paint-only: a pasted `transition: all 0.3s` theme doesn't animate layout.
    let all = sheet("module { transition: all 300ms } #b { background-color: #ff0000 }");
    let mut r = Renderer::new(Fonts::embedded());
    let mut p = Pixmap::new(400, 60).unwrap();
    r.render_at(&scene(50.0), &all, &mut p, t0);
    r.render_at(&scene(150.0), &all, &mut p, t0 + Duration::from_secs(1));
    assert!(r.animation_deadline().is_none());
}

/// Layer changes: slide pushes the old image out on one side and the new in from the other
/// (direction by `dir`); fade mixes them.
#[test]
fn layer_change_composites() {
    use sliver_render::{LayerEffect, composite_layers};
    let solid = |r: u8, g: u8| {
        let mut p = Pixmap::new(100, 10).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(r, g, 0, 255));
        p
    };
    let px = |p: &Pixmap, x: usize| {
        let d = p.data();
        [d[(5 * 100 + x) * 4], d[(5 * 100 + x) * 4 + 1]]
    };
    let (old, new) = (solid(255, 0), solid(0, 255));
    // Forward, a quarter in: the new layer covers the right quarter.
    let mut to = new.clone();
    composite_layers(&old, &mut to, LayerEffect::Slide, 0.25, 1.0);
    assert_eq!((px(&to, 70), px(&to, 80)), ([255, 0], [0, 255]));
    // Back: from the left.
    let mut to = new.clone();
    composite_layers(&old, &mut to, LayerEffect::Slide, 0.25, -1.0);
    assert_eq!((px(&to, 20), px(&to, 30)), ([0, 255], [255, 0]));
    // Fade halfway.
    let mut to = new.clone();
    composite_layers(&old, &mut to, LayerEffect::Fade, 0.5, 1.0);
    let [r, g] = px(&to, 50);
    assert!(
        (120..=136).contains(&r) && (120..=136).contains(&g),
        "{r} {g}"
    );
}
