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
    assert_eq!(deliver(&mut a, &mut link, &mut fake), [Redraw]);
    let layers = a.render(&world(9, 41));
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
    let layers = a.render(&world(9, 41));
    present(&a, &mut link, &mut fake, &layers);
    check("touchy-media-layer", &a.frame(Layer::Base).unwrap().image);
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
