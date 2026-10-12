//! End-to-end: the API served on a private `dbus-daemon`, driven by real D-Bus clients.
//! Skipped (with a note) when `dbus-daemon` isn't installed.

use sliver_api::identity::RealProc;
use sliver_api::item::Gesture;
use sliver_api::registry::{Focus, Interaction};
use sliver_api::service::{Api, Control, ControlReply, ControlState, Host};
use sliver_desktop::Window;
use sliver_ipc::dbus::{
    BUS_NAME, IFACE_CONTEXT, IFACE_CONTROL, IFACE_DESKTOP, IFACE_TOUCHBAR, ROOT_PATH, error,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

struct Bus {
    _tb: sliver_testbus::TestBus,
    addr: String,
}

impl Bus {
    fn start() -> Option<Bus> {
        // Isolated: no service dirs (nothing real gets D-Bus-activated) and dies with the test.
        let tb = sliver_testbus::TestBus::start()?;
        let addr = tb.address().to_owned();
        Some(Bus { _tb: tb, addr })
    }

    fn client(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.addr.as_str())
            .unwrap()
            .build()
            .unwrap()
    }
}

#[derive(Default)]
struct FakeHost {
    changed: AtomicUsize,
    requests: Mutex<Vec<Control>>,
}

impl Host for FakeHost {
    fn changed(&self) {
        self.changed.fetch_add(1, Ordering::SeqCst);
    }
    fn control(&self, req: Control) -> Result<ControlReply, String> {
        let reply = match &req {
            Control::ReloadConfig => ControlReply::Errors(vec!["config.toml:3: oops".into()]),
            Control::Refresh(m) if m == "nope" => return Err("no module nope".into()),
            _ => ControlReply::Done,
        };
        self.requests.lock().unwrap().push(req);
        Ok(reply)
    }
    fn control_state(&self) -> ControlState {
        ControlState {
            active_layer: "base".into(),
            layers: vec!["base".into(), "media".into()],
            healthy: true,
            ..Default::default()
        }
    }
}

fn setup() -> Option<(Bus, Api, Arc<FakeHost>)> {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not installed; skipping");
        return None;
    };
    let host = Arc::new(FakeHost::default());
    let h: Arc<dyn Host> = host.clone();
    let api = zbus::block_on(async {
        let conn = zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        Api::serve(conn, h, Arc::new(RealProc)).await.unwrap()
    });
    Some((bus, api, host))
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

fn create(c: &Connection, kv: &[(&str, Value<'static>)]) -> OwnedObjectPath {
    c.call_method(
        Some(BUS_NAME),
        ROOT_PATH,
        Some(IFACE_TOUCHBAR),
        "CreateContext",
        &(opts(kv),),
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap()
}

fn set_items(
    c: &Connection,
    path: &OwnedObjectPath,
    items: Vec<HashMap<String, OwnedValue>>,
) -> zbus::Result<Vec<String>> {
    c.call_method(
        Some(BUS_NAME),
        path.as_str(),
        Some(IFACE_CONTEXT),
        "SetItems",
        &(items,),
    )?
    .body()
    .deserialize()
}

fn error_name(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _) => name.to_string(),
        e => panic!("not a method error: {e}"),
    }
}

/// `Event` signals delivered to `c`, on a background thread.
fn events(c: &Connection) -> Receiver<(String, String, HashMap<String, OwnedValue>)> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(IFACE_CONTEXT)
        .unwrap()
        .member("Event")
        .unwrap()
        .build();
    let it = MessageIterator::for_match_rule(rule, c, Some(16)).unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for m in it {
            let Ok(m) = m else { break };
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

fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn button(id: &str) -> HashMap<String, OwnedValue> {
    opts(&[
        ("id", Value::from(id.to_owned())),
        ("type", Value::from("button")),
        ("label", Value::from("Hi")),
    ])
}

#[test]
fn publish_resolve_and_unicast_events() {
    let Some((bus, api, host)) = setup() else {
        return;
    };
    let a = bus.client();
    let b = bus.client();
    let a_events = events(&a);
    let b_events = events(&b);

    let path = create(&a, &[("provider", Value::from("org.example.Test"))]);
    let warnings = set_items(
        &a,
        &path,
        vec![
            button("go"),
            opts(&[("id", Value::from("x")), ("type", Value::from("hologram"))]),
        ],
    )
    .unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(host.changed.load(Ordering::SeqCst) >= 2);

    // The focused window belongs to this test process, which is also A's pid.
    api.set_focus(Focus {
        window: Some(Window {
            class: "test".into(),
            pid: Some(std::process::id()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let resolved = api.resolve();
    let ctx = resolved.app.expect("A's context fills App controls");
    assert_eq!(ctx.path(), path.as_str());
    assert_eq!(
        api.state().reg.context(ctx).unwrap().provider,
        "org.example.Test"
    );

    // B can't touch A's context.
    let e = set_items(&b, &path, vec![]).unwrap_err();
    assert_eq!(error_name(e), error::ACCESS_DENIED);

    // A tap reaches A, and only A.
    assert!(zbus::block_on(api.interact(ctx, "go", Interaction::Gesture(Gesture::Tap))).unwrap());
    let (item, kind, _) = a_events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!((item.as_str(), kind.as_str()), ("go", "tap"));
    assert!(
        b_events.recv_timeout(Duration::from_millis(300)).is_err(),
        "event leaked to another client"
    );

    // OnScreen is a real property.
    zbus::block_on(api.set_on_screen(&[ctx].into())).unwrap();
    let props = zbus::blocking::fdo::PropertiesProxy::builder(&a)
        .destination(BUS_NAME)
        .unwrap()
        .path(path.as_str())
        .unwrap()
        .build()
        .unwrap();
    let on: bool = props
        .get(IFACE_CONTEXT.try_into().unwrap(), "OnScreen")
        .unwrap()
        .try_into()
        .unwrap();
    assert!(on);

    // A disconnects: its context goes away. (close(), because the event thread holds a clone.)
    drop(props);
    a.close().unwrap();
    assert!(
        wait_until(|| api.state().reg.context(ctx).is_none()),
        "context not cleaned up"
    );
    assert!(api.resolve().app.is_none());
}

#[test]
fn control_and_desktop() {
    let Some((bus, api, host)) = setup() else {
        return;
    };
    let c = bus.client();
    let call = |member: &str, body: &dyn erased::Body| body.call(&c, member);
    call("Signal", &(8u32,)).unwrap();
    call("Trigger", &("clock".to_owned(), "hold".to_owned())).unwrap();
    let e = call("Trigger", &("clock".to_owned(), "wiggle".to_owned())).unwrap_err();
    assert_eq!(error_name(e), error::INVALID_ARGS);
    let e = call("Refresh", &("nope".to_owned(),)).unwrap_err();
    assert_eq!(error_name(e), "io.github.wawow830.Sliver.Error.Failed");
    let errs: Vec<String> = call("ReloadConfig", &())
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(errs, vec!["config.toml:3: oops"]);
    assert_eq!(
        *host.requests.lock().unwrap(),
        vec![
            Control::Signal(8),
            Control::Trigger("clock".into(), Gesture::Hold),
            Control::ReloadConfig
        ]
    );
    let props = zbus::blocking::fdo::PropertiesProxy::builder(&c)
        .destination(BUS_NAME)
        .unwrap()
        .path(ROOT_PATH)
        .unwrap()
        .build()
        .unwrap();
    let layer: String = props
        .get(IFACE_CONTROL.try_into().unwrap(), "ActiveLayer")
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(layer, "base");

    // Desktop1: the first focus source wins, others are busy.
    let focus = opts(&[
        ("app-id", Value::from("firefox")),
        ("pid", Value::from(4242u32)),
        ("workspace", Value::from("3")),
    ]);
    c.call_method(
        Some(BUS_NAME),
        ROOT_PATH,
        Some(IFACE_DESKTOP),
        "SetFocus",
        &(focus.clone(),),
    )
    .unwrap();
    {
        let s = api.state();
        let w = s.focus.window.as_ref().unwrap();
        assert_eq!(
            (w.class.as_str(), w.pid, s.focus.workspace.as_deref()),
            ("firefox", Some(4242), Some("3"))
        );
    }
    let d = bus.client();
    let e = d
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_DESKTOP),
            "SetFocus",
            &(focus.clone(),),
        )
        .unwrap_err();
    assert_eq!(error_name(e), error::BUSY);
    // Workspaces, both as aa{sv} and as av of a{sv} (KWin).
    let ws = vec![opts(&[
        ("id", Value::from("1")),
        ("active", Value::from(true)),
    ])];
    c.call_method(
        Some(BUS_NAME),
        ROOT_PATH,
        Some(IFACE_DESKTOP),
        "SetWorkspaces",
        &(ws,),
    )
    .unwrap();
    assert_eq!(api.state().workspaces.len(), 1);
    let wsv: Vec<OwnedValue> = (1..=3)
        .map(|i| {
            OwnedValue::try_from(Value::from(opts(&[("id", Value::from(i.to_string()))]))).unwrap()
        })
        .collect();
    c.call_method(
        Some(BUS_NAME),
        ROOT_PATH,
        Some(IFACE_DESKTOP),
        "SetWorkspacesV",
        &(wsv,),
    )
    .unwrap();
    assert_eq!(api.state().workspaces.len(), 3);
    let bad = vec![OwnedValue::from(5i32)];
    let e = c
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_DESKTOP),
            "SetWorkspacesV",
            &(bad,),
        )
        .unwrap_err();
    assert_eq!(error_name(e), error::INVALID_ARGS);

    // When the source leaves, someone else may take over.
    drop(props);
    c.close().unwrap();
    assert!(wait_until(|| d
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_DESKTOP),
            "SetFocus",
            &(focus.clone(),)
        )
        .is_ok()));
}

#[test]
fn limits_and_bad_input() {
    let Some((bus, _api, _host)) = setup() else {
        return;
    };
    let c = bus.client();
    let e = c
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[("scope", Value::from("everywhere"))]),),
        )
        .unwrap_err();
    assert_eq!(error_name(e), error::INVALID_ARGS);
    let path = create(&c, &[]);
    let e = set_items(&c, &path, vec![button("a"), button("a")]).unwrap_err();
    assert_eq!(error_name(e), error::INVALID_ARGS);
    for _ in 1..sliver_ipc::dbus::limits::CONTEXTS_PER_CLIENT {
        create(&c, &[]);
    }
    let e = c
        .call_method(
            Some(BUS_NAME),
            ROOT_PATH,
            Some(IFACE_TOUCHBAR),
            "CreateContext",
            &(opts(&[]),),
        )
        .unwrap_err();
    assert_eq!(error_name(e), error::LIMIT_EXCEEDED);
}

/// Tiny helper so the control test can call methods with different body types.
mod erased {
    use super::*;
    pub trait Body {
        fn call(&self, c: &Connection, member: &str) -> zbus::Result<zbus::Message>;
    }
    impl<T: serde::Serialize + zbus::zvariant::DynamicType> Body for T {
        fn call(&self, c: &Connection, member: &str) -> zbus::Result<zbus::Message> {
            c.call_method(Some(BUS_NAME), ROOT_PATH, Some(IFACE_CONTROL), member, self)
        }
    }
}
