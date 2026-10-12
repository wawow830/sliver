//! sliver-client against the real API server on a private `dbus-daemon`, including an agent
//! restart. Skipped when `dbus-daemon` isn't installed.

use sliver_api::identity::RealProc;
use sliver_api::item::{Gesture, ItemKind};
use sliver_api::registry::Interaction;
use sliver_api::service::{Api, Control, ControlReply, ControlState, Host};
use sliver_client::{Client, ContextOptions, Item};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
}

struct NoHost;
impl Host for NoHost {
    fn changed(&self) {}
    fn control(&self, _: Control) -> Result<ControlReply, String> {
        Ok(ControlReply::Done)
    }
    fn control_state(&self) -> ControlState {
        ControlState::default()
    }
}

fn agent(bus: &Bus) -> Api {
    zbus::block_on(async {
        let conn = zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        Api::serve(conn, Arc::new(NoHost), Arc::new(RealProc))
            .await
            .unwrap()
    })
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

fn label(api: &Api, item: &str) -> Option<String> {
    let s = api.state();
    let ctx = s.reg.contexts().next()?;
    ctx.find(item)?.label.clone()
}

#[test]
fn publish_events_and_survive_agent_restart() {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let api = agent(&bus);
    let conn = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let client = Client::new(conn).unwrap();
    assert!(client.agent_running());
    let events = client.events();

    let ctx = client
        .context(ContextOptions::status("org.example.Test").title("Test"))
        .unwrap();
    let w = ctx
        .set_items(vec![
            Item::button("go").label("Go").gestures(&["hold"]),
            Item::group("g", vec![Item::toggle("t").label("T")]),
        ])
        .unwrap();
    assert!(w.is_empty(), "{w:?}");
    ctx.update("go", Item::props().label("Going")).unwrap();
    ctx.update("t", Item::props().label("Nested")).unwrap();
    assert_eq!(label(&api, "go").as_deref(), Some("Going"));

    // Events come back to this client, mapped to the local handle.
    let id = api.state().reg.contexts().next().unwrap().id;
    zbus::block_on(api.interact(id, "go", Interaction::Gesture(Gesture::Hold))).unwrap();
    zbus::block_on(api.interact(id, "t", Interaction::Gesture(Gesture::Tap))).unwrap();
    let e = events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        (e.context, e.item.as_str(), e.kind.as_str()),
        (ctx.id(), "go", "hold")
    );
    let e = events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        (e.kind.as_str(), e.get_bool("state")),
        ("toggled", Some(true))
    );

    // The agent restarts: the client republishes the latest state on its own.
    zbus::block_on(api.connection().clone().close()).unwrap();
    drop(api);
    let api = agent(&bus);
    assert!(
        wait_until(|| label(&api, "go").as_deref() == Some("Going")),
        "not republished"
    );
    {
        let s = api.state();
        let c = s.reg.contexts().next().unwrap();
        assert_eq!(c.provider, "org.example.Test");
        let ItemKind::Group { items } = &c.find("g").unwrap().kind else {
            panic!()
        };
        assert_eq!(items[0].label.as_deref(), Some("Nested"));
    }
    // Events still flow after the restart.
    let id = api.state().reg.contexts().next().unwrap().id;
    zbus::block_on(api.interact(id, "go", Interaction::Gesture(Gesture::Tap))).unwrap();
    let e = events.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!((e.context, e.kind.as_str()), (ctx.id(), "tap"));

    ctx.destroy().unwrap();
    assert!(wait_until(|| api.state().reg.contexts().next().is_none()));
}

#[test]
fn contexts_wait_for_a_missing_agent() {
    let Some(bus) = Bus::start() else { return };
    let conn = zbus::blocking::connection::Builder::address(bus.addr.as_str())
        .unwrap()
        .build()
        .unwrap();
    let client = Client::new(conn).unwrap();
    assert!(!client.agent_running());
    let ctx = client
        .context(ContextOptions::status("org.example.Late"))
        .unwrap();
    ctx.set_items(vec![Item::button("b").label("Late")])
        .unwrap();
    let api = agent(&bus);
    assert!(wait_until(|| label(&api, "b").as_deref() == Some("Late")));
}
