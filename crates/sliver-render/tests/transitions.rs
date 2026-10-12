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
