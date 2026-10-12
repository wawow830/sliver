//! Keeps `examples/python/pomodoro.py` honest: run it against the real API server on a private
//! bus. Skipped when `dbus-daemon` or python3-gobject isn't available.

use sliver_api::identity::RealProc;
use sliver_api::item::Gesture;
use sliver_api::registry::Interaction;
use sliver_api::service::{Api, Control, ControlReply, ControlState, Host};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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

fn label(api: &Api) -> Option<String> {
    let s = api.state();
    s.reg.contexts().next()?.find("timer")?.label.clone()
}

#[test]
fn python_pomodoro() {
    let gi = Command::new("python3")
        .args(["-c", "import gi; gi.require_version('Gio','2.0')"])
        .status();
    if !gi.is_ok_and(|s| s.success()) {
        eprintln!("python3-gobject not available; skipping");
        return;
    }
    let Some(bus) = sliver_testbus::TestBus::start() else {
        return;
    };
    let addr = bus.address().to_owned();
    let api = zbus::block_on(async {
        let conn = zbus::connection::Builder::address(addr.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        Api::serve(conn, Arc::new(NoHost), Arc::new(RealProc))
            .await
            .unwrap()
    });
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/python/pomodoro.py"
    );
    let _py = Kill(bus.command("python3").arg(script).spawn().unwrap());

    let wait = |f: &dyn Fn() -> bool| {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    assert!(
        wait(&|| label(&api).is_some_and(|l| l.starts_with('▶'))),
        "not published: {:?}",
        label(&api)
    );
    let id = api.state().reg.contexts().next().unwrap().id;
    assert_eq!(
        api.state().reg.context(id).unwrap().provider,
        "org.example.Pomodoro"
    );
    zbus::block_on(api.interact(id, "timer", Interaction::Gesture(Gesture::Tap))).unwrap();
    assert!(
        wait(&|| label(&api).is_some_and(|l| l.starts_with('⏸'))),
        "tap not handled: {:?}",
        label(&api)
    );
    zbus::block_on(api.interact(id, "timer", Interaction::Gesture(Gesture::Hold))).unwrap();
    assert!(
        wait(&|| label(&api).as_deref() == Some("▶ 25:00")),
        "hold not handled: {:?}",
        label(&api)
    );
}

/// T106: the browser integration's native-messaging host, fed the extension's messages on stdin
/// like a browser would, publishes tabs as a scrubber and sends taps back on stdout.
#[test]
fn browser_native_host() {
    use sliver_api::item::ItemKind;
    use sliver_api::registry::Scope;
    use std::io::{Read, Write};
    use std::process::Stdio;
    let gi = Command::new("python3")
        .args(["-c", "import gi; gi.require_version('Gio','2.0')"])
        .status();
    if !gi.is_ok_and(|s| s.success()) {
        eprintln!("python3-gobject not available; skipping");
        return;
    }
    let Some(bus) = sliver_testbus::TestBus::start() else {
        return;
    };
    let addr = bus.address().to_owned();
    let api = zbus::block_on(async {
        let conn = zbus::connection::Builder::address(addr.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        Api::serve(conn, Arc::new(NoHost), Arc::new(RealProc))
            .await
            .unwrap()
    });
    let host = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../integration/browser/host/sliver-browser-host"
    );
    let mut child = Kill(
        bus.command("python3")
            .arg(host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let frame = |v: serde_json::Value| {
        let data = serde_json::to_vec(&v).unwrap();
        let mut out = (data.len() as u32).to_ne_bytes().to_vec();
        out.extend(data);
        out
    };
    let mut stdin = child.0.stdin.take().unwrap();
    let mut stdout = child.0.stdout.take().unwrap();
    let tabs: Vec<serde_json::Value> = (0..8)
        .map(|i| serde_json::json!({"id": 100 + i, "title": format!("Tab {i}"), "active": i == 6}))
        .collect();
    stdin
        .write_all(&frame(serde_json::json!({"type": "tabs", "tabs": tabs})))
        .unwrap();
    stdin.flush().unwrap();

    let wait = |f: &dyn Fn() -> bool| {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    };
    let scrubber = || {
        let s = api.state();
        let ctx = s.reg.contexts().next()?;
        match &ctx.find("tabs")?.kind {
            ItemKind::Scrubber {
                candidates,
                selected,
                offset,
                ..
            } => Some((candidates.len(), *selected, *offset)),
            _ => None,
        }
    };
    assert!(wait(&|| scrubber() == Some((8, 6, 3))), "{:?}", scrubber());
    let id = {
        let s = api.state();
        let ctx = s.reg.contexts().next().unwrap();
        assert_eq!(ctx.provider, "io.github.wawow830.Sliver.Browser");
        assert!(matches!(ctx.opts.scope, Scope::App) && ctx.opts.include_children);
        ctx.id
    };
    let mut read_cmd = || {
        let mut len = [0u8; 4];
        stdout.read_exact(&mut len).unwrap();
        let mut data = vec![0; u32::from_ne_bytes(len) as usize];
        stdout.read_exact(&mut data).unwrap();
        serde_json::from_slice::<serde_json::Value>(&data).unwrap()
    };
    zbus::block_on(api.interact(id, "tabs", Interaction::Select(2))).unwrap();
    assert_eq!(
        read_cmd(),
        serde_json::json!({"cmd": "activate", "tab": 102})
    );
    zbus::block_on(api.interact(id, "back", Interaction::Gesture(Gesture::Tap))).unwrap();
    assert_eq!(read_cmd(), serde_json::json!({"cmd": "back"}));
    // The browser closes the port: the host exits (and its context goes with its connection).
    drop(stdin);
    let end = Instant::now() + Duration::from_secs(10);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "host didn't exit");
        std::thread::sleep(Duration::from_millis(50));
    }
}
