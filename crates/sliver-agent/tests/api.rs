//! The agent with the public D-Bus API on a private `dbus-daemon`: apps publish items, the
//! agent draws them in App controls, and touches reach the app as `Event` signals.
//! Skipped (with a note) when `dbus-daemon` isn't installed.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

use chrono::{Local, TimeZone};
use sliver_agent::agent::Effect;
use sliver_agent::api::ApiLink;
use sliver_agent::link::{Event, Link};
use sliver_agent::modules::World;
use sliver_agent::published::Published;
use sliver_agent::{Agent, icons::Icons};
use sliver_api::registry::Focus;
use sliver_desktop::Window;
use sliver_ipc::dbus::{BUS_NAME, IFACE_CONTEXT, IFACE_TOUCHBAR, ROOT_PATH};
use sliver_ipc::proto::{Layer, TouchPhase};
use sliver_render::{Fonts, Pixmap, Renderer, Stylesheet};
use sliverd::testing::TestDaemon;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

struct Bus {
    _tb: sliver_testbus::TestBus,
    addr: String,
}

fn bus() -> Option<Bus> {
    // Isolated: no service dirs (nothing real gets D-Bus-activated) and dies with the test.
    let tb = sliver_testbus::TestBus::start()?;
    let addr = tb.address().to_owned();
    Some(Bus { _tb: tb, addr })
}

fn opts(kv: &[(&str, Value<'static>)]) -> HashMap<String, OwnedValue> {
    kv.iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                OwnedValue::try_from(v.try_clone().unwrap()).unwrap(),
            )
        })
        .collect()
}

fn events(c: &Connection) -> Receiver<(String, String, HashMap<String, OwnedValue>)> {
    let rule = zbus::MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(IFACE_CONTEXT)
        .unwrap()
        .member("Event")
        .unwrap()
        .build();
    let it = MessageIterator::for_match_rule(rule, c, Some(16)).unwrap();
    let (tx, rx) = channel();
    thread::spawn(move || {
        for m in it.flatten() {
            if let Ok(ev) = m
                .body()
                .deserialize::<(String, String, HashMap<String, OwnedValue>)>()
                && tx.send(ev).is_err()
            {
                break;
            }
        }
    });
    rx
}

fn check(name: &str, p: &Pixmap) {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/snapshots/{name}.png"));
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
        p.save_png(format!(
            "{}/../../target/{name}-actual.png",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
    }
    assert!(bad <= 20, "{name}: {bad} pixels differ");
}

#[test]
fn published_items_end_to_end() {
    let Some(bus) = bus() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let conn = zbus::block_on(
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build(),
    )
    .unwrap();
    let api = ApiLink::serve(conn, true).unwrap();

    // An app publishes a button, a toggle that's on, and a slider.
    let app = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let app_events = events(&app);
    let path: OwnedObjectPath = app
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[("provider", Value::from("org.example.Editor"))]),),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let items = vec![
        opts(&[
            ("id", Value::from("bold")),
            ("type", Value::from("toggle")),
            ("label", Value::from("Bold")),
            ("state", Value::from(true)),
        ]),
        opts(&[
            ("id", Value::from("run")),
            ("type", Value::from("button")),
            ("label", Value::from("Run")),
        ]),
        opts(&[
            ("id", Value::from("zoom")),
            ("type", Value::from("slider")),
            ("value", Value::from(50.0)),
            ("min", Value::from(0.0)),
            ("max", Value::from(200.0)),
            ("step", Value::from(10.0)),
        ]),
    ];
    let warnings: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(items,),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(api.drain().0, "Host::changed reached the agent");

    // The app's window is focused, so it fills App controls.
    api.set_focus(Focus {
        window: Some(Window {
            class: "editor".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut world = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap());
    world.published = Published::snapshot(&api.api);
    assert!(world.published.app.is_some());

    let loaded = sliver_config::load_str("", std::path::Path::new("."));
    let mut a = Agent::new(
        loaded.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    );
    let (mut fake, conn) = TestDaemon::new(TestDaemon::mac14_7()).unwrap();
    let t = thread::spawn(move || Link::handshake(conn, "api-test"));
    while !t.is_finished() {
        fake.pump().unwrap();
        thread::sleep(Duration::from_millis(1));
    }
    let mut link = t.join().unwrap().unwrap();
    for l in a.render(&world) {
        let f = a.frame(l).unwrap();
        link.present(l, &f.image, &f.pressed, &f.regions).unwrap();
    }
    fake.pump().unwrap();
    link.poll().unwrap();
    assert!(fake.errors.is_empty(), "{:?}", fake.errors);
    let f = a.frame(Layer::Base).unwrap();
    check("api-app-controls", &f.image);
    let bold = f
        .cells
        .iter()
        .find(|c| c.key.ends_with("#bold"))
        .expect("toggle drawn");
    assert!(
        bold.checked && bold.classes.contains(&"focused-app".to_string()),
        "{:?}",
        bold.classes
    );

    // Tap "Run": the app gets a tap event, unicast.
    let region = |a: &Agent, suffix: &str| {
        let f = a.frame(Layer::Base).unwrap();
        let i = f
            .cells
            .iter()
            .position(|c| c.key.ends_with(suffix))
            .unwrap();
        (
            f.regions
                .iter()
                .find(|r| r.id == i as u32 + 1)
                .unwrap()
                .rect,
            f.layout.tracks[i],
        )
    };
    let deliver = |a: &mut Agent, link: &mut Link, fake: &mut TestDaemon| {
        fake.pump().unwrap();
        let mut fx = Vec::new();
        for ev in link.poll().unwrap() {
            if let Event::Touch {
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
        for e in &fx {
            if let Effect::Api {
                context,
                item,
                what,
            } = e
            {
                api.interact(*context, item, *what);
            }
        }
        fx
    };
    let (r, _) = region(&a, "#run");
    fake.tap((r.x + r.w / 2) as f32, 30.0).unwrap();
    deliver(&mut a, &mut link, &mut fake);
    let (item, kind, _) = app_events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!((item.as_str(), kind.as_str()), ("run", "tap"));

    // Drag the slider to 3/4: the app hears begin…end in its own units, snapped to its step.
    let (_, track) = region(&a, "#zoom");
    let t = track.expect("slider track");
    fake.touch(3, TouchPhase::Down, t.x + t.w * 0.74, 30.0)
        .unwrap();
    fake.touch(3, TouchPhase::Up, t.x + t.w * 0.74, 30.0)
        .unwrap();
    let fx = deliver(&mut a, &mut link, &mut fake);
    let values: Vec<String> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Api { what, .. } => Some(format!("{what:?}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        values,
        [
            "Slider { value: 150.0, phase: Begin }",
            "Slider { value: 150.0, phase: End }"
        ]
    );
    let mut kinds = Vec::new();
    while let Ok((item, kind, _)) = app_events.recv_timeout(Duration::from_millis(500)) {
        kinds.push(format!("{item}:{kind}"));
    }
    assert!(kinds.iter().any(|k| k.starts_with("zoom:")), "{kinds:?}");
}

#[test]
fn modal_context_covers_the_bar_and_closes() {
    let Some(bus) = bus() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let conn = zbus::block_on(
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build(),
    )
    .unwrap();
    let api = ApiLink::serve(conn, true).unwrap();
    let app = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let app_events = events(&app);
    let path: OwnedObjectPath = app
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[
                ("provider", Value::from("org.example.Picker")),
                ("modal", Value::from(true)),
            ]),),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let items = vec![opts(&[
        ("id", Value::from("pick")),
        ("type", Value::from("segmented")),
        (
            "segments",
            Value::from(
                ["Red", "Green", "Blue"]
                    .map(|l| opts(&[("label", Value::from(l))]))
                    .to_vec(),
            ),
        ),
        ("selected", Value::from(1i32)),
    ])];
    let _: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(items,),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    api.set_focus(Focus {
        window: Some(Window {
            class: "picker".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut world = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap());
    world.published = Published::snapshot(&api.api);
    assert!(world.published.modal, "{:?}", world.published);

    let loaded = sliver_config::load_str("", std::path::Path::new("."));
    let mut a = Agent::new(
        loaded.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    );
    a.render(&world);
    let f = a.frame(Layer::Base).unwrap();
    check("api-modal", &f.image);
    let keys: Vec<&str> = f.cells.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys.len(), 4, "3 segments + close, nothing else: {keys:?}");
    assert!(f.cells[1].checked, "selected segment");

    // Tap the ✕ (simulated): the app is told, and the context stops being shown.
    let close = f
        .cells
        .iter()
        .position(|c| c.key.ends_with("#close"))
        .unwrap() as u32
        + 1;
    let fx = a.touch(close, 0, TouchPhase::Down, 1.0, 0);
    let mut fx2 = a.touch(close, 0, TouchPhase::Up, 1.0, 1000);
    fx2.extend(fx);
    for e in &fx2 {
        if let Effect::Api {
            context,
            item,
            what,
        } = e
        {
            api.interact(*context, item, *what);
        }
    }
    assert!(
        fx2.iter().any(|e| matches!(
            e,
            Effect::Api {
                what: sliver_api::registry::Interaction::CloseModal,
                ..
            }
        )),
        "{fx2:?}"
    );
    let (_, kind, _) = app_events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(kind, "closed");
    world.published = Published::snapshot(&api.api);
    assert!(world.published.app.is_none() || !world.published.modal);
}

/// T77: a scrubber shows a page of candidates; a swipe scrolls it (optimistically, at once), the
/// arrows page, and a tap picks. The app hears `scrolled` and `selected`.
#[test]
fn scrubber_scrolls_and_picks() {
    let Some(bus) = bus() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let conn = zbus::block_on(
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build(),
    )
    .unwrap();
    let api = ApiLink::serve(conn, true).unwrap();
    let app = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let app_events = events(&app);
    let path: OwnedObjectPath = app
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[
                ("provider", Value::from("org.example.Tabs")),
                ("modal", Value::from(true)),
            ]),),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let labels: Vec<String> = (0..10).map(|i| format!("Tab {i}")).collect();
    let items = vec![opts(&[
        ("id", Value::from("tabs")),
        ("type", Value::from("scrubber")),
        (
            "candidates",
            Value::from(
                labels
                    .iter()
                    .map(|l| opts(&[("label", Value::from(l.clone()))]))
                    .collect::<Vec<_>>(),
            ),
        ),
        ("visible-count", Value::from(4i32)),
        ("selected", Value::from(1i32)),
    ])];
    let warnings: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(items,),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    api.set_focus(Focus {
        window: Some(Window {
            class: "browser".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut world = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap());
    world.published = Published::snapshot(&api.api);
    let loaded = sliver_config::load_str("", std::path::Path::new("."));
    let mut a = Agent::new(
        loaded.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    );
    let shown = |a: &mut Agent, world: &World| {
        a.render(world);
        let f = a.frame(Layer::Base).unwrap();
        f.cells
            .iter()
            .filter(|c| !c.key.ends_with("#close"))
            .map(|c| {
                let t = match &c.content {
                    sliver_render::Content::Text(t) => t.clone(),
                    other => format!("{other:?}"),
                };
                if c.checked { format!("[{t}]") } else { t }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shown(&mut a, &world),
        ["Tab 0", "[Tab 1]", "Tab 2", "Tab 3", "›"]
    );

    // Feed the agent's effects to the API, as the main loop does.
    let apply = |fx: &[Effect]| {
        for e in fx {
            if let Effect::Api {
                context,
                item,
                what,
            } = e
            {
                api.interact(*context, item, *what);
            }
        }
    };
    let region = |a: &Agent, key_end: &str| {
        let f = a.frame(Layer::Base).unwrap();
        f.cells
            .iter()
            .position(|c| c.key.ends_with(key_end))
            .unwrap() as u32
            + 1
    };
    // Swipe left on a candidate: the next page.
    let r = region(&a, "#tabs.2");
    let mut fx = a.touch(r, 0, TouchPhase::Down, 600.0, 1_000);
    fx.extend(a.touch(r, 0, TouchPhase::Move, 560.0, 30_000));
    fx.extend(a.touch(r, 0, TouchPhase::Move, 480.0, 60_000));
    fx.extend(a.touch(r, 0, TouchPhase::Up, 480.0, 90_000));
    fx.extend(a.tick(500_000));
    apply(&fx);
    let (item, kind, data) = app_events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!((item.as_str(), kind.as_str()), ("tabs", "scrolled"));
    assert_eq!(i32::try_from(&data["offset"]).unwrap(), 4);
    world.published = Published::snapshot(&api.api);
    assert_eq!(
        shown(&mut a, &world),
        ["‹", "Tab 4", "Tab 5", "Tab 6", "Tab 7", "›"]
    );
    // Cells glide to their new widths (`transition: width`): snapshot once that has settled.
    while let Some(due) = a.animation_deadline() {
        std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
        a.render(&world);
    }
    check("api-scrubber", &a.frame(Layer::Base).unwrap().image);
    // The › arrow: the last page is full, so it stops at 6.
    let r = region(&a, "#tabs.next");
    let mut fx = a.touch(r, 0, TouchPhase::Down, 1.0, 1_000_000);
    fx.extend(a.touch(r, 0, TouchPhase::Up, 1.0, 1_040_000));
    fx.extend(a.tick(1_500_000));
    apply(&fx);
    let (_, kind, data) = app_events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(kind, "scrolled");
    assert_eq!(i32::try_from(&data["offset"]).unwrap(), 6);
    world.published = Published::snapshot(&api.api);
    assert_eq!(
        shown(&mut a, &world),
        ["‹", "Tab 6", "Tab 7", "Tab 8", "Tab 9"]
    );
    // Tap a candidate: picked, shown checked at once.
    let r = region(&a, "#tabs.8");
    let mut fx = a.touch(r, 0, TouchPhase::Down, 1.0, 2_000_000);
    fx.extend(a.touch(r, 0, TouchPhase::Up, 1.0, 2_040_000));
    fx.extend(a.tick(2_500_000));
    apply(&fx);
    let (_, kind, data) = app_events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(kind, "selected");
    assert_eq!(i32::try_from(&data["index"]).unwrap(), 8);
    world.published = Published::snapshot(&api.api);
    assert_eq!(
        shown(&mut a, &world),
        ["‹", "Tab 6", "Tab 7", "[Tab 8]", "Tab 9"]
    );
}

/// T80: a popover item opens its items full-width with a ✕ (NSPopoverTouchBarItem); the app
/// hears `popover open=true/false`, and taps inside reach the child items.
#[test]
fn popover_opens_and_closes() {
    let Some(bus) = bus() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let conn = zbus::block_on(
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build(),
    )
    .unwrap();
    let api = ApiLink::serve(conn, true).unwrap();
    let app = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let app_events = events(&app);
    let path: OwnedObjectPath = app
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[
                ("provider", Value::from("org.example.Editor")),
                ("modal", Value::from(true)),
            ]),),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let button = |id: &str, label: &str| {
        opts(&[
            ("id", Value::from(id.to_owned())),
            ("type", Value::from("button")),
            ("label", Value::from(label.to_owned())),
        ])
    };
    let items = vec![
        button("bold", "B"),
        opts(&[
            ("id", Value::from("more")),
            ("type", Value::from("popover")),
            ("label", Value::from("More")),
            (
                "items",
                Value::from(vec![button("strike", "S"), button("code", "Code")]),
            ),
        ]),
    ];
    let warnings: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(items,),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    api.set_focus(Focus {
        window: Some(Window {
            class: "editor".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut world = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap());
    world.published = Published::snapshot(&api.api);
    let loaded = sliver_config::load_str("", std::path::Path::new("."));
    let mut a = Agent::new(
        loaded.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    );
    let keys = |a: &mut Agent, world: &World| {
        a.render(world);
        a.frame(Layer::Base)
            .unwrap()
            .cells
            .iter()
            .filter(|c| c.published.is_some())
            .map(|c| c.key.rsplit_once('#').unwrap().1.to_owned())
            .collect::<Vec<_>>()
    };
    let tap = |a: &mut Agent, key_end: &str, t: u64| {
        let f = a.frame(Layer::Base).unwrap();
        let r = f
            .cells
            .iter()
            .position(|c| c.key.ends_with(key_end))
            .unwrap() as u32
            + 1;
        let mut fx = a.touch(r, 0, TouchPhase::Down, 1.0, t);
        fx.extend(a.touch(r, 0, TouchPhase::Up, 1.0, t + 40_000));
        fx.extend(a.tick(t + 500_000));
        for e in &fx {
            if let Effect::Api {
                context,
                item,
                what,
            } = e
            {
                api.interact(*context, item, *what);
            }
        }
    };
    let next = || {
        let (item, kind, data) = app_events.recv_timeout(Duration::from_secs(3)).unwrap();
        let open = data.get("open").map(|v| bool::try_from(v).unwrap());
        (item, kind, open)
    };
    assert_eq!(keys(&mut a, &world), ["bold", "more", "close"]);

    tap(&mut a, "#more", 1_000);
    assert_eq!(next(), ("more".into(), "popover".into(), Some(true)));
    world.published = Published::snapshot(&api.api);
    assert_eq!(keys(&mut a, &world), ["strike", "code", "more.close"]);
    let f = a.frame(Layer::Base).unwrap();
    assert!(
        f.cells[f.cells.len() - 1]
            .classes
            .contains(&"popover-close".to_owned())
    );
    while let Some(due) = a.animation_deadline() {
        std::thread::sleep(due.saturating_duration_since(std::time::Instant::now()));
        a.render(&world);
    }
    check("api-popover", &a.frame(Layer::Base).unwrap().image);

    // A tap inside reaches the child item; the popover stays open.
    tap(&mut a, "#code", 1_000_000);
    assert_eq!(next(), ("code".into(), "tap".into(), None));
    assert_eq!(keys(&mut a, &world), ["strike", "code", "more.close"]);

    tap(&mut a, "#more.close", 2_000_000);
    assert_eq!(next(), ("more".into(), "popover".into(), Some(false)));
    assert_eq!(keys(&mut a, &world), ["bold", "more", "close"]);

    // An open popover whose item disappears closes by itself.
    tap(&mut a, "#more", 3_000_000);
    assert_eq!(next().1, "popover");
    let _: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(vec![button("bold", "B")],),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    world.published = Published::snapshot(&api.api);
    assert_eq!(keys(&mut a, &world), ["bold", "close"]);
}

/// T81: `priority` drops the lowest items when the app zone overflows (a group as a unit),
/// `width = {min, max}` bounds an item, `icon-data` takes SVG, indeterminate progress is marked.
#[test]
fn priority_overflow_width_svg_and_indeterminate() {
    let Some(bus) = bus() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let conn = zbus::block_on(
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build(),
    )
    .unwrap();
    let api = ApiLink::serve(conn, true).unwrap();
    let app = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let path: OwnedObjectPath = app
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[
                ("provider", Value::from("org.example.Busy")),
                ("modal", Value::from(true)),
            ]),),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let wide = "a rather long button label";
    let button = |id: String, prio: i32| {
        opts(&[
            ("id", Value::from(id)),
            ("type", Value::from("button")),
            ("label", Value::from(wide)),
            ("priority", Value::from(prio)),
        ])
    };
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="red"/></svg>"#;
    let mut items: Vec<_> = (0..12).map(|i| button(format!("low{i}"), -1)).collect();
    items.push(opts(&[
        ("id", Value::from("keep")),
        ("type", Value::from("group")),
        ("priority", Value::from(5i32)),
        (
            "items",
            Value::from(vec![button("g1".into(), 0), button("g2".into(), 0)]),
        ),
    ]));
    items.push(opts(&[
        ("id", Value::from("capped")),
        ("type", Value::from("button")),
        ("label", Value::from(wide)),
        ("width", Value::from(opts(&[("max", Value::from(90i32))]))),
        ("priority", Value::from(5i32)),
    ]));
    items.push(opts(&[
        ("id", Value::from("logo")),
        ("type", Value::from("button")),
        ("icon-data", Value::from(svg.to_vec())),
        ("priority", Value::from(5i32)),
    ]));
    items.push(opts(&[
        ("id", Value::from("busy")),
        ("type", Value::from("progress")),
        ("indeterminate", Value::from(true)),
        ("priority", Value::from(5i32)),
    ]));
    let warnings: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(items,),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    api.set_focus(Focus {
        window: Some(Window {
            class: "busy".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let mut world = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap());
    world.published = Published::snapshot(&api.api);
    let loaded = sliver_config::load_str("", std::path::Path::new("."));
    let mut a = Agent::new(
        loaded.config,
        Stylesheet::sliver(),
        Renderer::new(Fonts::embedded()),
        Icons::builtin_only(),
        2008,
        60,
    );
    a.render(&world);
    let f = a.frame(Layer::Base).unwrap();
    let ids: Vec<String> = f
        .cells
        .iter()
        .map(|c| c.key.rsplit_once('#').unwrap().1.to_owned())
        .collect();
    // Some low-priority buttons gave way; everything with priority 5 (the group whole) stayed.
    let lows = ids.iter().filter(|i| i.starts_with("low")).count();
    assert!(lows > 0 && lows < 12, "{ids:?}");
    for keep in ["g1", "g2", "capped", "logo", "busy", "close"] {
        assert!(ids.iter().any(|i| i == keep), "{keep} missing: {ids:?}");
    }
    // The ones that stayed are the first ones (the last low-priority items go first).
    assert!(ids.contains(&"low0".to_owned()), "{ids:?}");
    assert!(!f.layout.overflow.iter().any(|o| *o), "fits now: {ids:?}");
    let at = |id: &str| ids.iter().position(|i| i == id).unwrap();
    assert!(
        f.layout.items[at("capped")].w <= 90.0,
        "{:?}",
        f.layout.items[at("capped")]
    );
    assert!(matches!(
        f.cells[at("logo")].content,
        sliver_render::Content::Icon(_)
    ));
    assert!(
        f.cells[at("busy")]
            .classes
            .contains(&"indeterminate".to_owned())
    );
}

/// `width` on App controls is a slot: the app's items lay out inside it, the rest is empty, and
/// what doesn't fit gives way by priority. An empty slot keeps its width.
#[test]
fn app_controls_width_is_a_slot() {
    let Some(bus) = bus() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let conn = zbus::block_on(
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build(),
    )
    .unwrap();
    let api = ApiLink::serve(conn, true).unwrap();
    let config = |width: &str| {
        sliver_config::load_str(
            &format!(
                "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [\"app-controls\", \"custom/after\"]\n\
                 [modules.app-controls]\nwidth = {width}\n[modules.\"custom/after\"]\nformat = \"after\"\n"
            ),
            std::path::Path::new("."),
        )
        .config
    };
    let render = |width: &str, api: &ApiLink| {
        let mut world = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap());
        world.published = Published::snapshot(&api.api);
        let mut a = Agent::new(
            config(width),
            Stylesheet::sliver(),
            Renderer::new(Fonts::embedded()),
            Icons::builtin_only(),
            2008,
            60,
        );
        a.render(&world);
        let f = a.frame(Layer::Base).unwrap();
        let after = f.cells.iter().position(|c| c.id == "custom/after").unwrap();
        let ids: Vec<String> = f
            .cells
            .iter()
            .filter(|c| c.published.is_some())
            .map(|c| c.key.rsplit_once('#').unwrap().1.to_owned())
            .collect();
        // Where the slot ends: the item after it, minus one gap.
        let zone_x = f.layout.zones[0].x;
        let start_of_after = f.layout.items[after].x - zone_x;
        (
            ids,
            start_of_after,
            f.layout.items.clone(),
            after,
            f.image.clone(),
        )
    };
    // Empty: the slot is still reserved. ("auto" collapses to nothing.)
    let (_, empty_end, _, _, _) = render("400", &api);
    let (_, auto_end, _, _, _) = render("\"auto\"", &api);
    // (400 px plus the one gap before the next module.)
    let reserved = empty_end - auto_end;
    assert!(
        (400.0..=420.0).contains(&reserved),
        "{empty_end} vs {auto_end}"
    );

    // Six buttons, b0 lowest priority: only what fits in 400 px stays, and the slot stays 400.
    let app = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let path: OwnedObjectPath = app
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[("provider", Value::from("org.example.Slot"))]),),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let items: Vec<_> = (0..6)
        .map(|i| {
            opts(&[
                ("id", Value::from(format!("b{i}"))),
                ("type", Value::from("button")),
                ("label", Value::from("Button")),
                ("priority", Value::from(i)),
            ])
        })
        .collect();
    let _: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(items,),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    api.set_focus(Focus {
        window: Some(Window {
            class: "slot".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let (ids, end, items, after, image) = render("400", &api);
    check("api-slot", &image);
    assert!(
        !ids.is_empty() && ids.len() < 6,
        "some fit, some gave way: {ids:?}"
    );
    assert!(
        !ids.contains(&"b0".to_owned()),
        "lowest priority goes first: {ids:?}"
    );
    assert!(ids.contains(&"b5".to_owned()), "{ids:?}");
    assert!(
        (end - empty_end).abs() <= 1.0,
        "slot keeps its 400 px: {end} vs {empty_end}"
    );
    assert!(items[after - 1].w >= 0.0);
    // fill: the slot takes the zone's leftover, so the next module is pushed to the far end.
    let (ids, end, _, _, _) = render("\"fill\"", &api);
    assert_eq!(ids.len(), 6, "{ids:?}");
    assert!(end > 1500.0, "{end}");
}

/// T121: a published item placed by the layout (`app/<provider>#<item>`) is a module like any
/// other: it leaves Status items, takes the user's format over its values, and a user binding
/// replaces the app's event (unless pass-through).
#[test]
fn pinned_published_item_is_a_module() {
    let Some(bus) = bus() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let conn = zbus::block_on(
        zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build(),
    )
    .unwrap();
    let api = ApiLink::serve(conn, true).unwrap();
    let app = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let app_events = events(&app);
    let path: OwnedObjectPath = app
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[
                ("provider", Value::from("org.example.Pomo")),
                ("scope", Value::from("status")),
            ]),),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let items = vec![
        opts(&[
            ("id", Value::from("timer")),
            ("type", Value::from("button")),
            ("label", Value::from("▶ 25:00")),
            (
                "values",
                Value::from(opts(&[("minutes", Value::from(25i32))])),
            ),
        ]),
        opts(&[
            ("id", Value::from("skip")),
            ("type", Value::from("button")),
            ("label", Value::from("Skip")),
        ]),
    ];
    let _: Vec<String> = app
        .call_method(
            Some(BUS_NAME),
            path.as_str(),
            Some(IFACE_CONTEXT),
            "SetItems",
            &(items,),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    let mut world = World::at(Local.with_ymd_and_hms(2026, 10, 12, 9, 41, 0).unwrap());
    world.published = Published::snapshot(&api.api);
    let config = |extra: &str| {
        let l = sliver_config::load_str(
            &format!(
                r#"
version = 1
[[layouts.default.zones]]
name = "main"
modules = ["app/org.example.Pomo#timer"]
[[layouts.default.zones]]
name = "controls"
modules = ["status-items"]
[modules."app/org.example.Pomo#timer"]
format = "{{minutes}}m left"
class = "pomo"
on-hold = {{ key = "F5" }}
[modules."app/org.example.Pomo#skip"]   # not placed: still applies inside Status items
format = "⏭ {{text}}"
{extra}
"#
            ),
            std::path::Path::new("."),
        );
        assert!(l.diagnostics.ok(), "{:?}", l.diagnostics);
        Agent::new(
            l.config,
            Stylesheet::sliver(),
            Renderer::new(Fonts::embedded()),
            Icons::builtin_only(),
            2008,
            60,
        )
    };
    let mut a = config("");
    a.render(&world);
    let f = a.frame(Layer::Base).unwrap();
    let shown: Vec<(String, String)> = f
        .cells
        .iter()
        .filter(|c| c.published.is_some())
        .map(|c| {
            let t = match &c.content {
                sliver_render::Content::Markup(t) | sliver_render::Content::Text(t) => t.clone(),
                other => format!("{other:?}"),
            };
            (c.id.clone(), t)
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                "app/org.example.Pomo#timer".to_owned(),
                "25m left".to_owned()
            ),
            ("app/org.example.Pomo#skip".to_owned(), "⏭ Skip".to_owned()),
        ],
        "timer pinned in main with the user's format; skip left in Status items, with its own format"
    );
    let timer = &f.cells[f
        .cells
        .iter()
        .position(|c| c.id.ends_with("#timer"))
        .unwrap()];
    assert!(timer.classes.contains(&"pomo".to_owned()));

    // A tap still reaches the app; the user's on-hold replaces the app's hold.
    let r = f
        .cells
        .iter()
        .position(|c| c.id.ends_with("#timer"))
        .unwrap() as u32
        + 1;
    let mut fx = a.touch(r, 0, TouchPhase::Down, 1.0, 1_000);
    fx.extend(a.touch(r, 0, TouchPhase::Up, 1.0, 40_000));
    fx.extend(a.tick(500_000));
    for e in &fx {
        if let Effect::Api {
            context,
            item,
            what,
        } = e
        {
            api.interact(*context, item, *what);
        }
    }
    let (item, kind, _) = app_events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!((item.as_str(), kind.as_str()), ("timer", "tap"));
    let mut fx = a.touch(r, 0, TouchPhase::Down, 1.0, 2_000_000);
    fx.extend(a.tick(3_000_000));
    fx.extend(a.touch(r, 0, TouchPhase::Up, 1.0, 3_100_000));
    assert!(
        fx.iter().any(|e| matches!(e, Effect::EmitKeys(..))),
        "user on-hold: {fx:?}"
    );
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Api { .. })),
        "the app doesn't hear the hold: {fx:?}"
    );
}
