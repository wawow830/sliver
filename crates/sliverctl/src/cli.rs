//! Argument parsing. See `sliverctl help` / [`USAGE`].

use std::collections::BTreeMap;

pub const USAGE: &str = "\
usage: sliverctl COMMAND [ARGS]

Inspect
  status                          agent health, layer, layout, profile, errors
  modules                         every module on the bar: id, key, type, layer, classes, x, width
  doctor [--json]                 check the whole install; says what to fix (exit 1 on a failure)

Control (waybar-style; MODULE is any module id, e.g. clock, custom/vpn)
  reload                          reload config (prints errors)
  validate FILE                   check a config file without applying it (= config check FILE)
  signal N                        like `pkill -RTMIN+N waybar`
  refresh MODULE                  re-run one module now
  show|hide|toggle MODULE         runtime visibility (not saved)
  reset-visibility
  tap|hold|double-tap MODULE      fire a gesture's binding as if touched
  trigger MODULE GESTURE          tap, hold, swipe-left, swipe-right, double-tap
  update MODULE KEY=VALUE...      push output to an exec-less custom module (text, icon, class, percentage, alt)
  layer switch|push|pop|toggle [NAME] [TIMEOUT_MS]
  layout NAME | profile NAME
  bar show|hide|toggle            the hatch (Fn Fn-hold) keeps working
  var set NAME VALUE              user variable for rules
  render [--config FILE] -o OUT.png  the bar as it is, or FILE drawn with the live state

Config (offline: edits ~/.config/sliver/config.toml, keeps comments; `sliverctl config help`)
  config path|check|get|set|unset|resolve|profile ...

Values: true/false -> bool, integers -> int, decimals -> double, anything else -> string
(prefix with s: to force a string, e.g. text=s:42).
";

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Bool(bool),
    Int(i32),
    Num(f64),
    Str(String),
}

pub fn parse_value(s: &str) -> Val {
    if let Some(rest) = s.strip_prefix("s:") {
        return Val::Str(rest.to_owned());
    }
    match s {
        "true" => return Val::Bool(true),
        "false" => return Val::Bool(false),
        _ => {}
    }
    if let Ok(i) = s.parse::<i32>() {
        return Val::Int(i);
    }
    if s.contains('.')
        && let Ok(f) = s.parse::<f64>()
        && f.is_finite()
    {
        return Val::Num(f);
    }
    Val::Str(s.to_owned())
}

pub type KeyVals = BTreeMap<String, Val>;

pub fn parse_kv(args: &[String]) -> Result<KeyVals, String> {
    args.iter()
        .map(|a| {
            let (k, v) = a
                .split_once('=')
                .ok_or_else(|| format!("expected KEY=VALUE, got {a:?}"))?;
            if k.is_empty() {
                return Err(format!("empty key in {a:?}"));
            }
            Ok((k.to_owned(), parse_value(v)))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Help,
    Version,
    Status,
    Modules,
    /// A `Control1` method with plain arguments.
    Call(Call),
    Render {
        config: Option<String>,
        out: String,
    },
    /// `config …`: offline, handled by `config::run`.
    Config(Vec<String>),
    Doctor(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Reload,
    Signal(u32),
    Refresh(String),
    Show(String),
    Hide(String),
    Toggle(String),
    ResetVisibility,
    Trigger(String, String),
    Update(String, KeyVals),
    SwitchLayer(String),
    PushLayer(String, u32),
    PopLayer,
    ToggleLayer(String),
    SetLayout(String),
    SetProfile(String),
    Bar(String),
    SetVariable(String, Val),
}

fn one(args: &[String], what: &str) -> Result<String, String> {
    match args {
        [a] => Ok(a.clone()),
        _ => Err(format!("expected exactly one {what}")),
    }
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some((cmd, rest)) = args.split_first() else {
        return Ok(Cmd::Help);
    };
    let c = |call| Ok(Cmd::Call(call));
    match cmd.as_str() {
        "help" | "-h" | "--help" => Ok(Cmd::Help),
        "version" | "-V" | "--version" => Ok(Cmd::Version),
        "status" => Ok(Cmd::Status),
        "modules" => Ok(Cmd::Modules),
        "config" => Ok(Cmd::Config(rest.to_vec())),
        "doctor" => Ok(Cmd::Doctor(rest.to_vec())),
        "reload" => c(Call::Reload),
        "validate" => Ok(Cmd::Config(vec!["check".into(), one(rest, "FILE")?])),
        "signal" => {
            let n = one(rest, "N")?;
            let n = n
                .trim_start_matches("RTMIN+")
                .parse()
                .map_err(|_| format!("bad signal number {n:?}"))?;
            c(Call::Signal(n))
        }
        "refresh" => c(Call::Refresh(one(rest, "MODULE")?)),
        "show" => c(Call::Show(one(rest, "MODULE")?)),
        "hide" => c(Call::Hide(one(rest, "MODULE")?)),
        "toggle" => c(Call::Toggle(one(rest, "MODULE")?)),
        "reset-visibility" => c(Call::ResetVisibility),
        "tap" | "hold" | "double-tap" => c(Call::Trigger(one(rest, "MODULE")?, cmd.clone())),
        "trigger" => match rest {
            [m, g] => c(Call::Trigger(m.clone(), g.clone())),
            _ => Err("usage: trigger MODULE GESTURE".into()),
        },
        "update" => match rest {
            [m, kv @ ..] if !kv.is_empty() => c(Call::Update(m.clone(), parse_kv(kv)?)),
            _ => Err("usage: update MODULE KEY=VALUE...".into()),
        },
        "layer" => match rest {
            [op, name] if op == "switch" => c(Call::SwitchLayer(name.clone())),
            [op, name] if op == "toggle" => c(Call::ToggleLayer(name.clone())),
            [op, name] if op == "push" => c(Call::PushLayer(name.clone(), 0)),
            [op, name, ms] if op == "push" => c(Call::PushLayer(
                name.clone(),
                ms.parse().map_err(|_| format!("bad timeout {ms:?}"))?,
            )),
            [op] if op == "pop" => c(Call::PopLayer),
            _ => Err("usage: layer switch|push|pop|toggle [NAME] [TIMEOUT_MS]".into()),
        },
        "layout" => c(Call::SetLayout(one(rest, "NAME")?)),
        "profile" => c(Call::SetProfile(one(rest, "NAME")?)),
        "bar" => match rest {
            [a] if ["show", "hide", "toggle"].contains(&a.as_str()) => c(Call::Bar(a.clone())),
            _ => Err("usage: bar show|hide|toggle".into()),
        },
        "var" => match rest {
            [op, name, value] if op == "set" => {
                c(Call::SetVariable(name.clone(), parse_value(value)))
            }
            _ => Err("usage: var set NAME VALUE".into()),
        },
        "render" => {
            let (mut config, mut out) = (None, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--config" => config = Some(it.next().ok_or("--config needs a FILE")?.clone()),
                    "-o" | "--output" => out = Some(it.next().ok_or("-o needs a FILE")?.clone()),
                    other => return Err(format!("unknown render option {other:?}")),
                }
            }
            Ok(Cmd::Render {
                config,
                out: out.ok_or("render needs -o OUT.png")?,
            })
        }
        other => Err(format!("unknown command {other:?} (try `sliverctl help`)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Cmd, String> {
        parse(&s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
    }

    #[test]
    fn values() {
        assert_eq!(parse_value("true"), Val::Bool(true));
        assert_eq!(parse_value("42"), Val::Int(42));
        assert_eq!(parse_value("0.5"), Val::Num(0.5));
        assert_eq!(parse_value("s:42"), Val::Str("42".into()));
        assert_eq!(parse_value("nan"), Val::Str("nan".into()));
        assert_eq!(parse_value("build ok"), Val::Str("build ok".into()));
    }

    #[test]
    fn control_commands() {
        assert_eq!(p("signal 8").unwrap(), Cmd::Call(Call::Signal(8)));
        assert_eq!(p("signal RTMIN+8").unwrap(), Cmd::Call(Call::Signal(8)));
        assert_eq!(
            p("hold clock").unwrap(),
            Cmd::Call(Call::Trigger("clock".into(), "hold".into()))
        );
        assert_eq!(
            p("layer push media 3000").unwrap(),
            Cmd::Call(Call::PushLayer("media".into(), 3000))
        );
        assert_eq!(p("layer pop").unwrap(), Cmd::Call(Call::PopLayer));
        assert_eq!(
            p("var set focus true").unwrap(),
            Cmd::Call(Call::SetVariable("focus".into(), Val::Bool(true)))
        );
        assert_eq!(
            p("update custom/status text=ok class=good").unwrap(),
            Cmd::Call(Call::Update(
                "custom/status".into(),
                KeyVals::from([
                    ("class".into(), Val::Str("good".into())),
                    ("text".into(), Val::Str("ok".into()))
                ])
            ))
        );
        assert!(p("bar sideways").is_err());
        assert!(p("update x").is_err());
        assert!(p("frobnicate").is_err());
        assert_eq!(p("").unwrap(), Cmd::Help);
        assert_eq!(
            p("render -o out.png").unwrap(),
            Cmd::Render {
                config: None,
                out: "out.png".into()
            }
        );
    }
}
