//! Keeps `examples/python/pomodoro.py` honest: run it against the real API server on a private
//! bus. Skipped when `dbus-daemon` or python3-gobject isn't available.

use sliver_api::identity::RealProc;
use sliver_api::item::Gesture;
use sliver_api::registry::Interaction;
use sliver_api::service::{Api, Control, ControlReply, ControlState, Host};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
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
    let Ok(mut daemon) = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
    else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let mut addr = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut addr)
        .unwrap();
    let _daemon = Kill(daemon);
    let addr = addr.trim().to_owned();
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
    let _py = Kill(
        Command::new("python3")
            .arg(script)
            .env("DBUS_SESSION_BUS_ADDRESS", &addr)
            .spawn()
            .unwrap(),
    );

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
