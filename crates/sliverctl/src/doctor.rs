//! `sliverctl doctor`: checks the whole install and says what to do about each problem
//! (automates the troubleshooting table in the user guide).
//!
//! The checks are plain functions over [`Sys`], the facts about the machine (units, bus names,
//! files, the agent's properties), so they are tested against a fake; [`Real`] gathers the real
//! ones. Reading only: nothing is started, changed or written. Checks that need root (the
//! daemon's hardware side) are not here; they come from sliverd itself (see docs/sliverd.md).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    /// Something optional is missing: some modules will be empty, or an integration is off.
    Warn,
    /// The bar or the config is not working.
    Fail,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        }
    }

    fn json(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable short name (`sliverd`, `agent`, `config`, `desktop`, `source/network`...).
    pub id: String,
    pub level: Level,
    pub summary: String,
    /// What to do, when it isn't ok.
    pub fix: Option<String>,
}

fn check(id: &str, level: Level, summary: impl Into<String>, fix: Option<&str>) -> Check {
    Check {
        id: id.into(),
        level,
        summary: summary.into(),
        fix: fix.map(str::to_owned),
    }
}

/// What systemd says about a unit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unit {
    /// `loaded`, `not-found`...
    pub load: String,
    /// `active`, `inactive`, `failed`, `activating`...
    pub active: String,
    /// `running`, `dead`, `failed`...
    pub sub: String,
    pub restarts: u32,
}

impl Unit {
    fn installed(&self) -> bool {
        !self.load.is_empty() && self.load != "not-found"
    }
}

/// What the agent says about itself over D-Bus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Agent {
    pub healthy: bool,
    pub errors: Vec<String>,
    pub version: Option<String>,
}

/// The config as loaded: its path, and what is wrong with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigState {
    pub path: Option<PathBuf>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Facts about the machine. Every method answers from the real system, or from a test's fake.
pub trait Sys {
    fn env(&self, key: &str) -> Option<String>;
    fn unit(&self, name: &str, user: bool) -> Unit;
    fn exists(&self, path: &Path) -> bool;
    /// Is `name` on `$PATH`?
    fn command(&self, name: &str) -> bool;
    /// Does `name` have an owner, or can the bus start it on demand? `None`: that bus can't be
    /// reached at all.
    fn bus_name(&self, system: bool, name: &str) -> Option<bool>;
    /// `Err`: the agent isn't on the bus (or answered with an error).
    fn agent(&self) -> Result<Agent, String>;
    fn config(&self) -> ConfigState;
    fn read(&self, path: &Path) -> Option<String>;
    /// Stdout of a command, `None` if it isn't there or fails.
    fn run(&self, cmd: &[&str]) -> Option<String>;
}

/// The machine, for the report header.
fn header(sys: &dyn Sys) -> Vec<(String, String)> {
    let os = sys
        .read(Path::new("/etc/os-release"))
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| "unknown".into());
    let model = [
        "/sys/devices/virtual/dmi/id/product_name",
        "/proc/device-tree/model",
    ]
    .iter()
    .find_map(|p| sys.read(Path::new(p)))
    .map(|s| s.trim_matches(char::from(0)).trim().to_owned())
    .unwrap_or_else(|| "unknown".into());
    let session = format!(
        "{} ({})",
        sys.env("XDG_CURRENT_DESKTOP").unwrap_or_else(|| "?".into()),
        sys.env("XDG_SESSION_TYPE").unwrap_or_else(|| "?".into())
    );
    vec![
        ("sliverctl".into(), env!("CARGO_PKG_VERSION").into()),
        ("os".into(), os),
        ("machine".into(), model),
        ("session".into(), session),
    ]
}

pub fn run_checks(sys: &dyn Sys) -> Vec<Check> {
    let mut out = Vec::new();
    out.extend(daemon(sys));
    out.extend(agent(sys));
    out.push(config(sys));
    out.push(desktop(sys));
    out.extend(sources(sys));
    out.extend(helpers(sys));
    out
}

// ---- sliverd and tiny-dfr -------------------------------------------------------------------

fn daemon(sys: &dyn Sys) -> Vec<Check> {
    let d = sys.unit("sliverd.service", false);
    let t = sys.unit("tiny-dfr.service", false);
    let mut out = Vec::new();
    let tiny_active = t.active == "active";
    out.push(if !d.installed() {
        check(
            "sliverd",
            Level::Fail,
            "sliverd isn't installed",
            Some("install the sliverd package (see the user guide, section 3)"),
        )
    } else if d.active == "active" {
        let restarts = if d.restarts > 0 {
            format!(", restarted {} times since boot", d.restarts)
        } else {
            String::new()
        };
        check("sliverd", Level::Ok, format!("sliverd is {}{restarts}", d.sub), None)
    } else if d.active == "failed" {
        check(
            "sliverd",
            Level::Fail,
            format!(
                "sliverd failed{}: the bar is on the fallback ({})",
                if d.restarts > 0 {
                    format!(" after {} restarts", d.restarts)
                } else {
                    String::new()
                },
                if tiny_active { "tiny-dfr" } else { "nothing" }
            ),
            Some(
                "journalctl -u sliverd -b, then: sudo systemctl reset-failed sliverd && sudo systemctl start sliverd",
            ),
        )
    } else {
        check(
            "sliverd",
            Level::Warn,
            format!("sliverd is {} (not running)", d.active),
            Some("sudo systemctl start sliverd (it starts by itself at the next boot)"),
        )
    });
    // Two daemons on one bar is the one combination that is plainly wrong.
    if d.active == "active" && tiny_active {
        out.push(check(
            "tiny-dfr",
            Level::Fail,
            "tiny-dfr and sliverd are both running; only one can own the bar",
            Some("sudo systemctl stop tiny-dfr (sliverd's unit normally stops it: reinstall the sliverd package)"),
        ));
    } else if t.installed() {
        out.push(check(
            "tiny-dfr",
            Level::Ok,
            format!(
                "tiny-dfr is installed and {} (the fallback when sliverd can't run)",
                t.active
            ),
            None,
        ));
    }
    out
}

// ---- the agent ------------------------------------------------------------------------------

fn agent(sys: &dyn Sys) -> Vec<Check> {
    let unit = sys.unit("sliver-agent.service", true);
    let mut out = Vec::new();
    if unit.installed() && unit.active != "active" {
        out.push(check(
            "agent-unit",
            if unit.active == "failed" {
                Level::Fail
            } else {
                Level::Warn
            },
            format!("sliver-agent.service is {}", unit.active),
            Some("journalctl --user -u sliver-agent -b; systemctl --user restart sliver-agent"),
        ));
    }
    match sys.agent() {
        Ok(a) if a.healthy && a.errors.is_empty() => out.push(check(
            "agent",
            Level::Ok,
            format!(
                "the agent is driving the bar{}",
                a.version.map(|v| format!(" (API {v})")).unwrap_or_default()
            ),
            None,
        )),
        Ok(a) if !a.errors.is_empty() => {
            let mut s = format!("the agent reports {} problem(s):", a.errors.len());
            for e in a.errors.iter().take(5) {
                s.push_str(&format!("\n        {e}"));
            }
            out.push(check(
                "agent",
                Level::Fail,
                s,
                Some("fix the file it names; the last good config stays active meanwhile (sliverctl config check)"),
            ));
        }
        Ok(_) => out.push(check(
            "agent",
            Level::Fail,
            "the agent is running but not driving the bar",
            Some("is sliverd running? journalctl --user -u sliver-agent -b"),
        )),
        // No session bus at all (ssh, a root shell): starting the agent wouldn't help.
        Err(_) if sys.bus_name(false, sliver_ipc::dbus::BUS_NAME).is_none() => out.push(check(
            "agent",
            Level::Warn,
            "the session bus isn't reachable, so the agent can't be checked",
            Some("run `sliverctl doctor` inside your desktop session"),
        )),
        Err(e) => out.push(check(
            "agent",
            Level::Fail,
            format!("the agent isn't reachable: {e}"),
            Some("systemctl --user start sliver-agent (the bar shows only F-keys until then)"),
        )),
    }
    out
}

fn config(sys: &dyn Sys) -> Check {
    let c = sys.config();
    let at = c
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "the built-in defaults".into());
    if !c.errors.is_empty() {
        let mut s = format!("{at}: {} error(s):", c.errors.len());
        for e in c.errors.iter().take(5) {
            s.push_str(&format!("\n        {e}"));
        }
        check(
            "config",
            Level::Fail,
            s,
            Some("sliverctl config check; invalid configs are never applied"),
        )
    } else if !c.warnings.is_empty() {
        let mut s = format!("{at}: {} warning(s):", c.warnings.len());
        for w in c.warnings.iter().take(5) {
            s.push_str(&format!("\n        {w}"));
        }
        check("config", Level::Warn, s, Some("sliverctl config check"))
    } else {
        check(
            "config",
            Level::Ok,
            match &c.path {
                Some(_) => format!("{at} is valid"),
                None => "no config file: the built-in defaults are in use".into(),
            },
            None,
        )
    }
}

// ---- the desktop ----------------------------------------------------------------------------

fn desktop(sys: &dyn Sys) -> Check {
    let env = |k: &str| sys.env(k);
    let set = |k: &str| env(k).is_some_and(|v| !v.is_empty());
    let desktop = env("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let part = |p: &str| desktop.split(':').any(|d| d == p);
    if set("HYPRLAND_INSTANCE_SIGNATURE") || part("hyprland") {
        check(
            "desktop",
            Level::Ok,
            "Hyprland: focus, workspaces and windows follow it",
            None,
        )
    } else if set("NIRI_SOCKET") || part("niri") {
        check(
            "desktop",
            Level::Ok,
            "niri: focus, workspaces and windows follow it",
            None,
        )
    } else if set("SWAYSOCK") || part("sway") {
        check(
            "desktop",
            Level::Ok,
            "sway: focus, workspaces and windows follow it",
            None,
        )
    } else if part("kde") {
        let enabled = sys
            .read(&config_dir(sys).join("kwinrc"))
            .is_some_and(|s| s.lines().any(|l| l.trim() == "sliver-focusEnabled=true"));
        if enabled {
            check(
                "desktop",
                Level::Ok,
                "KDE: the Sliver focus KWin script is enabled",
                None,
            )
        } else {
            check(
                "desktop",
                Level::Warn,
                "KDE: the Sliver focus KWin script isn't enabled, so per-app layouts won't follow the focus",
                Some(
                    "System Settings → Window Management → KWin Scripts → Sliver focus (package sliver-kwin)",
                ),
            )
        }
    } else if part("gnome") {
        let enabled = sys
            .run(&["gnome-extensions", "list", "--enabled"])
            .is_some_and(|s| s.lines().any(|l| l.trim().starts_with("sliver-focus@")));
        if enabled {
            check(
                "desktop",
                Level::Ok,
                "GNOME: the Sliver focus extension is enabled",
                None,
            )
        } else {
            check(
                "desktop",
                Level::Warn,
                "GNOME: the Sliver focus extension isn't enabled, so per-app layouts won't follow the focus",
                Some(
                    "gnome-extensions enable sliver-focus@sliver.wawow830.github.io, then log out and in (package gnome-shell-extension-sliver)",
                ),
            )
        }
    } else if set("WAYLAND_DISPLAY") {
        check(
            "desktop",
            Level::Ok,
            "another Wayland compositor: windows and workspaces via the generic protocols (wlr-foreign-toplevel, ext-workspace)",
            None,
        )
    } else if set("DISPLAY") {
        check(
            "desktop",
            Level::Warn,
            "an X11 session: focus-based layouts aren't supported yet",
            Some("layouts still switch by layer, profile and state (battery, media, lock)"),
        )
    } else {
        check(
            "desktop",
            Level::Warn,
            "no graphical session found in this shell's environment",
            Some(
                "run `sliverctl doctor` inside your desktop session, not over ssh or from a system service",
            ),
        )
    }
}

fn config_dir(sys: &dyn Sys) -> PathBuf {
    sys.env("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| sys.env("HOME").map(|h| Path::new(&h).join(".config")))
        .unwrap_or_default()
}

// ---- session sources ------------------------------------------------------------------------

/// What each optional source feeds, so a missing one says what stays empty.
fn sources(sys: &dyn Sys) -> Vec<Check> {
    // (id, system bus?, bus name, what it feeds, what to do)
    const BUSES: [(&str, bool, &str, &str, &str); 6] = [
        (
            "network",
            true,
            "org.freedesktop.NetworkManager",
            "the `network` module and the Wi-Fi toggle",
            "install and start NetworkManager (systemctl enable --now NetworkManager)",
        ),
        (
            "power",
            true,
            "org.freedesktop.UPower",
            "the `battery` module",
            "install and start upower (systemctl enable --now upower)",
        ),
        (
            "logind",
            true,
            "org.freedesktop.login1",
            "lock/sleep state, caffeine, the lock and power modules",
            "systemd-logind should always be there: is this a systemd system?",
        ),
        (
            "notifications",
            false,
            "org.freedesktop.Notifications",
            "the `notifications` module and Do Not Disturb",
            "run a notification daemon (mako, swaync, dunst, or your desktop's)",
        ),
        (
            "tray",
            false,
            "org.kde.StatusNotifierWatcher",
            "the `tray` / `status-items` modules",
            "run a bar or tray host that provides a StatusNotifierWatcher (waybar, quickshell, KDE, GNOME's AppIndicator extension)",
        ),
        (
            "portal",
            false,
            "org.freedesktop.portal.Desktop",
            "the `screenshot` module, accent colour and dark mode",
            "install xdg-desktop-portal and a backend for your desktop",
        ),
    ];
    let mut out = Vec::new();
    for (id, system, name, feeds, fix) in BUSES {
        let label = match id {
            "network" => "NetworkManager",
            "power" => "UPower",
            "logind" => "systemd-logind",
            "notifications" => "a notification daemon",
            "tray" => "a tray host (StatusNotifierWatcher)",
            _ => "xdg-desktop-portal",
        };
        let c = match sys.bus_name(system, name) {
            Some(true) => check(
                &format!("source/{id}"),
                Level::Ok,
                format!("{label} is available"),
                None,
            ),
            Some(false) => check(
                &format!("source/{id}"),
                // logind is not optional.
                if id == "logind" {
                    Level::Fail
                } else {
                    Level::Warn
                },
                format!("{label} isn't running: {feeds} will be empty"),
                Some(fix),
            ),
            None => check(
                &format!("source/{id}"),
                Level::Warn,
                format!(
                    "the {} bus isn't reachable, so {label} can't be checked",
                    if system { "system" } else { "session" }
                ),
                Some("run `sliverctl doctor` inside your desktop session"),
            ),
        };
        out.push(c);
    }
    // PipeWire: its own socket, and the pactl that `audio` follows.
    let runtime = sys
        .env("PIPEWIRE_RUNTIME_DIR")
        .or_else(|| sys.env("XDG_RUNTIME_DIR"));
    let socket = runtime.map(|r| Path::new(&r).join("pipewire-0"));
    out.push(match socket {
        Some(s) if sys.exists(&s) && sys.command("wpctl") && sys.command("pactl") => {
            check("source/audio", Level::Ok, "PipeWire is running (wpctl and pactl found)", None)
        }
        Some(s) if sys.exists(&s) => check(
            "source/audio",
            Level::Warn,
            "PipeWire is running but wpctl or pactl is missing: the volume sliders won't work",
            Some("install wireplumber and pulseaudio-utils"),
        ),
        _ => check(
            "source/audio",
            Level::Warn,
            "PipeWire isn't running: the audio sliders, mute toggles and the `privacy` module will be empty",
            Some("systemctl --user enable --now pipewire pipewire-pulse wireplumber"),
        ),
    });
    out
}

fn helpers(sys: &dyn Sys) -> Vec<Check> {
    let mut out = Vec::new();
    let wayland = sys.env("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    if wayland {
        out.push(if sys.command("wtype") {
            check(
                "helper/wtype",
                Level::Ok,
                "wtype is installed (the emoji picker types into the focused app)",
                None,
            )
        } else {
            check(
                "helper/wtype",
                Level::Warn,
                "wtype isn't installed: the emoji picker falls back to the clipboard",
                Some("sudo dnf install wtype"),
            )
        });
    }
    out
}

// ---- output ---------------------------------------------------------------------------------

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The report as JSON: `{"machine": {...}, "checks": [{"id", "level", "summary", "fix"}]}`.
pub fn render_json(sys: &dyn Sys, checks: &[Check]) -> String {
    let head: Vec<String> = header(sys)
        .iter()
        .map(|(k, v)| format!("{}:{}", json_str(k), json_str(v)))
        .collect();
    let list: Vec<String> = checks
        .iter()
        .map(|c| {
            format!(
                "{{\"id\":{},\"level\":{},\"summary\":{},\"fix\":{}}}",
                json_str(&c.id),
                json_str(c.level.json()),
                json_str(&c.summary),
                c.fix
                    .as_deref()
                    .map(json_str)
                    .unwrap_or_else(|| "null".into())
            )
        })
        .collect();
    format!(
        "{{\"machine\":{{{}}},\"checks\":[{}]}}",
        head.join(","),
        list.join(",")
    )
}

/// The report for people: a header, then one block per check.
pub fn render_text(sys: &dyn Sys, checks: &[Check]) -> String {
    let mut s = String::new();
    for (k, v) in header(sys) {
        s.push_str(&format!("{k:<10} {v}\n"));
    }
    s.push('\n');
    for c in checks {
        s.push_str(&format!("{:<5} {}\n", c.level.tag(), c.summary));
        if let Some(f) = &c.fix {
            s.push_str(&format!("      fix: {f}\n"));
        }
    }
    let count = |l| checks.iter().filter(|c| c.level == l).count();
    let n = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    s.push_str(&format!(
        "\n{} ok, {}, {}\n",
        count(Level::Ok),
        n(count(Level::Warn), "warning", "warnings"),
        n(count(Level::Fail), "failure", "failures")
    ));
    s
}

// ---- the real machine -----------------------------------------------------------------------

pub struct Real {
    session: Option<zbus::blocking::Connection>,
    system: Option<zbus::blocking::Connection>,
}

impl Real {
    pub fn connect() -> Real {
        Real {
            session: sliver_bus::session().ok(),
            system: sliver_bus::system().ok(),
        }
    }
}

fn run_cmd(cmd: &[&str]) -> Option<String> {
    let (prog, args) = cmd.split_first()?;
    let out = std::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Sys for Real {
    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn unit(&self, name: &str, user: bool) -> Unit {
        let mut cmd = vec!["systemctl"];
        if user {
            cmd.push("--user");
        }
        cmd.extend([
            "show",
            "-p",
            "LoadState",
            "-p",
            "ActiveState",
            "-p",
            "SubState",
            "-p",
            "NRestarts",
            name,
        ]);
        let Some(text) = run_cmd(&cmd) else {
            return Unit::default();
        };
        let kv: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
        Unit {
            load: kv.get("LoadState").unwrap_or(&"").to_string(),
            active: kv.get("ActiveState").unwrap_or(&"").to_string(),
            sub: kv.get("SubState").unwrap_or(&"").to_string(),
            restarts: kv
                .get("NRestarts")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        }
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn command(&self, name: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
    }

    fn bus_name(&self, system: bool, name: &str) -> Option<bool> {
        let conn = if system { &self.system } else { &self.session }.as_ref()?;
        let bus = zbus::blocking::fdo::DBusProxy::new(conn).ok()?;
        let owned = bus.name_has_owner(name.try_into().ok()?).ok()?;
        // Portals and some notification daemons start on first use.
        Some(
            owned
                || bus
                    .list_activatable_names()
                    .is_ok_and(|l| l.iter().any(|n| n.as_str() == name)),
        )
    }

    fn agent(&self) -> Result<Agent, String> {
        use sliver_ipc::dbus::{BUS_NAME, IFACE_CONTROL, ROOT_PATH};
        let conn = self.session.as_ref().ok_or("no session bus")?;
        let props = zbus::blocking::fdo::PropertiesProxy::builder(conn)
            .destination(BUS_NAME)
            .and_then(|b| b.path(ROOT_PATH))
            .and_then(|b| b.build())
            .map_err(|e| e.to_string())?;
        let get = |iface: &str, name: &str| {
            props
                .get(iface.try_into().expect("interface name"), name)
                .map_err(|e| match e {
                    zbus::fdo::Error::ServiceUnknown(_) => {
                        "sliver-agent isn't on the session bus".to_string()
                    }
                    e => e.to_string(),
                })
        };
        let healthy: bool = get(IFACE_CONTROL, "Healthy")?
            .try_into()
            .map_err(|e: zbus::zvariant::Error| e.to_string())?;
        // Errors (API minor 3): an older agent doesn't have it.
        let errors = get(IFACE_CONTROL, "Errors")
            .ok()
            .and_then(|v| <Vec<String>>::try_from(v).ok())
            .unwrap_or_default();
        let version = get(sliver_ipc::dbus::IFACE_TOUCHBAR, "Version")
            .ok()
            .and_then(|v| u32::try_from(v).ok())
            .map(|v| v.to_string());
        Ok(Agent {
            healthy,
            errors,
            version,
        })
    }

    fn config(&self) -> ConfigState {
        let loaded = sliver_config::load();
        let mut d = loaded.diagnostics;
        if d.ok() {
            d.extend(loaded.config.validate(sliver_config::Hardware::detect()));
        }
        ConfigState {
            path: sliver_config::find_config(),
            errors: d.errors.iter().map(|e| e.to_string()).collect(),
            warnings: d.warnings.iter().map(|e| e.to_string()).collect(),
        }
    }

    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn run(&self, cmd: &[&str]) -> Option<String> {
        run_cmd(cmd)
    }
}

/// `sliverctl doctor [--json]`. Exit status: 1 if anything failed, else 0.
pub fn run(args: &[String]) -> std::process::ExitCode {
    let mut json = false;
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            other => {
                eprintln!(
                    "sliverctl doctor: unknown argument {other:?}\n\nusage: sliverctl doctor [--json]"
                );
                return std::process::ExitCode::from(2);
            }
        }
    }
    let sys = Real::connect();
    let checks = run_checks(&sys);
    let text = if json {
        render_json(&sys, &checks)
    } else {
        render_text(&sys, &checks)
    };
    print!("{text}");
    if json {
        println!();
    }
    if checks.iter().any(|c| c.level == Level::Fail) {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests;
