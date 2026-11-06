//! A custom module's runtime settings, from its `[modules."custom/x"]` block.

use sliver_config::{Interval, ModuleInstance, parse_duration};
use std::time::Duration;
use toml::Value;

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
/// Highest `signal` offset (glibc has 30 real-time signals available to applications).
pub const MAX_SIGNAL: u8 = 30;

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

    /// Read the runtime keys of a resolved `custom/*` module.
    pub fn from_instance(m: &ModuleInstance) -> Result<Spec, String> {
        let t = &m.config;
        let exec = match t.get("exec") {
            None => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
            Some(_) => return Err("exec must be a non-empty string".into()),
        };
        let string = |k: &str| match t.get(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("{k} must be a string")),
        };
        let flag = |k: &str, default: bool| match t.get(k) {
            None => Ok(default),
            Some(Value::Boolean(b)) => Ok(*b),
            Some(_) => Err(format!("{k} must be true or false")),
        };
        let dur = |k: &str| {
            t.get(k)
                .map(parse_duration)
                .transpose()
                .map_err(|e| format!("{k}: {e}"))
        };
        let signal = match t.get("signal") {
            None => None,
            Some(Value::Integer(n)) if (1..=i64::from(MAX_SIGNAL)).contains(n) => Some(*n as u8),
            Some(_) => return Err(format!("signal must be an integer from 1 to {MAX_SIGNAL}")),
        };
        let json = match string("return-type")?.as_deref() {
            None | Some("") => false,
            Some("json") => true,
            Some(other) => {
                return Err(format!(
                    "return-type {other:?} (use \"json\" or leave it empty)"
                ));
            }
        };
        let mode = match (m.common.interval, signal) {
            (Some(Interval::Every(d)), _) => Mode::Every(d),
            (Some(Interval::Once), _) => Mode::Once,
            (None, Some(_)) => Mode::Once,
            (None, None) => Mode::Continuous,
        };
        let env = match t.get("env") {
            None => Vec::new(),
            Some(Value::Table(e)) => e
                .iter()
                .map(|(k, v)| match v {
                    Value::String(s) => Ok((k.clone(), s.clone())),
                    Value::Integer(n) => Ok((k.clone(), n.to_string())),
                    _ => Err(format!("env.{k} must be a string")),
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("env must be a table".into()),
        };
        Ok(Spec {
            key: m.id.to_string(),
            exec,
            exec_if: string("exec-if")?,
            mode,
            restart_interval: dur("restart-interval")?
                .unwrap_or(MIN_RESTART)
                .max(MIN_RESTART),
            signal,
            exec_on_event: flag("exec-on-event", true)?,
            json,
            escape: flag("escape", false)?,
            timeout: dur("timeout")?
                .unwrap_or(DEFAULT_TIMEOUT)
                .max(Duration::from_millis(50)),
            env,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_of(block: &str) -> Result<Spec, String> {
        let src = format!(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"custom/x\"]\n[modules.\"custom/x\"]\n{block}\n"
        );
        let loaded = sliver_config::load_str(&src, std::path::Path::new("."));
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
        Spec::from_instance(m)
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
    fn bad_values() {
        assert!(spec_of("exec = 3").is_err());
        assert!(spec_of("exec = \"a\"\nsignal = 99").is_err());
        assert!(spec_of("exec = \"a\"\nreturn-type = \"xml\"").is_err());
        assert!(spec_of("exec = \"a\"\nenv = 3").is_err());
    }
}
