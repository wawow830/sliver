//! `sliverctl`: the agent's `Control1` from the command line, the config files offline, and
//! `doctor`.

mod cli;
mod config;
mod doctor;

use cli::{Call, Cmd, KeyVals, Val};
use sliver_ipc::dbus::{BUS_NAME, IFACE_CONTROL, ROOT_PATH};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::process::ExitCode;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

fn main() -> ExitCode {
    // Die quietly on a closed pipe (`sliverctl modules | head`), like other Unix filters, instead
    // of panicking in println!. Rust ignores SIGPIPE by default.
    // SAFETY: called first thing in main, before any other threads exist.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
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

/// Print a line to stdout. A closed pipe (`sliverctl modules | head`) ends the program quietly
/// instead of panicking the way `println!` does.
fn out(line: &str) {
    let mut o = std::io::stdout().lock();
    if writeln!(o, "{line}").and_then(|_| o.flush()).is_err() {
        std::process::exit(0);
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

fn property(conn: &Connection, name: &str) -> Result<OwnedValue, String> {
    let props = zbus::blocking::fdo::PropertiesProxy::builder(conn)
        .destination(BUS_NAME)
        .and_then(|b| b.path(ROOT_PATH))
        .and_then(|b| b.build())
        .map_err(describe)?;
    props
        .get(IFACE_CONTROL.try_into().expect("interface name"), name)
        .map_err(|e| describe(e.into()))
}

fn run(cmd: Cmd) -> Result<ExitCode, String> {
    match cmd {
        Cmd::Help => {
            out(cli::USAGE.trim_end());
            return Ok(ExitCode::SUCCESS);
        }
        Cmd::Version => {
            out(&format!("sliverctl {}", env!("CARGO_PKG_VERSION")));
            return Ok(ExitCode::SUCCESS);
        }
        // Offline: never touches the session bus.
        Cmd::Config(args) => return Ok(config::run(&args)),
        // Reports a missing session bus or agent itself instead of failing up front.
        Cmd::Doctor(args) => return Ok(doctor::run(&args)),
        _ => {}
    }
    // sliver_bus: no zbus pool thread waking twice a second.
    let conn = sliver_bus::session().map_err(|e| format!("session bus: {e}"))?;
    match cmd {
        Cmd::Help | Cmd::Version | Cmd::Config(_) | Cmd::Doctor(_) => {
            unreachable!()
        }
        Cmd::Status => {
            for name in [
                "Healthy",
                "ActiveLayer",
                "ActiveLayout",
                "ActiveLayoutSource",
                "ActiveRule",
                "ActiveRuleName",
                "Layers",
                "ActiveProfile",
                "Profiles",
            ] {
                let v = property(&conn, name)?;
                out(&format!("{name}: {}", show(&v)));
            }
            let errors: Vec<String> = property(&conn, "Errors")?
                .try_into()
                .map_err(|e: zbus::zvariant::Error| e.to_string())?;
            if errors.is_empty() {
                out("Errors: none");
            } else {
                out(&format!("Errors: {}", errors.len()));
                for e in &errors {
                    out(&format!("  {e}"));
                }
            }
        }
        Cmd::Modules => {
            let list: Vec<HashMap<String, OwnedValue>> = control(&conn, "ListModules", &())?
                .body()
                .deserialize()
                .map_err(|e| e.to_string())?;
            for d in &list {
                out(&show_dict(d));
            }
        }
        Cmd::Render { config, out } => {
            let toml = match &config {
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
