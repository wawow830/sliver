//! The agent with the public D-Bus API on a private `dbus-daemon`: apps publish items, the
//! agent draws them in App controls, and touches reach the app as `Event` signals.
//! Skipped (with a note) when `dbus-daemon` isn't installed.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
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
    child: Child,
    addr: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bus() -> Option<Bus> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut addr = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut addr)
        .ok()?;
    Some(Bus {
        child,
        addr: addr.trim().to_owned(),
    })
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
