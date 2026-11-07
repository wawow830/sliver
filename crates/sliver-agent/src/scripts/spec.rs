//! A custom module's runtime settings, from its `[modules."custom/x"]` block.

use sliver_config::{Interval, ModuleInstance};
use std::time::Duration;

/// When a module's command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Once at start (and on signal/trigger).
    Once,
    /// Every period.
    Every(Duration),
    /// A long-running process; each stdout line is an update.
    Continuous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// `custom/vpn` (with `#instance` if any): what `update MODULE` and `SLIVER_MODULE` use.
    pub key: String,
    /// `None`: a static module (just `format`), nothing to run.
    pub exec: Option<String>,
    pub exec_if: Option<String>,
    pub mode: Mode,
    pub restart_interval: Duration,
    /// Real-time signal offset: `pkill -RTMIN+N`, `sliverctl signal N`.
    pub signal: Option<u8>,
    pub exec_on_event: bool,
    pub json: bool,
    pub escape: bool,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
}

pub const MIN_RESTART: Duration = Duration::from_secs(1);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);
pub const MAX_SIGNAL: u8 = sliver_config::MAX_SIGNAL as u8;

impl Spec {
    /// A run-once spec, for tests and tools.
    pub fn new(key: impl Into<String>, exec: impl Into<String>) -> Spec {
        Spec {
            key: key.into(),
            exec: Some(exec.into()),
            exec_if: None,
            mode: Mode::Once,
            restart_interval: MIN_RESTART,
            signal: None,
            exec_on_event: true,
            json: false,
            escape: false,
            timeout: DEFAULT_TIMEOUT,
            env: Vec::new(),
        }
    }

    /// The runtime keys of a resolved module with a script (`custom`, `image`, `graph`).
    pub fn from_instance(m: &ModuleInstance) -> Spec {
        let signal = m.int("signal").map(|n| n as u8);
        let mode = match (m.common.interval, signal) {
            (Some(Interval::Every(d)), _) => Mode::Every(d),
            (Some(Interval::Once), _) => Mode::Once,
            (None, Some(_)) => Mode::Once,
            (None, None) => Mode::Continuous,
        };
        Spec {
            key: m.id.to_string(),
            exec: m
                .text("exec")
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned),
            exec_if: m.text("exec-if").map(str::to_owned),
            mode,
            restart_interval: m.duration("restart-interval").unwrap_or(MIN_RESTART),
            signal,
            exec_on_event: m.flag("exec-on-event").unwrap_or(true),
            json: m.text("return-type") == Some("json"),
            escape: m.flag("escape").unwrap_or(false),
            timeout: m.duration("timeout").unwrap_or(DEFAULT_TIMEOUT),
            env: m.env("env").to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_of(block: &str) -> Option<Spec> {
        let src = format!(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"custom/x\"]\n[modules.\"custom/x\"]\n{block}\n"
        );
        let loaded = sliver_config::load_str(&src, std::path::Path::new("."));
        let hw = sliver_config::Hardware { physical_esc: true };
        (loaded.diagnostics.ok() && loaded.config.validate(hw).ok()).then_some(())?;
        let r = loaded
            .config
            .resolve("base", &sliver_config::Context::new())
            .unwrap();
        let m = r
            .zones
            .iter()
            .flat_map(|z| &z.modules)
            .find(|m| m.id.kind == "custom")
            .unwrap();
        Some(Spec::from_instance(m))
    }

    #[test]
    fn modes() {
        assert_eq!(spec_of("exec = \"a\"").unwrap().mode, Mode::Continuous);
        assert_eq!(
            spec_of("exec = \"a\"\ninterval = \"5s\"").unwrap().mode,
            Mode::Every(Duration::from_secs(5))
        );
        assert_eq!(
            spec_of("exec = \"a\"\ninterval = \"once\"").unwrap().mode,
            Mode::Once
        );
        assert_eq!(
            spec_of("exec = \"a\"\nsignal = 8").unwrap().mode,
            Mode::Once
        );
    }

    #[test]
    fn keys() {
        let s = spec_of(
            "exec = \"cmd\"\nexec-if = \"test -f x\"\ninterval = 2\nsignal = 8\nreturn-type = \"json\"\nescape = true\nexec-on-event = false\ntimeout = \"500ms\"\nrestart-interval = \"10ms\"\nenv = { A = \"1\", N = 2 }",
        )
        .unwrap();
        assert_eq!(s.key, "custom/x");
        assert_eq!(
            (s.exec.as_deref(), s.exec_if.as_deref()),
            (Some("cmd"), Some("test -f x"))
        );
        assert_eq!(
            (s.signal, s.json, s.escape, s.exec_on_event),
            (Some(8), true, true, false)
        );
        assert_eq!(s.timeout, Duration::from_millis(500));
        assert_eq!(s.restart_interval, MIN_RESTART, "restart floor");
        assert_eq!(
            s.env,
            [
                ("A".to_string(), "1".to_string()),
                ("N".to_string(), "2".to_string())
            ]
        );
        let d = spec_of("format = \"hi\"").unwrap();
        assert_eq!(
            (d.exec, d.timeout, d.exec_on_event),
            (None, DEFAULT_TIMEOUT, true)
        );
    }

    #[test]
    fn bad_values_fail_to_load() {
        for bad in [
            "exec = 3",
            "exec = \"a\"\nsignal = 99",
            "exec = \"a\"\nreturn-type = \"xml\"",
            "exec = \"a\"\nenv = 3",
        ] {
            assert!(spec_of(bad).is_none(), "{bad}");
        }
    }
}
