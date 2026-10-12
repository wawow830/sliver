//! `{ dbus = {..., args = [..]} }` against a mock service: arguments arrive with the types the
//! method declares, not the ones TOML would guess.

use sliver_agent::ops::dbus_call;
use sliver_config::DBusCall;
use std::sync::{Arc, Mutex};
use zbus::blocking::connection::Builder;

type Calls = Arc<Mutex<Vec<String>>>;

struct Mock(Calls);

#[zbus::interface(name = "org.example.Thing")]
impl Mock {
    // `b` where TOML has only booleans, `u` where TOML has only i64
    fn set(&self, on: bool, level: u32) {
        self.0.lock().unwrap().push(format!("set {on} {level}"));
    }
    fn options(&self, opts: std::collections::HashMap<String, zbus::zvariant::OwnedValue>) {
        let mut k: Vec<_> = opts.keys().cloned().collect();
        k.sort();
        self.0
            .lock()
            .unwrap()
            .push(format!("options {}", k.join(",")));
    }
    fn ping(&self) {
        self.0.lock().unwrap().push("ping".into());
    }
}

fn call(method: &str, args: &str) -> DBusCall {
    let a: toml::Table = format!("a = {args}").parse().unwrap();
    DBusCall {
        system_bus: false,
        dest: "org.example.Thing".into(),
        path: "/org/example/Thing".into(),
        interface: "org.example.Thing".into(),
        method: method.into(),
        args: a["a"].as_array().unwrap().clone(),
    }
}

#[test]
fn arguments_take_the_methods_declared_types() {
    let Some(tb) = sliver_testbus::TestBus::start() else {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    };
    let calls = Calls::default();
    let server = Builder::address(tb.address())
        .unwrap()
        .name("org.example.Thing")
        .unwrap()
        .serve_at("/org/example/Thing", Mock(calls.clone()))
        .unwrap()
        .build()
        .unwrap();
    let client = Builder::address(tb.address()).unwrap().build().unwrap();

    dbus_call(&client, &call("Set", "[true, 7]")).unwrap();
    dbus_call(
        &client,
        &call("Options", "[{ interactive = true, modal = false }]"),
    )
    .unwrap();
    dbus_call(&client, &call("Ping", "[]")).unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        ["set true 7", "options interactive,modal", "ping"]
    );
    // mistakes are errors, not silent miscalls
    let e = dbus_call(&client, &call("Set", "[true, -1]")).unwrap_err();
    assert!(e.contains("does not fit"), "{e}");
    let e = dbus_call(&client, &call("Set", "[true]")).unwrap_err();
    assert!(e.contains("takes 2"), "{e}");
    assert_eq!(calls.lock().unwrap().len(), 3, "nothing was sent");
    drop(server);
}
