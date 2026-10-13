use super::*;
use std::collections::{HashMap, HashSet};

/// A machine as a test describes it.
#[derive(Default)]
struct Fake {
    env: HashMap<String, String>,
    units: HashMap<(String, bool), Unit>,
    files: HashMap<PathBuf, String>,
    commands: HashSet<String>,
    session_names: HashSet<String>,
    system_names: HashSet<String>,
    no_session_bus: bool,
    agent: Option<Result<Agent, String>>,
    config: ConfigState,
    run: HashMap<String, String>,
}

impl Fake {
    /// A healthy Hyprland laptop.
    fn healthy() -> Fake {
        let mut f = Fake::default();
        f.env
            .insert("HYPRLAND_INSTANCE_SIGNATURE".into(), "x".into());
        f.env.insert("XDG_RUNTIME_DIR".into(), "/run/user/1".into());
        f.env.insert("WAYLAND_DISPLAY".into(), "wayland-1".into());
        let up = |sub: &str| Unit {
            load: "loaded".into(),
            active: "active".into(),
            sub: sub.into(),
            restarts: 0,
        };
        f.units
            .insert(("sliverd.service".into(), false), up("running"));
        f.units
            .insert(("sliver-agent.service".into(), true), up("running"));
        f.units.insert(
            ("tiny-dfr.service".into(), false),
            Unit {
                load: "loaded".into(),
                active: "inactive".into(),
                sub: "dead".into(),
                restarts: 0,
            },
        );
        f.files
            .insert("/run/user/1/pipewire-0".into(), String::new());
        for c in ["wpctl", "pactl", "wtype"] {
            f.commands.insert(c.into());
        }
        for n in [
            "org.freedesktop.Notifications",
            "org.kde.StatusNotifierWatcher",
            "org.freedesktop.portal.Desktop",
        ] {
            f.session_names.insert(n.into());
        }
        for n in [
            "org.freedesktop.NetworkManager",
            "org.freedesktop.UPower",
            "org.freedesktop.login1",
        ] {
            f.system_names.insert(n.into());
        }
        f.agent = Some(Ok(Agent {
            healthy: true,
            errors: vec![],
            version: Some("3".into()),
        }));
        f
    }
}

impl Sys for Fake {
    fn env(&self, key: &str) -> Option<String> {
        self.env.get(key).cloned()
    }
    fn unit(&self, name: &str, user: bool) -> Unit {
        self.units
            .get(&(name.to_owned(), user))
            .cloned()
            .unwrap_or_default()
    }
    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
    fn command(&self, name: &str) -> bool {
        self.commands.contains(name)
    }
    fn bus_name(&self, system: bool, name: &str) -> Option<bool> {
        if !system && self.no_session_bus {
            return None;
        }
        let set = if system {
            &self.system_names
        } else {
            &self.session_names
        };
        Some(set.contains(name))
    }
    fn agent(&self) -> Result<Agent, String> {
        self.agent
            .clone()
            .unwrap_or_else(|| Err("not on the bus".into()))
    }
    fn config(&self) -> ConfigState {
        self.config.clone()
    }
    fn read(&self, path: &Path) -> Option<String> {
        self.files.get(path).cloned()
    }
    fn run(&self, cmd: &[&str]) -> Option<String> {
        self.run.get(&cmd.join(" ")).cloned()
    }
}

fn find<'a>(checks: &'a [Check], id: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no check {id}: {checks:#?}"))
}

#[test]
fn a_healthy_machine_has_nothing_but_ok() {
    let checks = run_checks(&Fake::healthy());
    let bad: Vec<_> = checks.iter().filter(|c| c.level != Level::Ok).collect();
    assert!(bad.is_empty(), "{bad:#?}");
    assert!(checks.iter().all(|c| c.fix.is_none()));
}

#[test]
fn a_failed_daemon_names_the_fallback_and_the_fix() {
    let mut f = Fake::healthy();
    f.units.insert(
        ("sliverd.service".into(), false),
        Unit {
            load: "loaded".into(),
            active: "failed".into(),
            sub: "failed".into(),
            restarts: 5,
        },
    );
    f.units
        .get_mut(&("tiny-dfr.service".into(), false))
        .unwrap()
        .active = "active".into();
    let c = run_checks(&f);
    let d = find(&c, "sliverd");
    assert_eq!(d.level, Level::Fail);
    assert!(
        d.summary.contains("5 restarts") && d.summary.contains("tiny-dfr"),
        "{d:?}"
    );
    assert!(d.fix.as_deref().unwrap().contains("reset-failed"));
}

#[test]
fn both_daemons_on_one_bar_is_a_failure() {
    let mut f = Fake::healthy();
    f.units
        .get_mut(&("tiny-dfr.service".into(), false))
        .unwrap()
        .active = "active".into();
    assert_eq!(find(&run_checks(&f), "tiny-dfr").level, Level::Fail);
}

#[test]
fn not_installed_and_not_running_are_told_apart() {
    let mut f = Fake::healthy();
    f.units.remove(&("sliverd.service".into(), false));
    let c = run_checks(&f);
    assert!(find(&c, "sliverd").summary.contains("isn't installed"));
    f.units.insert(
        ("sliverd.service".into(), false),
        Unit {
            load: "loaded".into(),
            active: "inactive".into(),
            sub: "dead".into(),
            restarts: 0,
        },
    );
    let d = run_checks(&f);
    let d = find(&d, "sliverd");
    assert_eq!(d.level, Level::Warn);
    assert!(
        d.fix
            .as_deref()
            .unwrap()
            .contains("systemctl start sliverd")
    );
}

#[test]
fn agent_problems_come_with_the_errors() {
    let mut f = Fake::healthy();
    f.agent = Some(Ok(Agent {
        healthy: true,
        errors: vec!["config.toml: layouts.x: bad width".into()],
        version: None,
    }));
    let c = run_checks(&f);
    let a = find(&c, "agent");
    assert_eq!(a.level, Level::Fail);
    assert!(a.summary.contains("bad width"), "{a:?}");
    f.agent = Some(Err("sliver-agent isn't on the session bus".into()));
    f.units
        .get_mut(&("sliver-agent.service".into(), true))
        .unwrap()
        .active = "failed".into();
    let c = run_checks(&f);
    assert_eq!(find(&c, "agent").level, Level::Fail);
    assert_eq!(find(&c, "agent-unit").level, Level::Fail);
    f.agent = Some(Ok(Agent {
        healthy: false,
        errors: vec![],
        version: None,
    }));
    f.units
        .get_mut(&("sliver-agent.service".into(), true))
        .unwrap()
        .active = "active".into();
    assert!(
        find(&run_checks(&f), "agent")
            .summary
            .contains("not driving the bar")
    );
}

#[test]
fn config_errors_fail_and_warnings_warn() {
    let mut f = Fake::healthy();
    f.config = ConfigState {
        path: Some("/home/u/.config/sliver/config.toml".into()),
        errors: vec!["line 3: expected `=`".into()],
        warnings: vec![],
    };
    let c = run_checks(&f);
    assert_eq!(find(&c, "config").level, Level::Fail);
    assert!(find(&c, "config").summary.contains("line 3"));
    f.config.errors.clear();
    f.config.warnings = vec!["unknown key".into()];
    assert_eq!(find(&run_checks(&f), "config").level, Level::Warn);
}

#[test]
fn missing_sources_say_what_goes_empty() {
    let mut f = Fake::healthy();
    f.system_names.remove("org.freedesktop.NetworkManager");
    f.session_names.remove("org.kde.StatusNotifierWatcher");
    f.files.clear();
    let c = run_checks(&f);
    let n = find(&c, "source/network");
    assert_eq!(n.level, Level::Warn);
    assert!(n.summary.contains("`network` module"), "{n:?}");
    assert!(
        find(&c, "source/tray")
            .fix
            .as_deref()
            .unwrap()
            .contains("StatusNotifierWatcher")
    );
    assert!(
        find(&c, "source/audio")
            .summary
            .contains("PipeWire isn't running")
    );
    // logind is not optional
    f.system_names.remove("org.freedesktop.login1");
    assert_eq!(find(&run_checks(&f), "source/logind").level, Level::Fail);
}

#[test]
fn no_session_bus_is_one_clear_warning_per_source_not_a_crash() {
    let mut f = Fake::healthy();
    f.no_session_bus = true;
    f.agent = Some(Err("no session bus".into()));
    let c = run_checks(&f);
    assert!(
        find(&c, "source/tray")
            .summary
            .contains("session bus isn't reachable")
    );
    // The agent too: a warning to run it in the session, not "start the agent".
    let a = find(&c, "agent");
    assert_eq!(a.level, Level::Warn);
    assert!(
        a.fix
            .as_deref()
            .unwrap()
            .contains("inside your desktop session"),
        "{a:?}"
    );
    // The summary reads as a sentence.
    assert!(
        find(&c, "source/notifications")
            .summary
            .contains("so a notification daemon"),
        "{:?}",
        find(&c, "source/notifications")
    );
}

#[test]
fn the_desktop_helpers_are_checked_where_they_are_needed() {
    // KDE: the script must be enabled in kwinrc
    let mut f = Fake::healthy();
    f.env.remove("HYPRLAND_INSTANCE_SIGNATURE");
    f.env.insert("XDG_CURRENT_DESKTOP".into(), "KDE".into());
    f.env.insert("HOME".into(), "/home/u".into());
    assert_eq!(find(&run_checks(&f), "desktop").level, Level::Warn);
    f.files.insert(
        "/home/u/.config/kwinrc".into(),
        "[Plugins]\nsliver-focusEnabled=true\n".into(),
    );
    assert_eq!(find(&run_checks(&f), "desktop").level, Level::Ok);
    // GNOME: asks gnome-extensions
    f.env.insert("XDG_CURRENT_DESKTOP".into(), "GNOME".into());
    assert_eq!(find(&run_checks(&f), "desktop").level, Level::Warn);
    f.run.insert(
        "gnome-extensions list --enabled".into(),
        "sliver-focus@sliver.wawow830.github.io\n".into(),
    );
    assert_eq!(find(&run_checks(&f), "desktop").level, Level::Ok);
    // X11 only
    let mut x = Fake::healthy();
    x.env.remove("HYPRLAND_INSTANCE_SIGNATURE");
    x.env.remove("WAYLAND_DISPLAY");
    x.env.insert("DISPLAY".into(), ":0".into());
    assert!(find(&run_checks(&x), "desktop").summary.contains("X11"));
    // wtype only matters on Wayland
    let mut w = Fake::healthy();
    w.commands.remove("wtype");
    assert_eq!(find(&run_checks(&w), "helper/wtype").level, Level::Warn);
    w.env.remove("WAYLAND_DISPLAY");
    assert!(run_checks(&w).iter().all(|c| c.id != "helper/wtype"));
}

#[test]
fn the_reports_carry_the_machine_and_every_check() {
    let mut f = Fake::healthy();
    f.files.insert(
        "/etc/os-release".into(),
        "NAME=Fedora\nPRETTY_NAME=\"Fedora Asahi Remix 44\"\n".into(),
    );
    f.files.insert(
        "/sys/devices/virtual/dmi/id/product_name".into(),
        "Mac14,7\n".into(),
    );
    f.system_names.remove("org.freedesktop.UPower");
    let checks = run_checks(&f);
    let text = render_text(&f, &checks);
    assert!(
        text.contains("Fedora Asahi Remix 44") && text.contains("Mac14,7"),
        "{text}"
    );
    assert!(text.contains("WARN  UPower isn't running"), "{text}");
    assert!(
        text.contains("      fix: install and start upower"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("1 warning, 0 failures"), "{text}");
    let json = render_json(&f, &checks);
    assert!(
        json.starts_with("{\"machine\":{") && json.contains("\"os\":\"Fedora Asahi Remix 44\""),
        "{json}"
    );
    assert!(json.contains("\"level\":\"warn\""), "{json}");
    assert!(json.contains("\"fix\":null"), "{json}");
    // valid JSON: quotes and newlines in summaries are escaped
    assert_eq!(json_str("a\"b\nc"), "\"a\\\"b\\nc\"");
}
