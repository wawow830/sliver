//! The real `sliverctl` binary against the agent's `Control1` server on a private
//! `dbus-daemon`. Skipped when `dbus-daemon` isn't installed.

use sliver_agent::api::{Api, Control, ControlReply, ControlState, Host, Prop};
use sliver_config::Gesture;
use std::process::Command;
use std::sync::{Arc, Mutex};

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
            errors: vec!["config.toml:7: modules.\"custom/vpn\": exec: no such file".into()],
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

#[test]
fn control_calls_reach_the_agent() {
    let Some(bus) = Bus::start() else {
        eprintln!("dbus-daemon not installed; skipping");
        return;
    };
    let host = Arc::new(Rec::default());
    let h: Arc<dyn Host> = host.clone();
    let _api = zbus::block_on(async {
        let conn = zbus::connection::Builder::address(bus.addr.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        Api::serve(conn, h, false).await.unwrap()
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
    assert!(
        out.contains("Errors: 1\n  config.toml:7: modules.\"custom/vpn\": exec: no such file"),
        "status lists the agent's current errors: {out}"
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
}
