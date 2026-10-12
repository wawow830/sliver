//! `sliverctl`: command-line client of the public sliver session D-Bus API (`docs/api.md`).

mod cli;

use cli::{Call, Cmd, ItemSpec, KeyVals, Publish, Val};
use sliver_client::{Client, ContextOptions, Item};
use sliver_ipc::dbus::{BUS_NAME, IFACE_CONTROL, IFACE_TOUCHBAR, ROOT_PATH};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};
use std::process::ExitCode;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sliverctl: {e}");
            return ExitCode::from(2);
        }
    };
    match run(cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sliverctl: {e}");
            ExitCode::FAILURE
        }
    }
}

fn describe(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, msg, _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
        {
            let _ = msg;
            "sliver-agent isn't running (systemctl --user start sliver-agent)".into()
        }
        zbus::Error::MethodError(name, msg, _) => {
            let short = name
                .as_str()
                .rsplit('.')
                .next()
                .unwrap_or(name.as_str())
                .to_owned();
            format!("{short}: {}", msg.unwrap_or_default())
        }
        e => e.to_string(),
    }
}

fn value(v: &Val) -> Value<'static> {
    match v {
        Val::Bool(b) => Value::from(*b),
        Val::Int(i) => Value::from(*i),
        Val::Num(n) => Value::from(*n),
        Val::Str(s) => Value::from(s.clone()),
    }
}

fn owned(v: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("plain values")
}

fn dict(kv: &KeyVals) -> HashMap<String, OwnedValue> {
    kv.iter()
        .map(|(k, v)| (k.clone(), owned(value(v))))
        .collect()
}

/// Human-readable D-Bus value.
fn show(v: &Value<'_>) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::U8(x) => x.to_string(),
        Value::I16(x) => x.to_string(),
        Value::U16(x) => x.to_string(),
        Value::I32(x) => x.to_string(),
        Value::U32(x) => x.to_string(),
        Value::I64(x) => x.to_string(),
        Value::U64(x) => x.to_string(),
        Value::F64(x) => x.to_string(),
        Value::Str(s) => {
            let s = s.as_str();
            if s.is_empty() || s.contains(char::is_whitespace) {
                format!("{s:?}")
            } else {
                s.to_owned()
            }
        }
        Value::ObjectPath(p) => p.to_string(),
        Value::Value(inner) => show(inner),
        Value::Array(a) => format!(
            "[{}]",
            a.inner().iter().map(show).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

fn show_dict(d: &HashMap<String, OwnedValue>) -> String {
    let sorted: BTreeMap<_, _> = d.iter().collect();
    sorted
        .iter()
        .map(|(k, v)| format!("{k}={}", show(v)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn control<B>(conn: &Connection, member: &str, body: &B) -> Result<zbus::Message, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(Some(BUS_NAME), ROOT_PATH, Some(IFACE_CONTROL), member, body)
        .map_err(describe)
}

fn property(conn: &Connection, iface: &str, name: &str) -> Result<OwnedValue, String> {
    let props = zbus::blocking::fdo::PropertiesProxy::builder(conn)
        .destination(BUS_NAME)
        .and_then(|b| b.path(ROOT_PATH))
        .and_then(|b| b.build())
        .map_err(describe)?;
    props
        .get(
            iface
                .try_into()
                .map_err(|e: zbus::names::Error| e.to_string())?,
            name,
        )
        .map_err(|e| describe(e.into()))
}

fn run(cmd: Cmd) -> Result<ExitCode, String> {
    match cmd {
        Cmd::Help => {
            print!("{}", cli::USAGE);
            return Ok(ExitCode::SUCCESS);
        }
        Cmd::Version => {
            println!("sliverctl {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }
    let conn = Connection::session().map_err(|e| format!("session bus: {e}"))?;
    match cmd {
        Cmd::Help | Cmd::Version => unreachable!(),
        Cmd::Status => {
            for (iface, name) in [
                (IFACE_CONTROL, "Healthy"),
                (IFACE_CONTROL, "ActiveLayer"),
                (IFACE_CONTROL, "Layers"),
                (IFACE_CONTROL, "ActiveProfile"),
                (IFACE_CONTROL, "Profiles"),
                (IFACE_TOUCHBAR, "DisplayState"),
                (IFACE_TOUCHBAR, "Version"),
            ] {
                let v = property(&conn, iface, name)?;
                println!("{name}: {}", show(&v));
            }
        }
        Cmd::Modules | Cmd::Contexts => {
            let member = if cmd == Cmd::Modules {
                "ListModules"
            } else {
                "ListContexts"
            };
            let list: Vec<HashMap<String, OwnedValue>> = control(&conn, member, &())?
                .body()
                .deserialize()
                .map_err(|e| e.to_string())?;
            for d in &list {
                println!("{}", show_dict(d));
            }
        }
        Cmd::Render { layout, out } => {
            let toml = match &layout {
                Some(f) => std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?,
                None => String::new(),
            };
            let png: Vec<u8> = control(
                &conn,
                "RenderPreview",
                &(toml, HashMap::<String, OwnedValue>::new()),
            )?
            .body()
            .deserialize()
            .map_err(|e| e.to_string())?;
            std::fs::write(&out, png).map_err(|e| format!("{out}: {e}"))?;
        }
        Cmd::Call(call) => return call_control(&conn, call),
        Cmd::Publish(p) => return publish(conn, p),
    }
    Ok(ExitCode::SUCCESS)
}

fn call_control(conn: &Connection, call: Call) -> Result<ExitCode, String> {
    let errors = |m: zbus::Message| -> Result<ExitCode, String> {
        let errs: Vec<String> = m.body().deserialize().map_err(|e| e.to_string())?;
        for e in &errs {
            eprintln!("{e}");
        }
        Ok(if errs.is_empty() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    };
    match call {
        Call::Reload => return errors(control(conn, "ReloadConfig", &())?),
        Call::Validate(toml) => return errors(control(conn, "ValidateConfig", &(toml,))?),
        Call::Signal(n) => control(conn, "Signal", &(n,))?,
        Call::Refresh(m) => control(conn, "Refresh", &(m,))?,
        Call::Show(m) => control(conn, "Show", &(m,))?,
        Call::Hide(m) => control(conn, "Hide", &(m,))?,
        Call::Toggle(m) => control(conn, "Toggle", &(m,))?,
        Call::ResetVisibility => control(conn, "ResetVisibility", &())?,
        Call::Trigger(m, g) => control(conn, "Trigger", &(m, g))?,
        Call::Update(m, kv) => control(conn, "Update", &(m, dict(&kv)))?,
        Call::SwitchLayer(n) => control(conn, "SwitchLayer", &(n,))?,
        Call::PushLayer(n, ms) => control(conn, "PushLayer", &(n, ms))?,
        Call::PopLayer => control(conn, "PopLayer", &())?,
        Call::ToggleLayer(n) => control(conn, "ToggleLayer", &(n,))?,
        Call::SetLayout(n) => control(conn, "SetLayout", &(n,))?,
        Call::SetProfile(n) => control(conn, "SetProfile", &(n,))?,
        Call::Bar(a) => control(conn, "Bar", &(a,))?,
        Call::SetVariable(n, v) => control(conn, "SetVariable", &(n, owned(value(&v))))?,
    };
    Ok(ExitCode::SUCCESS)
}

fn item(spec: &ItemSpec) -> Item {
    let mut it = match spec.ty.as_str() {
        "toggle" => Item::toggle(&spec.id),
        "label" => Item::label_item(&spec.id, ""),
        "slider" => Item::slider(&spec.id, 0.0, 1.0, 0.0),
        "progress" => Item::progress(&spec.id, 0.0),
        "spacer" => Item::spacer(&spec.id, "small"),
        "segmented" => Item::segmented(&spec.id, &[], -1),
        _ => Item::button(&spec.id),
    };
    if let Some(l) = &spec.label {
        it = it.label(l);
    }
    for (k, v) in &spec.props {
        it = it.set(k, value(v));
    }
    it
}

fn publish(conn: Connection, p: Publish) -> Result<ExitCode, String> {
    let client = Client::new(conn).map_err(describe)?;
    if !client.agent_running() {
        eprintln!("sliverctl: sliver-agent isn't running; items will appear when it starts");
    }
    let mut opts = match (p.scope.as_str(), &p.target_app) {
        ("target", Some(app)) => {
            ContextOptions::target_app(p.provider.as_deref().unwrap_or("sliverctl"), app)
        }
        ("app", _) => ContextOptions::app(),
        _ => ContextOptions::status(p.provider.as_deref().unwrap_or("sliverctl")),
    };
    if let Some(pr) = &p.provider {
        opts = opts.provider(pr);
    }
    if let Some(t) = &p.title {
        opts = opts.title(t);
    }
    if p.modal {
        opts = opts.modal(true);
    }
    let ctx = client.context(opts).map_err(describe)?;
    for w in ctx
        .set_items(p.items.iter().map(item).collect())
        .map_err(describe)?
    {
        eprintln!("sliverctl: warning: {w}");
    }
    if p.stdin {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                match cli::parse_update_line(&line) {
                    Ok(Some((id, kv))) => {
                        let mut props = Item::props();
                        for (k, v) in &kv {
                            props = props.set(k, value(v));
                        }
                        match ctx.update(&id, props) {
                            Ok(ws) => ws.iter().for_each(|w| eprintln!("sliverctl: warning: {w}")),
                            Err(e) => eprintln!("sliverctl: {}", describe(e)),
                        }
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("sliverctl: {e}"),
                }
            }
        });
    }
    let mut out = std::io::stdout().lock();
    for ev in client.events() {
        let data = show_dict(&ev.data);
        let line = if data.is_empty() {
            format!("{} {}", ev.item, ev.kind)
        } else {
            format!("{} {} {data}", ev.item, ev.kind)
        };
        // A closed pipe (`| head -1`) ends the program, like any good filter.
        if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    Ok(ExitCode::SUCCESS)
}
