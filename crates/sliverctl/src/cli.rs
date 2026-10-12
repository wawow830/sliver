//! Argument parsing (pure, unit-tested). See `sliverctl help` / [`USAGE`].

use std::collections::BTreeMap;

pub const USAGE: &str = "\
usage: sliverctl COMMAND [ARGS]

Inspect
  status                          agent health, layer, profile, display state
  modules                         every module: id, type, source, visibility
  contexts                        contexts published by apps and scripts

Control (waybar-style; MODULE is any module id, e.g. clock, custom/vpn, app/org.x.Y#item)
  reload                          reload config (prints errors)
  validate FILE                   check a config file without applying it
  signal N                        like `pkill -RTMIN+N waybar`
  refresh MODULE                  re-run one module now
  show|hide|toggle MODULE         runtime visibility (not saved)
  reset-visibility
  tap|hold|double-tap MODULE      fire a gesture's binding as if touched
  trigger MODULE GESTURE          tap, hold, swipe-left, swipe-right, double-tap, drag
  update MODULE KEY=VALUE...      push output to an exec-less custom module (text, icon, class, percentage, alt)
  layer switch|push|pop|toggle [NAME] [TIMEOUT_MS]
  layout NAME | profile NAME
  bar show|hide|toggle            the hatch (Fn Fn-hold) keeps working
  var set NAME VALUE              user variable for rules
  render [--layout FILE] -o OUT.png

Publish (stays connected; prints `ITEM KIND [KEY=VALUE...]` per event)
  publish [--scope app|status|target] [--provider ID] [--title T] [--target-app APP_ID]
          [--modal] [--stdin] ITEM...
      ITEM  = TYPE:ID[:LABEL]   TYPE = button toggle label slider progress spacer
      -p ID.KEY=VALUE           set an extra property (repeatable)
      --stdin                   read `ID KEY=VALUE...` lines and apply them as updates

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
pub struct ItemSpec {
    pub ty: String,
    pub id: String,
    pub label: Option<String>,
    pub props: KeyVals,
}

const TYPES: &[&str] = &[
    "button",
    "toggle",
    "label",
    "slider",
    "progress",
    "spacer",
    "segmented",
];

pub fn parse_item(s: &str) -> Result<ItemSpec, String> {
    let mut parts = s.splitn(3, ':');
    let ty = parts.next().unwrap_or_default();
    let id = parts
        .next()
        .filter(|i| !i.is_empty())
        .ok_or_else(|| format!("item {s:?}: expected TYPE:ID[:LABEL]"))?;
    if !TYPES.contains(&ty) {
        return Err(format!(
            "item {s:?}: unknown type {ty:?} (one of {})",
            TYPES.join(", ")
        ));
    }
    Ok(ItemSpec {
        ty: ty.into(),
        id: id.into(),
        label: parts.next().map(str::to_owned),
        props: KeyVals::new(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Publish {
    pub scope: String,
    pub provider: Option<String>,
    pub title: Option<String>,
    pub target_app: Option<String>,
    pub modal: bool,
    pub stdin: bool,
    pub items: Vec<ItemSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Help,
    Version,
    Status,
    Modules,
    Contexts,
    /// A `Control1` method with plain arguments.
    Call(Call),
    Render {
        layout: Option<String>,
        out: String,
    },
    Publish(Publish),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Reload,
    Validate(String),
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
        "contexts" => Ok(Cmd::Contexts),
        "reload" => c(Call::Reload),
        "validate" => {
            let path = one(rest, "FILE")?;
            let toml = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
            c(Call::Validate(toml))
        }
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
            let (mut layout, mut out) = (None, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--layout" => layout = Some(it.next().ok_or("--layout needs a FILE")?.clone()),
                    "-o" | "--output" => out = Some(it.next().ok_or("-o needs a FILE")?.clone()),
                    other => return Err(format!("unknown render option {other:?}")),
                }
            }
            Ok(Cmd::Render {
                layout,
                out: out.ok_or("render needs -o OUT.png")?,
            })
        }
        "publish" => parse_publish(rest).map(Cmd::Publish),
        other => Err(format!("unknown command {other:?} (try `sliverctl help`)")),
    }
}

fn parse_publish(args: &[String]) -> Result<Publish, String> {
    let mut p = Publish {
        scope: "status".into(),
        provider: None,
        title: None,
        target_app: None,
        modal: false,
        stdin: false,
        items: Vec::new(),
    };
    let mut extra: Vec<(String, String, Val)> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--scope" => p.scope = val("--scope")?,
            "--provider" => p.provider = Some(val("--provider")?),
            "--title" => p.title = Some(val("--title")?),
            "--target-app" => p.target_app = Some(val("--target-app")?),
            "--modal" => p.modal = true,
            "--stdin" => p.stdin = true,
            "-p" | "--prop" => {
                let spec = val("-p")?;
                let (lhs, v) = spec
                    .split_once('=')
                    .ok_or_else(|| format!("-p {spec:?}: expected ID.KEY=VALUE"))?;
                let (id, key) = lhs
                    .split_once('.')
                    .ok_or_else(|| format!("-p {spec:?}: expected ID.KEY=VALUE"))?;
                extra.push((id.into(), key.into(), parse_value(v)));
            }
            s if s.starts_with('-') => return Err(format!("unknown publish option {s:?}")),
            s => p.items.push(parse_item(s)?),
        }
    }
    if !["app", "status", "target"].contains(&p.scope.as_str()) {
        return Err(format!("unknown scope {:?}", p.scope));
    }
    if p.scope == "target" && p.target_app.is_none() {
        return Err("--scope target needs --target-app APP_ID".into());
    }
    if p.items.is_empty() {
        return Err("publish needs at least one ITEM".into());
    }
    for (id, key, v) in extra {
        let item = p
            .items
            .iter_mut()
            .find(|i| i.id == id)
            .ok_or_else(|| format!("-p: no item {id:?}"))?;
        item.props.insert(key, v);
    }
    Ok(p)
}

/// One line of `publish --stdin` input: `ID KEY=VALUE...`.
pub fn parse_update_line(line: &str) -> Result<Option<(String, KeyVals)>, String> {
    let words = shell_words(line)?;
    match words.split_first() {
        None => Ok(None),
        Some((id, kv)) if !kv.is_empty() => Ok(Some((id.clone(), parse_kv(kv)?))),
        Some(_) => Err(format!("expected `ID KEY=VALUE...`, got {line:?}")),
    }
}

/// Split on whitespace, honouring '…' and "…" so labels can contain spaces.
fn shell_words(line: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut quote, mut any) = (Vec::new(), String::new(), None, false);
    for ch in line.chars() {
        match (quote, ch) {
            (None, '\'' | '"') => (quote, any) = (Some(ch), true),
            (Some(q), c) if c == q => quote = None,
            (None, c) if c.is_whitespace() => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (_, c) => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if any {
        out.push(cur);
    }
    Ok(out)
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
                layout: None,
                out: "out.png".into()
            }
        );
    }

    #[test]
    fn publish() {
        let Cmd::Publish(pb) =
            p("publish --provider org.x.Y button:go:Go toggle:t -p go.icon=media-playback-start")
                .unwrap()
        else {
            panic!()
        };
        assert_eq!(pb.scope, "status");
        assert_eq!(pb.items[0].label.as_deref(), Some("Go"));
        assert_eq!(
            pb.items[0].props.get("icon"),
            Some(&Val::Str("media-playback-start".into()))
        );
        assert_eq!(pb.items[1].label, None);
        assert!(p("publish").is_err());
        assert!(p("publish hologram:x").is_err());
        assert!(p("publish --scope target button:x").is_err());
        assert!(p("publish button:x -p y.label=z").is_err());
        // Labels may contain colons.
        assert_eq!(
            parse_item("label:clock:12:30").unwrap().label.as_deref(),
            Some("12:30")
        );
    }

    #[test]
    fn update_lines() {
        let (id, kv) = parse_update_line(r#"go label="⏸ 24:59" value=0.5"#)
            .unwrap()
            .unwrap();
        assert_eq!(id, "go");
        assert_eq!(kv.get("label"), Some(&Val::Str("⏸ 24:59".into())));
        assert_eq!(kv.get("value"), Some(&Val::Num(0.5)));
        assert_eq!(parse_update_line("   ").unwrap(), None);
        assert!(parse_update_line("go").is_err());
        assert!(parse_update_line("go label='x").is_err());
    }
}
