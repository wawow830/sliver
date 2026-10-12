//! End-to-end: the agent core + link against sliverd's real core in-process (`TestDaemon`: real
//! socket, real memfds, sliverd's validation, touch routing and pressed feedback). PNG snapshots in `tests/snapshots/`;
//! bless with `SLIVER_BLESS=1`.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use chrono::{Local, TimeZone};
use sliver_agent::link::Link;
use sliver_agent::modules::World;
use sliver_agent::{Agent, icons::Icons};
use sliver_config::load_str;
use sliver_ipc::proto::{Layer, RegionKind};
use sliver_render::{Fonts, Pixmap, Renderer, Stylesheet};
use sliverd::testing::TestDaemon as FakeDaemon;

fn world(h: u32, m: u32) -> World {
    World::at(Local.with_ymd_and_hms(2026, 10, 12, h, m, 5).unwrap())
}

fn agent(toml: &str) -> Agent {
    let loaded = load_str(toml, std::path::Path::new("."));
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    Agent::new(
        loaded.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    )
}

/// Connects a link to a fresh FakeDaemon (the handshake blocks, so it runs on a thread while we
/// pump the fake).
fn connect() -> (FakeDaemon, Link) {
    let (mut fake, conn) = FakeDaemon::new(FakeDaemon::mac14_7()).unwrap();
    let t = thread::spawn(move || Link::handshake(conn, "test-agent"));
    while !t.is_finished() {
        fake.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let link = t.join().unwrap().unwrap();
    fake.pump().unwrap(); // the buffer attachments
    (fake, link)
}

/// Presents whatever the agent rebuilt and lets the fake process it.
fn present(a: &Agent, link: &mut Link, fake: &mut FakeDaemon, layers: &[Layer]) {
    for &l in layers {
        let f = a.frame(l).unwrap();
        link.present(l, &f.image, &f.pressed, &f.regions).unwrap();
    }
    fake.pump().unwrap();
    link.poll().unwrap();
}

fn check(name: &str, p: &Pixmap) {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/snapshots/{name}.png"));
    if std::env::var_os("SLIVER_BLESS").is_some() {
        p.save_png(&path).unwrap();
        return;
    }
    let want = Pixmap::load_png(&path)
        .unwrap_or_else(|e| panic!("{name}: {e}; bless with SLIVER_BLESS=1"));
    let bad = want
        .data()
        .chunks(4)
        .zip(p.data().chunks(4))
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 2))
        .count();
    if bad > 20 {
        let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/snapshot-failures/{name}.png"));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        p.save_png(&out).unwrap();
        panic!("{name}: {bad} pixels differ; actual at {out:?}");
    }
}

/// The fake's copy of a frame, as a Pixmap (XRGB little-endian → RGBA).
fn fake_pixels(px: &[u8]) -> Pixmap {
    let mut p = Pixmap::new(2008, 60).unwrap();
    for (d, s) in p.data_mut().chunks_exact_mut(4).zip(px.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], 255]);
    }
    p
}

#[test]
fn default_config_end_to_end() {
    let mut a = agent("");
    let (mut fake, mut link) = connect();
    let layers = a.render(&world(9, 41));
    assert_eq!(layers, [Layer::Base, Layer::Fn], "{:?}", a.errors);
    present(&a, &mut link, &mut fake, &layers);
    assert!(fake.errors.is_empty(), "{:?}", fake.errors);
    assert!(!fake.safe_layer_active());

    // What sliverd holds is exactly what we rendered.
    let base = fake.base.as_ref().expect("base presented");
    assert_eq!(
        fake_pixels(&base.pixels).data(),
        a.frame(Layer::Base).unwrap().image.data()
    );
    check("default-base", &a.frame(Layer::Base).unwrap().image);
    check("default-fn", &a.frame(Layer::Fn).unwrap().image);
    check("default-fn-pressed", &a.frame(Layer::Fn).unwrap().pressed);

    // Fn layer: F1..F12 as key regions, handled by sliverd alone.
    let fnl = fake.fn_layer.as_ref().expect("fn presented");
    let keys: Vec<u16> = fnl
        .regions
        .iter()
        .filter_map(|r| match &r.kind {
            RegionKind::Key { keys } => Some(keys[0]),
            _ => None,
        })
        .collect();
    assert_eq!(keys, (59..=68).chain([87, 88]).collect::<Vec<u16>>());

    // Hold Fn and tap F5: the fake emits KEY_F5 down/up without asking us.
    let f5 = fnl.regions[4].rect;
    fake.set_fn(true).unwrap();
    fake.tap((f5.x + f5.w / 2) as f32, 30.0).unwrap();
    assert_eq!(fake.keys, [(63, true), (63, false)]);

    // Re-rendering the same state sends nothing (invariant 3).
    let before = link.presents;
    let layers = a.render(&world(9, 41));
    present(&a, &mut link, &mut fake, &layers);
    assert_eq!(link.presents, before);
}

#[test]
fn clock_ticks_present_only_changes() {
    let mut a = agent(
        r#"
        [[layouts.default.zones]]
        name = "main"
        width = "fill"
        modules = ["key/Esc", "spacer", "clock"]
        [modules.clock]
        format = "{:%H:%M}"
        "#,
    );
    let (mut fake, mut link) = connect();
    let layers = a.render(&world(9, 41));
    present(&a, &mut link, &mut fake, &layers);
    check("clock-0941", &a.frame(Layer::Base).unwrap().image);
    assert_eq!(
        a.next_change().unwrap().format("%H:%M:%S").to_string(),
        "09:42:00"
    );

    let before = link.presents;
    let layers = a.render(&world(9, 42));
    present(&a, &mut link, &mut fake, &layers);
    assert_eq!(link.presents, before + 1, "only base changed");
    let base = fake.base.as_ref().unwrap();
    assert_eq!(
        fake_pixels(&base.pixels).data(),
        a.frame(Layer::Base).unwrap().image.data()
    );

    // Display off: nothing ticks.
    a.display = sliver_ipc::proto::DisplayState::Off;
    assert_eq!(a.next_change(), None);
}

const TOUCHY: &str = r#"
[layers]
base = "main"

[[layouts.main.zones]]
name = "main"
width = "fill"
modules = ["custom/hello", "custom/media", "group/tools", "custom/vol"]

[modules."custom/hello"]
format = "Hello"
on-tap = "echo hi"
on-hold = { key = "F5" }

[modules."custom/media"]
format = "Media"
on-tap = { layer = "media", mode = "toggle" }

[modules."group/tools"]
modules = ["custom/more", "custom/a"]
drawer = { reveal = "tap", collapse-after = "5s" }

[modules."custom/more"]
format = "‹"

[modules."custom/a"]
format = "A"

[modules."custom/vol"]
format = "Vol"
width = 300
on-drag = "set-volume {value}"
"#;

/// Region id and centre x of the cell whose module key is `key`, on the base frame.
fn region(a: &Agent, key: &str) -> (u32, f32) {
    let f = a.frame(Layer::Base).unwrap();
    let i = f
        .cells
        .iter()
        .position(|c| c.key == key)
        .unwrap_or_else(|| panic!("no cell {key}"));
    let r = f
        .regions
        .iter()
        .find(|r| r.id == i as u32 + 1)
        .expect("touchable");
    (r.id, (r.rect.x + r.rect.w / 2) as f32)
}

/// Delivers what sliverd forwarded to the agent; returns the effects.
/// Renders at the agent's animation deadlines until it's at rest, like the real loop. Returns
/// the base images drawn, each with when it was drawn.
fn settle(a: &mut Agent, w: &World) -> Vec<(std::time::Instant, Pixmap)> {
    let mut images = Vec::new();
    while let Some(due) = a.animation_deadline() {
        std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
        let t = std::time::Instant::now();
        a.render(w);
        images.push((t, a.frame(Layer::Base).unwrap().image.clone()));
        assert!(images.len() < 30, "animations end");
    }
    images
}

fn deliver(
    a: &mut Agent,
    link: &mut Link,
    fake: &mut FakeDaemon,
) -> Vec<sliver_agent::agent::Effect> {
    fake.pump().unwrap();
    let mut fx = Vec::new();
    for ev in link.poll().unwrap() {
        if let sliver_agent::link::Event::Touch {
            region,
            slot,
            phase,
            x,
            time_us,
            ..
        } = ev
        {
            fx.extend(a.touch(region, slot, phase, x, time_us));
        }
    }
    fx
}

#[test]
fn touches_become_actions() {
    use sliver_agent::agent::Effect::*;
    use sliver_ipc::proto::{KeyMode, TouchPhase};
    let mut a = agent(TOUCHY);
    let (mut fake, mut link) = connect();
    let layers = a.render(&world(9, 41));
    present(&a, &mut link, &mut fake, &layers);
    assert!(fake.errors.is_empty(), "{:?}", fake.errors);

    // Tap → shell command.
    let (_, x) = region(&a, "custom/hello");
    fake.tap(x, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(
        fx.contains(&Spawn(vec!["sh".into(), "-c".into(), "echo hi".into()])),
        "{fx:?}"
    );

    // Hold → key through sliverd, and pressed feedback is drawn by us.
    let idle = a.frame(Layer::Base).unwrap().image.clone();
    fake.touch(0, TouchPhase::Down, x, 30.0).unwrap();
    assert_eq!(deliver(&mut a, &mut link, &mut fake), [RedrawPress]);
    // The fast path re-renders just the base image…
    let layers = a.render_press();
    assert_eq!(layers, [Layer::Base]);
    let fast = a.frame(Layer::Base).unwrap().image.clone();
    // …and matches a full render.
    a.render(&world(9, 41));
    assert_eq!(
        fast.data(),
        a.frame(Layer::Base).unwrap().image.data(),
        "fast = full"
    );
    assert_ne!(
        a.frame(Layer::Base).unwrap().image.data(),
        idle.data(),
        "pressed feedback"
    );
    check("touchy-hello-pressed", &a.frame(Layer::Base).unwrap().image);
    present(&a, &mut link, &mut fake, &layers);
    let t = a.next_deadline().expect("hold timer");
    // The key, and the user module re-runs after its action (waybar exec-on-event).
    assert_eq!(
        a.tick(t),
        [
            EmitKeys(vec![63], KeyMode::Tap),
            ModuleFired("custom/hello".into())
        ]
    );
    fake.touch(0, TouchPhase::Up, x, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(
        !fx.iter().any(|e| matches!(e, Spawn(_))),
        "a hold isn't also a tap: {fx:?}"
    );

    // Drawer: tapping the handle opens the group, and it closes by itself after 5 s.
    let (_, x) = region(&a, "group/tools/custom/more");
    fake.tap(x, 30.0).unwrap();
    assert!(deliver(&mut a, &mut link, &mut fake).contains(&Redraw));
    a.render(&world(9, 41));
    assert!(
        a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .any(|c| c.key == "group/tools/custom/a")
    );
    let close = a.next_deadline().expect("collapse timer");
    assert_eq!(a.tick(close), [Redraw]);
    a.render(&world(9, 41));
    assert!(
        !a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .any(|c| c.key == "group/tools/custom/a")
    );

    // Dragging the 'Vol' module runs on-drag, throttled, with the final value.
    let (_, x) = region(&a, "custom/vol");
    let layers = a.render(&world(9, 41));
    present(&a, &mut link, &mut fake, &layers);
    fake.touch(1, TouchPhase::Down, x - 100.0, 30.0).unwrap();
    fake.touch(1, TouchPhase::Move, x, 30.0).unwrap();
    fake.touch(1, TouchPhase::Up, x + 50.0, 30.0).unwrap();
    let spawns: Vec<String> = deliver(&mut a, &mut link, &mut fake)
        .into_iter()
        .filter_map(|e| match e {
            Spawn(v) => Some(v[2].clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        spawns,
        [
            format!("set-volume {}", x - 100.0),
            format!("set-volume {}", x + 50.0)
        ]
    );

    // Toggle a custom layer: the media keys replace the base frame, as key regions.
    let (_, x) = region(&a, "custom/media");
    fake.tap(x, 30.0).unwrap();
    assert!(deliver(&mut a, &mut link, &mut fake).contains(&Redraw));
    assert_eq!(a.base_layer(), "media");
    let before = a.frame(Layer::Base).unwrap().image.clone();
    let start = std::time::Instant::now();
    let layers = a.render(&world(9, 41));
    present(&a, &mut link, &mut fake, &layers);
    // The theme slides the media layer in: the first frame still shows the old layer, the
    // middle one a mix, and the loop goes idle once it's in.
    assert_eq!(a.frame(Layer::Base).unwrap().image.data(), before.data());
    let frames = settle(&mut a, &world(9, 41));
    assert!(!frames.is_empty(), "slid");
    let settled = a.frame(Layer::Base).unwrap().image.clone();
    // A frame drawn well inside the 200 ms slide is a mix (a loaded machine may draw none
    // that early; then there's nothing to check).
    for (t, img) in &frames {
        if t.duration_since(start) < std::time::Duration::from_millis(120) {
            assert!(
                img.data() != before.data() && img.data() != settled.data(),
                "in between"
            );
        }
    }
    check("touchy-media-layer", &settled);
    let keys = fake
        .base
        .as_ref()
        .unwrap()
        .regions
        .iter()
        .filter(|r| matches!(r.kind, RegionKind::Key { .. }))
        .count();
    assert_eq!(keys, 12);
}

fn session_world() -> World {
    use sliver_session::{BatteryState, Media, PlaybackStatus};
    let mut w = world(9, 41);
    let s = &mut w.session;
    s.power.on_battery = Some(true);
    s.power.battery_pct = Some(18);
    s.power.battery_state = BatteryState::Discharging;
    s.audio.volume_pct = Some(40);
    s.audio.muted = Some(false);
    s.brightness.screen_pct = Some(70);
    s.brightness.keyboard_pct = Some(30);
    s.caffeine = true;
    s.media = Some(Media {
        player: "org.mpris.MediaPlayer2.test".into(),
        identity: "Test <Player>".into(),
        status: PlaybackStatus::Playing,
        title: "Song & Dance".into(),
        artist: "Band".into(),
        ..Media::default()
    });
    w
}

#[test]
fn session_modules() {
    use sliver_agent::agent::{Effect::*, SessionOp};
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        r#"
        [[layouts.default.zones]]
        name = "main"
        width = "fill"
        modules = ["mpris", "idle-inhibitor"]
        [[layouts.default.zones]]
        name = "controls"
        modules = ["battery", "backlight/slider", "audio/slider", "audio"]
        [modules.battery]
        states = { warning = 30, critical = 10 }
        format = "{capacity}%"
        [modules."audio/slider"]
        collapsed = false
        "#,
    );
    let (mut fake, mut link) = connect();
    let w = session_world();
    let layers = a.render(&w);
    present(&a, &mut link, &mut fake, &layers);
    assert!(fake.errors.is_empty(), "{:?}", fake.errors);
    check("session-modules", &a.frame(Layer::Base).unwrap().image);
    let cells = &a.frame(Layer::Base).unwrap().cells;
    let battery = cells.iter().find(|c| c.kind == "battery").unwrap();
    assert!(
        battery.classes.contains(&"warning".to_string()),
        "{:?}",
        battery.classes
    );
    let mpris = cells.iter().find(|c| c.kind == "mpris").unwrap();
    assert!(
        matches!(&mpris.content, sliver_render::Content::Markup(t) if t == "⏸ Song &amp; Dance"),
        "{:?}",
        mpris.content
    );
    assert!(
        cells
            .iter()
            .find(|c| c.kind == "idle-inhibitor")
            .unwrap()
            .checked
    );

    // Tap mpris → play/pause; the collapsed brightness slider opens on tap.
    let (_, x) = region(&a, "mpris");
    fake.tap(x, 30.0).unwrap();
    assert!(
        deliver(&mut a, &mut link, &mut fake).contains(&Session(SessionOp::Media(
            sliver_session::mpris::Command::PlayPause
        )))
    );
    let (_, x) = region(&a, "backlight/slider");
    fake.tap(x, 30.0).unwrap();
    assert!(deliver(&mut a, &mut link, &mut fake).contains(&Redraw));
    let layers = a.render(&w);
    present(&a, &mut link, &mut fake, &layers);
    // It glides open (the theme's `transition: width`): frames until it lands, then none.
    let frames = settle(&mut a, &w).len();
    assert!(frames >= 1, "it animated: {frames} frames");
    check("session-slider-open", &a.frame(Layer::Base).unwrap().image);

    // Dragging the open volume slider sets the volume along its trough.
    let f = a.frame(Layer::Base).unwrap();
    let i = f
        .cells
        .iter()
        .position(|c| c.kind == "audio/slider")
        .unwrap();
    let t = f.layout.tracks[i].expect("slider track");
    fake.touch(2, TouchPhase::Down, t.x + t.w * 0.75, 30.0)
        .unwrap();
    fake.touch(2, TouchPhase::Up, t.x + t.w * 0.75, 30.0)
        .unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(
        fx.contains(&Session(SessionOp::Volume {
            mic: false,
            pct: 75
        })),
        "{fx:?}"
    );
}

/// `[gestures]`: a two-finger swipe and an edge swipe run their actions, and the modules under
/// the fingers don't fire.
#[test]
fn bar_gestures() {
    use sliver_agent::agent::Effect::*;
    use sliver_ipc::proto::{KeyMode, TouchPhase::*};
    let mut a = agent(
        r#"
        [[layouts.default.zones]]
        name = "main"
        width = "fill"
        modules = ["custom/a", "custom/b"]
        [modules."custom/a"]
        format = "A"
        width = "fill"
        on-tap = "echo a"
        [modules."custom/b"]
        format = "B"
        width = "fill"
        on-tap = "echo b"
        [gestures]
        two-finger-swipe-right = { layer = "media" }
        edge-swipe-from-left = { keys = ["LeftCtrl", "c"] }
        "#,
    );
    let (mut fake, mut link) = connect();
    let layers = a.render(&world(9, 41));
    present(&a, &mut link, &mut fake, &layers);
    // (A plain tap does spawn, so "no Spawn" below means no tap.)
    fake.tap(1400.0, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(fx.iter().any(|e| matches!(e, Spawn(_))), "{fx:?}");

    // Edge swipe from the left end: the keys, and no tap on custom/a.
    fake.touch(0, Down, 10.0, 30.0).unwrap();
    fake.touch(0, Move, 60.0, 30.0).unwrap();
    fake.touch(0, Move, 120.0, 30.0).unwrap();
    fake.touch(0, Up, 120.0, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(fx.contains(&EmitKeys(vec![29, 46], KeyMode::Tap)), "{fx:?}");
    assert!(!fx.iter().any(|e| matches!(e, Spawn(_))), "no tap: {fx:?}");

    // Two fingers, one on each module, both to the right: the media layer; no taps.
    fake.touch(0, Down, 400.0, 30.0).unwrap();
    fake.touch(1, Down, 1400.0, 30.0).unwrap();
    for dx in [30.0, 70.0, 90.0] {
        fake.touch(0, Move, 400.0 + dx, 30.0).unwrap();
        fake.touch(1, Move, 1400.0 + dx, 30.0).unwrap();
    }
    fake.touch(0, Up, 490.0, 30.0).unwrap();
    fake.touch(1, Up, 1490.0, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert_eq!(a.base_layer(), "media", "{fx:?}");
    assert!(!fx.iter().any(|e| matches!(e, Spawn(_))), "no taps: {fx:?}");
}

/// Touch a collapsed slider's icon and slide without lifting: it opens under the finger and
/// the value moves by how far the finger slid (never jumping to where the finger landed).
#[test]
fn press_and_slide_a_collapsed_slider() {
    use sliver_agent::agent::{Effect::*, SessionOp};
    use sliver_ipc::proto::TouchPhase::*;
    let mut a = agent(
        r#"
        [[layouts.default.zones]]
        name = "controls"
        modules = ["backlight/slider"]
        "#,
    );
    let (mut fake, mut link) = connect();
    let w = session_world(); // screen brightness 70 %
    let layers = a.render(&w);
    present(&a, &mut link, &mut fake, &layers);
    let (id, x) = region(&a, "backlight/slider");
    fake.touch(0, Down, x, 30.0).unwrap();
    assert!(
        deliver(&mut a, &mut link, &mut fake).contains(&Redraw),
        "opens on press"
    );
    let layers = a.render(&w);
    present(&a, &mut link, &mut fake, &layers);
    let f = a.frame(Layer::Base).unwrap();
    let i = f.cell_index(id).expect("same region once open");
    let t = f.layout.tracks[i].expect("open slider has a trough");
    // Slide right by a fifth of the trough: past the slop it starts, anchored at 70 %.
    fake.touch(0, Move, x + 20.0, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(
        fx.contains(&Session(SessionOp::ScreenBrightness(70))),
        "{fx:?}"
    );
    fake.touch(0, Up, x + 20.0 + t.w * 0.2, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(
        fx.contains(&Session(SessionOp::ScreenBrightness(90))),
        "{fx:?}"
    );
    // Still open after lifting (it closes on its own later), and a fresh drag on the open
    // slider is absolute again.
    assert!(a.frame(Layer::Base).unwrap().layout.tracks[i].is_some());
    fake.touch(1, Down, t.x + t.w * 0.25, 30.0).unwrap();
    fake.touch(1, Up, t.x + t.w * 0.25, 30.0).unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    assert!(
        fx.contains(&Session(SessionOp::ScreenBrightness(25))),
        "{fx:?}"
    );
}

#[test]
fn preview_of_the_default_bar() {
    let mut a = agent("");
    a.render(&World::sample(
        Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 5).unwrap(),
    ));
    check("preview-default", &a.frame(Layer::Base).unwrap().image);
}

/// A `transition` in the stylesheet animates a press: the agent asks to be woken for the next
/// frame while it runs, and not at all once it has settled (no busy loop).
#[test]
fn transitions_wake_the_loop_only_while_animating() {
    use sliver_ipc::proto::TouchPhase;
    use std::time::Duration;
    let loaded = load_str(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"custom/b\"]\n[modules.\"custom/b\"]\nformat = \"B\"\non-tap = \"true\"\n",
        std::path::Path::new("."),
    );
    assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
    let mut sheet = Stylesheet::sliver();
    let mut diags = Vec::new();
    sheet.add_str(
        "t.css",
        "module { transition: background-color 150ms linear }",
        None,
        &mut diags,
    );
    assert!(diags.is_empty(), "{diags:?}");
    let mut a = Agent::new(
        loaded.config,
        sheet,
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    );
    let w = world(9, 41);
    a.render(&w);
    assert!(
        a.animation_deadline().is_none(),
        "first frame never animates"
    );
    let (id, x) = region(&a, "custom/b");
    a.touch(id, 0, TouchPhase::Down, x, 1_000);
    a.render(&w);
    // The sliver theme shows presses at once (`module:active { transition: none }`)…
    assert!(a.animation_deadline().is_none(), "the press is instant");
    // …and fades the release.
    a.touch(id, 0, TouchPhase::Up, x, 50_000);
    a.render(&w);
    assert!(a.animation_deadline().is_some(), "the release fades out");
    std::thread::sleep(Duration::from_millis(250));
    a.render(&w);
    assert!(
        a.animation_deadline().is_none(),
        "settled: the loop can sleep"
    );
}

#[test]
fn runtime_layout_visibility_and_pushed_output() {
    let mut a = agent(
        r#"
        [[layouts.default.zones]]
        name = "main"
        modules = ["custom/push", "clock"]
        [modules."custom/push"]
        format = "[{}]"
        "#,
    );
    let w = world(9, 41);
    a.pushed.insert(
        "custom/push".into(),
        sliver_modules::Output {
            text: "pushed".into(),
            ..Default::default()
        },
    );
    a.render(&w);
    let texts: Vec<String> = a
        .frame(Layer::Base)
        .unwrap()
        .cells
        .iter()
        .map(|c| format!("{:?}", c.content))
        .collect();
    assert!(texts[0].contains("[pushed]"), "{texts:?}");

    a.hidden_modules.insert("clock".into());
    a.render(&w);
    assert_eq!(a.frame(Layer::Base).unwrap().cells.len(), 1);

    assert!(a.set_layout("nope").is_err());
    a.set_layout("fkeys").unwrap();
    a.render(&w);
    assert_eq!(
        a.frame(Layer::Base).unwrap().cells.len(),
        12,
        "fkeys on base"
    );
    a.set_layout("").unwrap();
    a.render(&w);
    assert_eq!(a.frame(Layer::Base).unwrap().cells.len(), 1);
}

/// Tapping a workspace button asks the compositor to switch; holding a taskbar item closes it.
#[test]
fn workspaces_and_taskbar_buttons_drive_the_compositor() {
    use sliver_agent::agent::Effect;
    use sliver_desktop::control::Action as Desk;
    use sliver_desktop::{WindowInfo, WorkspaceInfo};
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"workspaces\", \"taskbar\"]\n",
    );
    let mut w = world(9, 41);
    w.desktop.connected = true;
    w.desktop.workspaces = vec![WorkspaceInfo {
        id: 3,
        name: "3".into(),
        active: true,
        windows: 1,
        ..Default::default()
    }];
    w.desktop.workspace = Some(sliver_desktop::Workspace {
        id: 3,
        name: "3".into(),
    });
    w.desktop.windows = vec![WindowInfo {
        address: "ab12".into(),
        class: "kitty".into(),
        title: "sh".into(),
        workspace: 3,
        focused: true,
        ..Default::default()
    }];
    a.render(&w);
    let (id, x) = region(&a, "workspaces/3");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 1_000);
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 40_000));
    fx.extend(a.tick(400_000));
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Desktop(Desk::FocusWorkspace(ws)) if ws.id == 3)),
        "{fx:?}"
    );
    // hold on the taskbar item
    let (id, x) = region(&a, "taskbar/ab12");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 500_000);
    fx.extend(a.tick(2_000_000));
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 2_100_000));
    assert!(
        fx.iter().any(
            |e| matches!(e, Effect::Desktop(Desk::CloseWindow { address }) if address == "ab12")
        ),
        "{fx:?}"
    );
}

/// The timer module: tap starts, tap pauses, hold resets, and reaching zero notifies once.
#[test]
fn timer_counts_down_and_notifies() {
    use sliver_agent::agent::Effect;
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"timer\"]\n[modules.timer]\nduration = \"1s\"\n",
    );
    let w = world(9, 41);
    let text = |a: &Agent| match &a.frame(Layer::Base).unwrap().cells[0].content {
        sliver_render::Content::Markup(t) | sliver_render::Content::Text(t) => t.clone(),
        other => format!("{other:?}"),
    };
    a.render(&w);
    assert_eq!(text(&a), "00:01");
    let (id, x) = region(&a, "timer");
    let tap = |a: &mut Agent, t: u64| {
        let mut fx = a.touch(id, 0, TouchPhase::Down, x, t);
        fx.extend(a.touch(id, 0, TouchPhase::Up, x, t + 40_000));
        fx.extend(a.tick(t + 400_000));
        fx
    };
    tap(&mut a, 1_000);
    a.render(&w);
    assert!(a.frame(Layer::Base).unwrap().cells[0].checked, "running");
    assert!(
        a.frame(Layer::Base).unwrap().cells[0].next_change.is_some(),
        "ticks every second while running"
    );
    // the deadline fires a notification once
    let fx = a.tick(2_000_000);
    let spawns = fx
        .iter()
        .filter(|e| matches!(e, Effect::Spawn(argv) if argv[0] == "notify-send"))
        .count();
    assert_eq!(spawns, 1, "{fx:?}");
    assert!(
        a.tick(3_000_000)
            .iter()
            .all(|e| !matches!(e, Effect::Spawn(_))),
        "only once"
    );
    a.render(&w);
    assert_eq!(text(&a), "00:00");
    // tap again resets a finished timer
    tap(&mut a, 5_000_000);
    a.render(&w);
    assert_eq!(text(&a), "00:01");
    assert!(!a.frame(Layer::Base).unwrap().cells[0].checked);
}

/// The emoji picker: the `emoji` layer shows tabs, a page of emoji and ✕; tapping one types it
/// (here via the clipboard method, which is deterministic), recents fill the first tab.
#[test]
fn emoji_picker_types_pages_and_closes() {
    use sliver_agent::agent::Effect;
    use sliver_config::{Action, LayerMode};
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent("[modules.\"emoji#strip\"]\ninsert = \"copy\"\ncolumns = 6\n");
    let w = world(9, 41);
    a.set_emoji(w.emoji.clone());
    let mut fx = Vec::new();
    a.run_action(
        &Action::Layer {
            name: "emoji".into(),
            mode: LayerMode::Toggle,
        },
        0,
        &mut fx,
    );
    a.render(&w);
    let cells = |a: &Agent| {
        a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .map(|c| match &c.content {
                sliver_render::Content::Markup(t) => t.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let before = cells(&a);
    // Esc may be on the layer; the strip is 10 tabs + 6 emoji + close
    assert_eq!(before.iter().filter(|t| *t == "✕").count(), 1, "{before:?}");
    assert!(
        before.contains(&"😀".to_string()),
        "first group when recents are empty: {before:?}"
    );
    let tap = |a: &mut Agent, key: &str, t: u64| {
        let (id, x) = region(a, key);
        let mut fx = a.touch(id, 0, TouchPhase::Down, x, t);
        fx.extend(a.touch(id, 0, TouchPhase::Up, x, t + 40_000));
        fx.extend(a.tick(t + 400_000));
        fx
    };
    // tap the first emoji: the clipboard gets it
    let fx = tap(&mut a, "emoji#strip/e0", 1_000);
    assert!(
        fx.iter().any(|e| matches!(e, Effect::Spawn(argv) if argv[0] == "wl-copy" && argv.last().unwrap() == "😀")),
        "{fx:?}"
    );
    // pick another one, then look at the recents tab: newest first
    let picked = cells(&a)[10 + 3].clone();
    tap(&mut a, "emoji#strip/e3", 1_500_000);
    a.render(&w);
    let fx = tap(&mut a, "emoji#strip/tab0", 2_000_000);
    assert!(fx.contains(&Effect::Redraw));
    a.render(&w);
    let recents = cells(&a);
    assert_eq!(
        recents[10], picked,
        "the last pick is first in recents: {recents:?}"
    );
    assert_eq!(recents[11], "😀");
    let first_page = cells(&a);
    // paging moves the window
    let (id, x) = region(&a, "emoji#strip/tab2");
    a.touch(id, 0, TouchPhase::Down, x, 3_000_000);
    a.touch(id, 0, TouchPhase::Up, x, 3_040_000);
    a.tick(3_400_000);
    a.render(&w);
    assert_ne!(cells(&a), first_page, "another tab shows other emoji");
    // ✕ closes the layer
    let fx = tap(&mut a, "emoji#strip/close", 4_000_000);
    assert!(fx.contains(&Effect::Redraw), "{fx:?}");
    a.render(&w);
    assert!(
        !cells(&a).contains(&"✕".to_string()),
        "back on the base layer"
    );
}

/// Hold an emoji with skin tones: its variants replace the page; the pick is typed, goes to
/// recents, and from then on the group page shows (and types) that tone.
#[test]
fn emoji_skin_tones() {
    use sliver_agent::agent::Effect;
    use sliver_config::{Action, LayerMode};
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent("[modules.\"emoji#strip\"]\ninsert = \"copy\"\ncolumns = 6\n");
    let w = world(9, 41);
    a.set_emoji(w.emoji.clone());
    let mut fx = Vec::new();
    let layer = Action::Layer {
        name: "emoji".into(),
        mode: LayerMode::Toggle,
    };
    a.run_action(&layer, 0, &mut fx);
    a.render(&w);
    let texts = |a: &Agent| {
        a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .filter(|c| c.key.starts_with("emoji#strip/"))
            .map(|c| match &c.content {
                sliver_render::Content::Markup(t) => t.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let touch = |a: &mut Agent, key: &str, t: u64, hold: bool| {
        let (id, x) = region(a, key);
        let mut fx = a.touch(id, 0, TouchPhase::Down, x, t);
        if hold {
            fx.extend(a.tick(t + 600_000));
        }
        fx.extend(a.touch(id, 0, TouchPhase::Up, x, t + 700_000));
        fx.extend(a.tick(t + 1_200_000));
        fx
    };
    // People & Body starts with 👋, which has tones; a hold opens them.
    touch(&mut a, "emoji#strip/tab2", 1_000_000, false);
    a.render(&w);
    assert_eq!(texts(&a)[10], "👋");
    let fx = touch(&mut a, "emoji#strip/e0", 3_000_000, true);
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Spawn(_))),
        "a hold doesn't type: {fx:?}"
    );
    a.render(&w);
    let t = texts(&a);
    assert_eq!(
        &t[10..17],
        ["‹", "👋", "👋🏻", "👋🏼", "👋🏽", "👋🏾", "👋🏿"],
        "{t:?}"
    );
    // Pick medium: typed, and the page goes back with 👋🏽 in 👋's place.
    let fx = touch(&mut a, "emoji#strip/t3", 5_000_000, false);
    assert!(
        fx.iter().any(
            |e| matches!(e, Effect::Spawn(v) if v[0] == "wl-copy" && v.last().unwrap() == "👋🏽")
        ),
        "{fx:?}"
    );
    a.render(&w);
    let t = texts(&a);
    assert_eq!(t[10], "👋🏽", "{t:?}");
    assert_eq!(w.emoji.items()[0], "👋🏽", "remembered in recents");
    // Tapping it again types the tone; ‹ from a tones view goes back without typing.
    let fx = touch(&mut a, "emoji#strip/e0", 7_000_000, false);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::Spawn(v) if v.last().unwrap() == "👋🏽"))
    );
    touch(&mut a, "emoji#strip/e0", 9_000_000, true);
    a.render(&w);
    let fx = touch(&mut a, "emoji#strip/back", 11_000_000, false);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Spawn(_))));
    a.render(&w);
    assert_eq!(texts(&a)[10], "👋🏽");
}

/// `drawer = { mode = "bar" }`: tapping the leader opens the rest of the group as a sub-bar
/// with a ✕, instead of expanding in place.
#[test]
fn bar_mode_drawer_opens_a_sub_bar() {
    use sliver_agent::agent::Effect;
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"custom/x\", \"group/tools\"]\n\
[modules.\"custom/x\"]\nformat = \"X\"\n\
[modules.\"group/tools\"]\nmodules = [\"custom/more\", \"custom/a\", \"custom/b\"]\ndrawer = { mode = \"bar\" }\n\
[modules.\"custom/more\"]\nformat = \"more\"\n[modules.\"custom/a\"]\nformat = \"A\"\non-tap = \"echo a\"\n[modules.\"custom/b\"]\nformat = \"B\"\n",
    );
    let w = world(9, 41);
    a.render(&w);
    let keys = |a: &Agent| {
        a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .map(|c| c.key.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(&a),
        ["custom/x", "group/tools/custom/more"],
        "closed: only the leader"
    );
    let tap = |a: &mut Agent, key: &str, t: u64| {
        let (id, x) = region(a, key);
        let mut fx = a.touch(id, 0, TouchPhase::Down, x, t);
        fx.extend(a.touch(id, 0, TouchPhase::Up, x, t + 40_000));
        fx.extend(a.tick(t + 400_000));
        fx
    };
    let fx = tap(&mut a, "group/tools/custom/more", 1_000);
    assert!(fx.contains(&Effect::Redraw), "{fx:?}");
    a.render(&w);
    assert_eq!(
        keys(&a),
        [
            "custom/x",
            "group/tools/custom/more",
            "group/tools/custom/a",
            "group/tools/custom/b",
            "menu/close"
        ],
        "the first zone stays; the rest of the group follows it"
    );
    let fx = tap(&mut a, "group/tools/custom/a", 2_000_000);
    assert!(fx.iter().any(|e| matches!(e, Effect::Spawn(argv) if argv.last().is_some_and(|s| s.contains("echo a")))), "{fx:?}");
    tap(&mut a, "menu/close", 3_000_000);
    a.render(&w);
    assert_eq!(
        keys(&a),
        ["custom/x", "group/tools/custom/more"],
        "✕ closes it"
    );
}

/// A module with `menu = [...]` opens a sub-bar (entries + ✕) on its trigger gesture; picking an
/// entry runs its action and closes it.
#[test]
fn menu_sub_bar_opens_runs_and_closes() {
    use sliver_agent::agent::Effect;
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"custom/pw\"]\n[modules.\"custom/pw\"]\nformat = \"P\"\nmenu = [{ label = \"Lock\", on-tap = \"echo lock\" }, { label = \"Off\", on-tap = \"echo off\" }]\n",
    );
    let w = world(9, 41);
    a.render(&w);
    let keys = |a: &Agent| {
        a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .map(|c| c.key.clone())
            .collect::<Vec<_>>()
    };
    assert!(keys(&a).contains(&"custom/pw".to_string()));
    let tap = |a: &mut Agent, key: &str, t: u64| {
        let (id, x) = region(a, key);
        let mut fx = a.touch(id, 0, TouchPhase::Down, x, t);
        fx.extend(a.touch(id, 0, TouchPhase::Up, x, t + 40_000));
        fx.extend(a.tick(t + 400_000));
        fx
    };
    let fx = tap(&mut a, "custom/pw", 1_000);
    assert!(fx.contains(&Effect::Redraw), "{fx:?}");
    a.render(&w);
    // like a modal context, the sub-bar covers everything but the layout's first zone
    assert_eq!(keys(&a), ["custom/pw", "menu/0", "menu/1", "menu/close"]);
    // picking runs the action and closes
    let fx = tap(&mut a, "menu/1", 2_000_000);
    assert!(
        fx.iter().any(|e| matches!(e, Effect::Spawn(argv) if argv.last().is_some_and(|s| s.contains("echo off")))),
        "{fx:?}"
    );
    a.render(&w);
    assert!(
        !keys(&a).contains(&"menu/0".to_string()),
        "closed after a pick"
    );
    // ✕ closes without doing anything
    tap(&mut a, "custom/pw", 3_000_000);
    a.render(&w);
    let fx = tap(&mut a, "menu/close", 4_000_000);
    assert!(!fx.iter().any(|e| matches!(e, Effect::Spawn(_))));
    a.render(&w);
    assert!(!keys(&a).contains(&"menu/close".to_string()));
}

/// Tapping the keyboard layout indicator cycles the layout on the compositor.
#[test]
fn language_tap_switches_layout() {
    use sliver_agent::agent::Effect;
    use sliver_desktop::control::Action as Desk;
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"language\"]\n",
    );
    let mut w = world(9, 41);
    w.desktop.connected = true;
    w.desktop.keyboard_layout = Some("English (US)".into());
    a.render(&w);
    let (id, x) = region(&a, "language");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 1_000);
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 40_000));
    fx.extend(a.tick(400_000));
    assert!(fx.contains(&Effect::Desktop(Desk::NextLayout)), "{fx:?}");
}

/// Holding the layout indicator lists the layouts; picking one switches to it by position.
#[test]
fn language_hold_lists_and_picks_a_layout() {
    use sliver_agent::agent::Effect;
    use sliver_desktop::control::Action as Desk;
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"language\"]\n",
    );
    let mut w = world(9, 41);
    w.desktop.connected = true;
    w.desktop.keyboard_layout = Some("English (US)".into());
    w.desktop.keyboard_layouts = vec!["English (US)".into(), "German".into(), "Greek".into()];
    w.desktop.keyboard_layout_index = Some(0);
    a.render(&w);
    let keys = |a: &Agent| {
        a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .map(|c| c.key.clone())
            .collect::<Vec<_>>()
    };
    let (id, x) = region(&a, "language");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 1_000);
    fx.extend(a.tick(1_000_000));
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 1_100_000));
    assert!(fx.contains(&Effect::Redraw), "{fx:?}");
    assert!(
        !fx.contains(&Effect::Desktop(Desk::NextLayout)),
        "a hold is not a tap"
    );
    a.render(&w);
    assert_eq!(
        keys(&a),
        ["language", "menu/0", "menu/1", "menu/2", "menu/close"]
    );
    let (id, x) = region(&a, "menu/2");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 2_000_000);
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 2_040_000));
    fx.extend(a.tick(2_400_000));
    assert!(fx.contains(&Effect::Desktop(Desk::SetLayout(2))), "{fx:?}");
    a.render(&w);
    assert!(
        !keys(&a).contains(&"menu/0".to_string()),
        "closed after the pick"
    );
    // one layout only: nothing to list, hold does nothing special
    w.desktop.keyboard_layouts = vec!["English (US)".into()];
    a.render(&w);
    let (id, x) = region(&a, "language");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 4_000_000);
    fx.extend(a.tick(5_000_000));
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 5_100_000));
    a.render(&w);
    assert!(!keys(&a).contains(&"menu/0".to_string()));
}

/// Tapping the network, bluetooth and dnd modules asks for the matching switch.
#[test]
fn network_bluetooth_dnd_taps_toggle() {
    use sliver_agent::agent::{Effect, SessionOp};
    use sliver_ipc::proto::TouchPhase;
    use sliver_session::network::{NetKind, NetState};
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"network\", \"bluetooth\", \"dnd\", \"dark-mode\"]\n",
    );
    let mut w = world(9, 41);
    w.session.network.state = NetState::Connected;
    w.session.network.kind = NetKind::Wifi;
    w.session.network.ssid = Some("Home".into());
    w.session.network.wifi_enabled = Some(true);
    w.session.bluetooth.available = true;
    w.session.bluetooth.powered = true;
    w.session.notifications.dnd = Some(false);
    w.session.appearance.dark = Some(true);
    a.render(&w);
    let mut t = 1_000u64;
    for (key, want) in [
        ("network", SessionOp::ToggleWifi),
        ("bluetooth", SessionOp::ToggleBluetooth),
        ("dnd", SessionOp::ToggleDnd),
        ("dark-mode", SessionOp::ToggleDark),
    ] {
        let (id, x) = region(&a, key);
        let mut fx = a.touch(id, 0, TouchPhase::Down, x, t);
        fx.extend(a.touch(id, 0, TouchPhase::Up, x, t + 40_000));
        fx.extend(a.tick(t + 400_000));
        assert!(fx.contains(&Effect::Session(want.clone())), "{key}: {fx:?}");
        t += 1_000_000;
    }
}

fn player(
    name: &str,
    title: &str,
    status: sliver_session::PlaybackStatus,
) -> sliver_session::Media {
    sliver_session::Media {
        player: format!("org.mpris.MediaPlayer2.{name}"),
        identity: name.into(),
        title: title.into(),
        status,
        length_us: Some(200_000_000),
        position_us: Some(100_000_000),
        can_seek: true,
        can_play_pause: true,
        track_id: Some("/track/1".into()),
        ..Default::default()
    }
}

/// Album art shows as the module's icon; holding switches players; `scrub = true` makes a
/// progress slider that seeks on drag.
#[test]
fn now_playing_art_player_picker_and_scrub() {
    use sliver_agent::agent::{Effect, SessionOp};
    use sliver_ipc::proto::TouchPhase;
    use sliver_session::PlaybackStatus::{Paused, Playing};
    let toml = "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"mpris\"]\n[modules.mpris]\nformat = \"{title}\"\n";
    let mut a = agent(toml);
    let mut w = world(9, 41);
    let one = player("one", "Song One", Playing);
    let two = player("two", "Song Two", Paused);
    w.session.media = Some(one.clone());
    w.session.players = vec![one.clone(), two.clone()];
    let art = format!(
        "{}/../sliver-render/tests/fixtures/art-48x32.jpg",
        env!("CARGO_MANIFEST_DIR")
    );
    w.media_art = Some(std::path::PathBuf::from(&art));
    a.render(&w);
    let content = |a: &Agent| a.frame(Layer::Base).unwrap().cells[0].content.clone();
    assert!(
        matches!(content(&a), sliver_render::Content::IconMarkup(_, ref t) if t == "Song One"),
        "art icon + title: {:?}",
        content(&a)
    );
    // hold: picks the next player
    let (id, x) = region(&a, "mpris");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 1_000);
    fx.extend(a.tick(1_000_000));
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 1_100_000));
    assert!(fx.contains(&Effect::Redraw), "{fx:?}");
    a.render(&w);
    assert!(
        matches!(content(&a), sliver_render::Content::Markup(ref t) | sliver_render::Content::IconMarkup(_, ref t) if t == "Song Two"),
        "{:?}",
        content(&a)
    );
    // controls now target the picked player
    let (id, x) = region(&a, "mpris");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 3_000_000);
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 3_040_000));
    fx.extend(a.tick(3_400_000));
    assert!(
        fx.iter().any(|e| matches!(e, Effect::Session(SessionOp::MediaOn { player, .. }) if player.ends_with(".two"))),
        "{fx:?}"
    );

    // scrub: a slider at the playback position; dragging seeks
    let mut a = agent(&toml.replace("format = \"{title}\"", "scrub = true"));
    let mut w = world(9, 41);
    w.session.media = Some(one.clone());
    w.session.players = vec![one];
    a.render(&w);
    let f = a.frame(Layer::Base).unwrap();
    let sliderv = match &f.cells[0].content {
        sliver_render::Content::Slider { value, .. } => *value,
        other => panic!("scrub renders a slider: {other:?}"),
    };
    assert!((sliderv - 0.5).abs() < 0.01, "100 s of 200 s: {sliderv}");
    let t = f.layout.tracks[0].expect("track");
    let (id, _) = region(&a, "mpris");
    let mut fx = a.touch(id, 0, TouchPhase::Down, t.x + t.w * 0.25, 1_000);
    fx.extend(a.touch(id, 0, TouchPhase::Move, t.x + t.w * 0.25, 20_000));
    fx.extend(a.touch(id, 0, TouchPhase::Up, t.x + t.w * 0.25, 40_000));
    fx.extend(a.tick(400_000));
    assert!(
        fx.iter().any(
            |e| matches!(e, Effect::Session(SessionOp::Seek { pct, .. }) if (24..=26).contains(pct))
        ),
        "{fx:?}"
    );
}

/// The notifications module shows the latest summary; tap opens it, a swipe dismisses it.
#[test]
fn notifications_show_latest_open_and_dismiss() {
    use sliver_agent::agent::{Effect, SessionOp};
    use sliver_ipc::proto::TouchPhase;
    let mut a = agent(
        "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"notifications\"]\n",
    );
    let mut w = world(9, 41);
    w.session.notifications.count = 2;
    w.session.notifications.latest = Some(sliver_session::notifications::Latest {
        id: 7,
        app: "chat".into(),
        summary: "Lunch at 12? <b>".into(),
    });
    a.render(&w);
    let text = match &a.frame(Layer::Base).unwrap().cells[0].content {
        sliver_render::Content::Markup(t) | sliver_render::Content::IconMarkup(_, t) => t.clone(),
        other => format!("{other:?}"),
    };
    assert_eq!(text, "Lunch at 12? &lt;b&gt;", "the summary, escaped");
    let (id, x) = region(&a, "notifications");
    let mut fx = a.touch(id, 0, TouchPhase::Down, x, 1_000);
    fx.extend(a.touch(id, 0, TouchPhase::Up, x, 40_000));
    fx.extend(a.tick(400_000));
    assert!(
        fx.contains(&Effect::Session(SessionOp::OpenNotification)),
        "{fx:?}"
    );
    // swipe left
    let mut fx = a.touch(id, 0, TouchPhase::Down, x + 150.0, 1_000_000);
    fx.extend(a.touch(id, 0, TouchPhase::Move, x + 40.0, 1_050_000));
    fx.extend(a.touch(id, 0, TouchPhase::Move, x - 100.0, 1_100_000));
    fx.extend(a.touch(id, 0, TouchPhase::Up, x - 100.0, 1_150_000));
    fx.extend(a.tick(1_500_000));
    assert!(
        fx.contains(&Effect::Session(SessionOp::DismissNotification)),
        "{fx:?}"
    );
}

/// T68: `[display]` and `[display.on-battery]` reach sliverd's real idle/backlight core.
#[test]
fn display_config_and_battery_profile_reach_sliverd() {
    let a = agent(
        r#"
        [display]
        brightness = "fixed"
        max-brightness = 200
        [display.on-battery]
        max-brightness = 40
        dim-after = "never"
        "#,
    );
    let (mut fake, link) = connect();
    let send = |link: &Link, on_battery| {
        let p =
            sliver_agent::display::policy(&a.config.display, on_battery, link.panel.max_brightness);
        link.send(&sliver_ipc::proto::AgentMsg::DisplayPolicy(p))
            .unwrap();
    };

    send(&link, Some(false));
    fake.pump().unwrap();
    assert!(fake.errors.is_empty(), "{:?}", fake.errors);
    assert_eq!(fake.policy.brightness, Some(200));
    assert_eq!(fake.policy.follow_screen, Some(false));
    assert_eq!(
        fake.hw.backlight.current(),
        Some(200),
        "AC level on the backlight"
    );

    send(&link, Some(true));
    fake.pump().unwrap();
    assert_eq!(fake.policy.brightness, Some(40));
    assert_eq!(fake.policy.dim_after_ms, Some(0), "never dims on battery");
    assert_eq!(
        fake.policy.off_after_ms,
        Some(60_000),
        "unset override keeps [display]"
    );
    assert_eq!(
        fake.hw.backlight.current(),
        Some(40),
        "battery level on the backlight"
    );
}

/// T79: with pixel shift on, the agent draws nothing within 11 px of either end, because sliverd
/// moves the whole picture up to 11 px either way. (Edge touch regions may still reach the ends.)
#[test]
fn pixel_shift_keeps_content_off_the_ends() {
    let mut a = agent("[display]\npixel-shift = true\n");
    let layers = a.render(&world(9, 41));
    assert!(a.errors.is_empty(), "{:?}", a.errors);
    // The bar background: the corner of an inset frame is never covered.
    let bg = a.frame(Layer::Base).unwrap().image.pixel(0, 0).unwrap();
    let ink_near_ends = |p: &Pixmap, n: u32| {
        let w = p.width();
        (0..p.height()).any(|y| (0..n).chain(w - n..w).any(|x| p.pixel(x, y).unwrap() != bg))
    };
    let mut plain = agent("");
    plain.render(&world(9, 41));
    assert!(
        ink_near_ends(&plain.frame(Layer::Fn).unwrap().image, 11),
        "test needs content near an end without pixel shift"
    );
    for l in layers {
        let f = a.frame(l).unwrap();
        assert!(
            !ink_near_ends(&f.image, 11),
            "{l:?}: drawn within 11 px of an end"
        );
    }
}
