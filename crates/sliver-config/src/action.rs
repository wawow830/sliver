//! What a gesture does (customization.md §5).

use std::time::Duration;

use toml::{Table, Value};

use crate::diag::{Diagnostics, Reader};
use crate::keys::Key;
use crate::values::parse_duration;

/// Macro caps (customization.md §5).
pub const MAX_MACRO_STEPS: usize = 32;
pub const MAX_MACRO_TIME: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerMode {
    /// Shown while the finger stays down.
    Hold,
    Toggle,
    /// Back to the previous layer after the next action.
    Oneshot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DBusCall {
    pub system_bus: bool,
    pub dest: String,
    pub path: String,
    pub interface: String,
    pub method: String,
    /// Arguments, passed through as TOML values. The agent maps them to D-Bus types.
    pub args: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// `sh -c` in the user session (waybar compat). `{value}` is substituted for `on-drag`.
    Shell(String),
    /// argv, no shell.
    Exec(Vec<String>),
    Key(Key),
    /// Pressed in order, released in reverse.
    Keys(Vec<Key>),
    Layer {
        name: String,
        mode: LayerMode,
    },
    CloseLayer,
    Layout(String),
    Open(String),
    Url(String),
    DBus(DBusCall),
    Signal(u32),
    Update(String),
    ToggleAlt,
    ModuleAction(String),
    /// Only valid inside a macro.
    Sleep(Duration),
    Macro(Vec<Action>),
}

impl Action {
    pub fn parse(v: &Value, path: &str, d: &mut Diagnostics) -> Option<Action> {
        Self::parse_inner(v, path, d, false)
    }

    fn parse_inner(v: &Value, path: &str, d: &mut Diagnostics, in_macro: bool) -> Option<Action> {
        match v {
            Value::String(s) if s.trim().is_empty() => {
                d.error(path, "empty command");
                None
            }
            Value::String(s) => Some(Action::Shell(s.clone())),
            Value::Array(steps) if in_macro => {
                d.error(path, "macros can't be nested");
                let _ = steps;
                None
            }
            Value::Array(steps) => {
                if steps.len() > MAX_MACRO_STEPS {
                    d.error(
                        path,
                        format!(
                            "macro has {} steps; the limit is {MAX_MACRO_STEPS}",
                            steps.len()
                        ),
                    );
                    return None;
                }
                let actions: Vec<Action> = steps
                    .iter()
                    .enumerate()
                    .filter_map(|(i, s)| Self::parse_inner(s, &format!("{path}[{i}]"), d, true))
                    .collect();
                let total: Duration = actions
                    .iter()
                    .map(|a| {
                        if let Action::Sleep(t) = a {
                            *t
                        } else {
                            Duration::ZERO
                        }
                    })
                    .sum();
                if total > MAX_MACRO_TIME {
                    d.error(
                        path,
                        format!("macro sleeps {total:?} in total; the limit is 10s"),
                    );
                    return None;
                }
                (actions.len() == steps.len()).then_some(Action::Macro(actions))
            }
            Value::Table(t) => Self::parse_table(t, path, d, in_macro),
            other => {
                d.error(
                    path,
                    format!(
                        "expected a command string, action table or array, got {}",
                        other.type_str()
                    ),
                );
                None
            }
        }
    }

    fn parse_table(t: &Table, path: &str, d: &mut Diagnostics, in_macro: bool) -> Option<Action> {
        const FORMS: &[&str] = &[
            "exec",
            "key",
            "keys",
            "layer",
            "close-layer",
            "layout",
            "open",
            "url",
            "dbus",
            "signal",
            "update",
            "format",
            "module-action",
            "sleep",
        ];
        let forms: Vec<&str> = FORMS
            .iter()
            .copied()
            .filter(|k| t.contains_key(*k))
            .collect();
        let [form] = forms.as_slice() else {
            d.error(
                path,
                if forms.is_empty() {
                    format!("unknown action; use one of {}", FORMS.join(", "))
                } else {
                    format!(
                        "an action table needs exactly one of {}, found {}",
                        FORMS.join(", "),
                        forms.join(" and ")
                    )
                },
            );
            return None;
        };
        let mut r = Reader::new(t, path);
        let key_path = r.path(form);
        let action = match *form {
            "exec" => {
                let argv = r.strings("exec", d);
                if argv.is_empty() {
                    d.error(key_path, "exec needs at least the program");
                    None
                } else {
                    Some(Action::Exec(argv))
                }
            }
            "key" => r
                .string("key", d)
                .and_then(|s| parse_key(&s, &key_path, d))
                .map(Action::Key),
            "keys" => {
                let names = r.strings("keys", d);
                let keys: Vec<Key> = names
                    .iter()
                    .filter_map(|s| parse_key(s, &key_path, d))
                    .collect();
                if keys.is_empty() {
                    d.error(key_path, "keys needs at least one key");
                    None
                } else {
                    (keys.len() == names.len()).then_some(Action::Keys(keys))
                }
            }
            "layer" => {
                let name = r.string("layer", d);
                let mode = match r.string("mode", d).as_deref() {
                    None | Some("toggle") => Some(LayerMode::Toggle),
                    Some("hold") => Some(LayerMode::Hold),
                    Some("oneshot") => Some(LayerMode::Oneshot),
                    Some(m) => {
                        d.error(
                            r.path("mode"),
                            format!("bad layer mode {m:?} (hold, toggle, oneshot)"),
                        );
                        None
                    }
                };
                name.zip(mode)
                    .map(|(name, mode)| Action::Layer { name, mode })
            }
            "close-layer" => r.bool("close-layer", d).map(|_| Action::CloseLayer),
            "layout" => r.string("layout", d).map(Action::Layout),
            "open" => r.string("open", d).map(Action::Open),
            "url" => r.string("url", d).map(Action::Url),
            "dbus" => r
                .table("dbus", d)
                .and_then(|t| parse_dbus(t, &key_path, d))
                .map(Action::DBus),
            "signal" => r.uint("signal", 64, d).map(|n| Action::Signal(n as u32)),
            "update" => r.string("update", d).map(Action::Update),
            "format" => match r.string("format", d).as_deref() {
                Some("toggle-alt") => Some(Action::ToggleAlt),
                Some(other) => {
                    d.error(
                        key_path,
                        format!("unknown format action {other:?} (toggle-alt)"),
                    );
                    None
                }
                None => None,
            },
            "module-action" => r.string("module-action", d).map(Action::ModuleAction),
            "sleep" => {
                if !in_macro {
                    d.error(
                        key_path.clone(),
                        "sleep only makes sense inside a macro (an array of actions)",
                    );
                }
                r.get("sleep")
                    .and_then(|v| parse_duration(v).map_err(|e| d.error(key_path, e)).ok())
                    .map(Action::Sleep)
            }
            _ => unreachable!(),
        };
        r.finish(d);
        action
    }

    /// Every key this action presses, for F-key coverage checks.
    pub fn keys(&self) -> Vec<Key> {
        match self {
            Action::Key(k) => vec![*k],
            Action::Keys(ks) => ks.clone(),
            Action::Macro(steps) => steps.iter().flat_map(Action::keys).collect(),
            _ => Vec::new(),
        }
    }
}

fn parse_key(s: &str, path: &str, d: &mut Diagnostics) -> Option<Key> {
    s.parse().map_err(|e: String| d.error(path, e)).ok()
}

fn parse_dbus(t: &Table, path: &str, d: &mut Diagnostics) -> Option<DBusCall> {
    let mut r = Reader::new(t, path);
    let system_bus = match r.string("bus", d).as_deref() {
        None | Some("session") => false,
        Some("system") => true,
        Some(b) => {
            d.error(r.path("bus"), format!("bad bus {b:?} (session or system)"));
            false
        }
    };
    let req = |k: &'static str, r: &mut Reader, d: &mut Diagnostics| {
        let v = r.string(k, d);
        if v.is_none() {
            d.error(r.path(k), "required");
        }
        v
    };
    let dest = req("dest", &mut r, d);
    let obj = req("path", &mut r, d);
    let interface = req("interface", &mut r, d);
    let method = req("method", &mut r, d);
    let args = match r.get("args") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => {
            d.error(r.path("args"), "args must be an array");
            Vec::new()
        }
    };
    r.finish(d);
    Some(DBusCall {
        system_bus,
        dest: dest?,
        path: obj?,
        interface: interface?,
        method: method?,
        args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> (Option<Action>, Diagnostics) {
        let t: Table = format!("a = {src}").parse().unwrap();
        let mut d = Diagnostics::default();
        (Action::parse(&t["a"], "a", &mut d), d)
    }

    #[test]
    fn forms() {
        assert_eq!(
            parse(r#""pavucontrol""#).0,
            Some(Action::Shell("pavucontrol".into()))
        );
        assert_eq!(
            parse(r#"{ key = "F5" }"#).0,
            Some(Action::Key(Key::function(5).unwrap()))
        );
        let (a, d) = parse(r#"{ keys = ["LeftCtrl", "LeftAlt", "F1"] }"#);
        assert!(d.ok(), "{d:?}");
        assert!(matches!(a, Some(Action::Keys(k)) if k.len() == 3));
        assert_eq!(
            parse(r#"{ layer = "media", mode = "hold" }"#).0,
            Some(Action::Layer {
                name: "media".into(),
                mode: LayerMode::Hold
            })
        );
        assert_eq!(
            parse(r#"{ format = "toggle-alt" }"#).0,
            Some(Action::ToggleAlt)
        );
        let (a, d) =
            parse(r#"[{ keys = ["LeftCtrl", "C"] }, { sleep = "100ms" }, "notify-send copied"]"#);
        assert!(d.ok(), "{d:?}");
        assert!(matches!(a, Some(Action::Macro(s)) if s.len() == 3));
    }

    #[test]
    fn errors() {
        assert!(!parse(r#"{ key = "NoSuchKey" }"#).1.ok());
        assert!(!parse(r#"{ key = "F1", url = "x" }"#).1.ok());
        assert!(!parse(r#"{ sleep = "1s" }"#).1.ok());
        assert!(!parse(r#"[{ sleep = "11s" }]"#).1.ok());
        assert!(!parse(r#"{ dbus = { dest = "a" } }"#).1.ok());
        assert!(!parse(r#""""#).1.ok());
    }
}
