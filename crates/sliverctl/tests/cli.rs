//! The real `sliverctl` binary against the API server on a private `dbus-daemon`.
//! Skipped when `dbus-daemon` isn't installed.

use sliver_api::identity::RealProc;
use sliver_api::item::{Gesture, Prop};
use sliver_api::registry::Interaction;
use sliver_api::service::{Api, Control, ControlReply, ControlState, Host};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
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

#[derive(Default)]
struct Rec(Mutex<Vec<Control>>);
impl Host for Rec {
    fn changed(&self) {}
    fn control(&self, req: Control) -> Result<ControlReply, String> {
        let r = match &req {
            Control::ReloadConfig => ControlReply::Errors(vec!["bad.toml:1: nope".into()]),
            _ => ControlReply::Done,
        };
        self.0.lock().unwrap().push(req);
        Ok(r)
    }
    fn control_state(&self) -> ControlState {
        ControlState {
            active_layer: "base".into(),
            healthy: true,
            ..Default::default()
        }
    }
}

fn sliverctl(bus: &Bus) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_sliverctl"));
    c.env("DBUS_SESSION_BUS_ADDRESS", &bus.addr);
    sliver_testbus::die_with_parent(&mut c);
    c
}

fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn control_publish_and_events() {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let host = Arc::new(Rec::default());
    let h: Arc<dyn Host> = host.clone();
    let api = zbus::block_on(async {
        let conn = zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        Api::serve(conn, h, Arc::new(RealProc)).await.unwrap()
    });

    let ok = |args: &[&str]| {
        let o = sliverctl(&bus).args(args).output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    assert!(ok(&["signal", "RTMIN+8"]).0);
    assert!(ok(&["hold", "clock"]).0);
    assert!(ok(&["update", "custom/x", "text=hi there", "percentage=40"]).0);
    assert!(ok(&["var", "set", "focus", "true"]).0);
    let (success, _, err) = ok(&["reload"]);
    assert!(!success && err.contains("bad.toml:1: nope"));
    let (success, out, _) = ok(&["status"]);
    assert!(
        success && out.contains("ActiveLayer: base") && out.contains("Healthy: true"),
        "{out}"
    );
    let (success, _, err) = ok(&["frobnicate"]);
    assert!(!success && err.contains("unknown command"));
    {
        let reqs = host.0.lock().unwrap();
        assert_eq!(reqs[0], Control::Signal(8));
        assert_eq!(reqs[1], Control::Trigger("clock".into(), Gesture::Hold));
        let Control::Update(m, props) = &reqs[2] else {
            panic!("{:?}", reqs[2])
        };
        assert_eq!(m, "custom/x");
        assert_eq!(props.get("text"), Some(&Prop::Str("hi there".into())));
        assert_eq!(props.get("percentage"), Some(&Prop::Int(40)));
        assert_eq!(
            reqs[3],
            Control::SetVariable("focus".into(), Prop::Bool(true))
        );
    }

    // publish: events come out on stdout, updates go in on stdin.
    let mut child = sliverctl(&bus)
        .args([
            "publish",
            "--provider",
            "org.example.Cli",
            "--stdin",
            "button:go:Go",
            "toggle:t:T",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let ctx = || api.state().reg.contexts().next().map(|c| c.id);
    assert!(wait_until(|| ctx().is_some_and(|id| api
        .state()
        .reg
        .context(id)
        .unwrap()
        .find("t")
        .is_some())));
    let id = ctx().unwrap();
    assert_eq!(
        api.state().reg.context(id).unwrap().provider,
        "org.example.Cli"
    );

    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "go label='Going now'").unwrap();
    assert!(wait_until(|| api
        .state()
        .reg
        .context(id)
        .unwrap()
        .find("go")
        .unwrap()
        .label
        .as_deref()
        == Some("Going now")));

    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    zbus::block_on(api.interact(id, "go", Interaction::Gesture(Gesture::Tap))).unwrap();
    zbus::block_on(api.interact(id, "t", Interaction::Gesture(Gesture::Tap))).unwrap();
    assert_eq!(lines.next().unwrap().unwrap(), "go tap");
    assert_eq!(lines.next().unwrap().unwrap(), "t toggled state=true");

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(wait_until(|| ctx().is_none()), "context outlived sliverctl");
}
