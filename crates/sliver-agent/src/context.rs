//! The flat context that rules and `when` conditions match on (integration.md §1,
//! `sliver_config::CONTEXT_KEYS`), built from the desktop and session states. Unknown values are
//! left out, so conditions on them never match (they never default to something wrong).

use chrono::{DateTime, Local};
use sliver_config::Context;
use sliver_desktop::DesktopState;
use sliver_session::{BatteryState, PlaybackStatus, SessionState};

pub fn battery_state(s: BatteryState) -> &'static str {
    match s {
        BatteryState::Unknown => "unknown",
        BatteryState::Charging => "charging",
        BatteryState::Discharging => "discharging",
        BatteryState::Empty => "empty",
        BatteryState::FullyCharged => "full",
        BatteryState::PendingCharge => "pending-charge",
        BatteryState::PendingDischarge => "pending-discharge",
    }
}

pub fn media_status(s: PlaybackStatus) -> &'static str {
    match s {
        PlaybackStatus::Playing => "playing",
        PlaybackStatus::Paused => "paused",
        PlaybackStatus::Stopped => "stopped",
    }
}

pub fn build(
    desktop: &DesktopState,
    session: &SessionState,
    now: DateTime<Local>,
    physical_esc: bool,
    layer: &str,
) -> Context {
    let mut c = Context::new();
    c.set("physical_esc", physical_esc);
    c.set("layer", layer);
    c.set("time", now.format("%H:%M").to_string());
    c.set("weekday", now.format("%a").to_string().to_ascii_lowercase());

    if desktop.connected {
        if let Some(w) = &desktop.window {
            c.set("app_id", w.class.as_str());
            c.set("title", w.title.as_str());
            if let Some(pid) = w.pid {
                c.set("pid", pid as f64);
            }
            c.set("fullscreen", w.fullscreen);
            c.set("floating", w.floating);
            c.set("xwayland", w.xwayland);
        }
        if let Some(ws) = &desktop.workspace {
            c.set("workspace", ws.id as f64);
            c.set("workspace_name", ws.name.as_str());
        }
        if let Some(s) = &desktop.special_workspace {
            c.set("special_workspace", s.as_str());
        }
        if let Some(m) = &desktop.monitor {
            c.set("monitor", m.as_str());
        }
        c.set("monitors", desktop.monitors as f64);
        c.set("external_display", desktop.external_display);
        if let Some(k) = &desktop.keyboard_layout {
            c.set("keyboard_layout", k.as_str());
        }
        if let Some(m) = &desktop.submap {
            c.set("submap", m.as_str());
        }
        c.set("screencast", desktop.screencast);
    }

    c.set("locked", session.locked);
    c.set("idle", session.idle);
    c.set("sleeping", session.sleeping);
    c.set("caffeine", session.caffeine);
    let p = &session.power;
    if let Some(b) = p.on_battery {
        c.set("on_battery", b);
    }
    if let Some(pct) = p.battery_pct {
        c.set("battery_pct", pct as f64);
        c.set("battery_state", battery_state(p.battery_state));
    }
    if let Some(l) = p.lid_closed {
        c.set("lid_closed", l);
    }
    if let Some(pr) = &p.profile {
        c.set("power_profile", pr.as_str());
    }
    match &session.media {
        Some(m) => {
            c.set("media_status", media_status(m.status));
            c.set("media_player", m.identity.as_str());
            c.set("media_title", m.title.as_str());
            c.set("media_artist", m.artist.as_str());
        }
        None => {
            c.set("media_status", "none");
        }
    }
    let a = &session.audio;
    if let Some(m) = a.mic_muted {
        c.set("mic_muted", m);
    }
    if let Some(m) = a.muted {
        c.set("output_muted", m);
    }
    if let Some(v) = a.volume_pct {
        c.set("volume", v as f64);
    }
    if let Some(d) = session.notifications.dnd {
        c.set("dnd", d);
    }
    if let Some(b) = session.brightness.screen_pct {
        c.set("brightness", b as f64);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use sliver_config::load_str;
    use sliver_desktop::Window;

    #[test]
    fn rules_match_desktop_and_session() {
        let mut d = DesktopState {
            connected: true,
            ..DesktopState::default()
        };
        d.window = Some(Window {
            address: "1".into(),
            class: "kitty".into(),
            title: "vim".into(),
            pid: Some(4),
            floating: false,
            fullscreen: false,
            xwayland: false,
        });
        let mut s = SessionState::default();
        s.power.on_battery = Some(true);
        s.power.battery_pct = Some(12);
        let now = Local.with_ymd_and_hms(2026, 10, 12, 23, 30, 0).unwrap();
        let ctx = build(&d, &s, now, true, "base");
        let cfg = load_str(
            r#"
            [[rules]]
            when = { app-id = "kitty", on-battery = true, battery-pct = { below = 20 } }
            layout = "fkeys"
            "#,
            std::path::Path::new("."),
        )
        .config;
        assert_eq!(cfg.resolve("base", &ctx).unwrap().layout, "fkeys");
        assert!(
            matches!(ctx.get("media_status"), Some(sliver_config::CtxValue::Str(s)) if s == "none")
        );
        d.connected = false;
        let ctx = build(&d, &s, now, true, "base");
        assert_eq!(
            cfg.resolve("base", &ctx).unwrap().layout,
            "default",
            "no compositor: no app match"
        );
    }
}

/// Screen lockers that don't keep logind's `LockedHint` honest themselves.
const LOCKERS: &[&str] = &[
    "hyprlock",
    "swaylock",
    "gtklock",
    "waylock",
    "i3lock",
    "physlock",
    "swaylock-effects",
];

/// Whether to believe logind's `LockedHint` (integration.md §3: it's verified to read `true` on
/// an unlocked Hyprland session). On compositors whose lockers are separate programs, trust it
/// only while one of them runs. GNOME and KDE maintain the hint, so trust it there.
/// `running` lists process names (`/proc/*/comm`; injectable for tests).
pub fn plausibly_locked(
    hint: bool,
    backend: sliver_desktop::Backend,
    running: impl FnOnce() -> Vec<String>,
) -> bool {
    use sliver_desktop::Backend::*;
    match (hint, backend) {
        (false, _) => false,
        (true, Gnome | Kde) => true,
        (true, _) => running().iter().any(|p| LOCKERS.contains(&p.as_str())),
    }
}

/// Names of running processes (`comm`).
pub fn process_names() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|e| std::fs::read_to_string(e.path().join("comm")).ok())
        .map(|s| s.trim().to_owned())
        .collect()
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use sliver_desktop::Backend;

    #[test]
    fn stale_hint_on_hyprland_is_ignored() {
        let none = || Vec::new();
        assert!(!plausibly_locked(true, Backend::Hyprland, none));
        assert!(plausibly_locked(true, Backend::Hyprland, || vec![
            "bash".into(),
            "hyprlock".into()
        ]));
        assert!(plausibly_locked(true, Backend::Gnome, none));
        assert!(!plausibly_locked(false, Backend::Gnome, none));
    }
}
