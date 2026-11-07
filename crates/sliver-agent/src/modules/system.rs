//! Specs for modules showing the session and hardware: power, audio, network, notifications
//! and services.

use super::*;
use crate::act::Level;
use crate::agent::SessionOp;

fn session(op: SessionOp) -> Act {
    Act::Session(op)
}

pub(super) fn battery(world: &World) -> Option<Spec> {
    let s = &world.session;
    let p = &s.power;
    let level = p.battery_pct?;
    let charging = matches!(
        p.battery_state,
        BatteryState::Charging | BatteryState::PendingCharge
    );
    let mut sp = Spec::new(
        ValueMap::new().with("capacity", level as f64),
        "{capacity}%",
    )
    .icon(battery_icon(level, charging));
    sp.percentage = Some(level as f64);
    sp.lower_is_worse = true;
    sp.status = match p.battery_state {
        BatteryState::Charging | BatteryState::PendingCharge => Some("charging"),
        BatteryState::FullyCharged => Some("full"),
        BatteryState::Discharging | BatteryState::PendingDischarge => Some("discharging"),
        _ => None,
    };
    Some(sp)
}

pub(super) fn backlight(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let s = &world.session;
    let kind = m.id.kind.as_str();
    let kbd = kind.starts_with("kbd");
    let level = if kbd {
        s.brightness.keyboard_pct
    } else {
        s.brightness.screen_pct
    }?;
    let icon = if kbd {
        "keyboard-brightness-symbolic"
    } else {
        "display-brightness-symbolic"
    };
    let mut sp = Spec::new(ValueMap::new().with("percent", level as f64), "{percent}%").icon(icon);
    sp.percentage = pct(Some(level));
    sp.level = Some(f32::from(level.min(100)) / 100.0);
    let which = if kbd { Level::Keyboard } else { Level::Screen };
    Some(sp.verb("set", Act::Set(which)))
}

pub(super) fn audio(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let s = &world.session;
    let a = &s.audio;
    let mic = m.text("node") == Some("source");
    let (vol, muted) = if mic {
        (a.mic_volume_pct?, a.mic_muted.unwrap_or(false))
    } else {
        (a.volume_pct?, a.muted.unwrap_or(false))
    };
    let icon = match (mic, muted, vol) {
        (true, true, _) => "microphone-disabled-symbolic",
        (true, false, _) => "audio-input-microphone-symbolic",
        (false, true, _) => "audio-volume-muted-symbolic",
        (false, false, 0..=33) => "audio-volume-low-symbolic",
        (false, false, 34..=66) => "audio-volume-medium-symbolic",
        _ => "audio-volume-high-symbolic",
    };
    let mut sp = Spec::new(ValueMap::new().with("volume", vol as f64), "{volume}%").icon(icon);
    sp.percentage = pct(Some(vol));
    sp.level = Some(f32::from(vol.min(100)) / 100.0);
    sp.status = muted.then_some("muted");
    Some(sp.verb("set", Act::Set(Level::Volume { mic })).does(
        Gesture::Tap,
        "toggle-mute",
        session(SessionOp::ToggleMute { mic }),
    ))
}

pub(super) fn idle_inhibitor(world: &World) -> Option<Spec> {
    let s = &world.session;
    let on = s.caffeine;
    let mut sp = Spec::new(
        ValueMap::new().with("status", if on { "activated" } else { "deactivated" }),
        "☕",
    )
    .does(Gesture::Tap, "toggle", session(SessionOp::ToggleCaffeine));
    sp.status = Some(if on { "activated" } else { "deactivated" });
    sp.checked = on;
    Some(sp)
}

pub(super) fn power_profiles(world: &World) -> Option<Spec> {
    let s = &world.session;
    let p = s.power.profile.as_deref()?;
    Some(Spec::new(
        ValueMap::new().with("profile", p).with("", p),
        "{profile}",
    ))
}

pub(super) fn network(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let s = &world.session;
    use sliver_session::network::{NetKind, NetState};
    let n = &s.network;
    if n.state == NetState::Unknown {
        return None; // no NetworkManager
    }
    let (status, icon): (&'static str, String) = match (n.state, n.kind) {
        _ if n.wifi_enabled == Some(false) => ("off", "network-wireless-disabled-symbolic".into()),
        (NetState::Disconnected, _) => ("disconnected", "network-wireless-offline-symbolic".into()),
        (NetState::Connecting, _) => ("connecting", "network-wireless-acquiring-symbolic".into()),
        (_, NetKind::Wifi) => {
            let level = match n.signal_pct.unwrap_or(0) {
                80..=100 => "excellent",
                55..=79 => "good",
                30..=54 => "ok",
                1..=29 => "weak",
                _ => "none",
            };
            ("wifi", format!("network-wireless-signal-{level}-symbolic"))
        }
        (_, NetKind::Ethernet) => ("ethernet", "network-wired-symbolic".into()),
        (_, NetKind::Vpn) => ("vpn", "network-vpn-symbolic".into()),
        _ => ("linked", "network-wired-symbolic".into()),
    };
    let essid = n.ssid.clone().unwrap_or_else(|| n.name.clone());
    // Throughput is only sampled when the module has an explicit `interval`, so a
    // plain network cell never wakes the bar.
    let polled = matches!(m.common.interval, Some(sliver_config::Interval::Every(_)));
    let (rates, hist) = if polled {
        let s = &world.stats;
        (s.net.unwrap_or_default(), s.net_history.clone())
    } else {
        Default::default()
    };
    let v = ValueMap::new()
        .with(
            "bandwidthDownBytes",
            crate::stats::human_rate(rates.down, false),
        )
        .with(
            "bandwidthUpBytes",
            crate::stats::human_rate(rates.up, false),
        )
        .with(
            "bandwidthTotalBytes",
            crate::stats::human_rate(rates.down + rates.up, false),
        )
        .with(
            "bandwidthDownBits",
            crate::stats::human_rate(rates.down, true),
        )
        .with("bandwidthUpBits", crate::stats::human_rate(rates.up, true))
        .with(
            "bandwidthTotalBits",
            crate::stats::human_rate(rates.down + rates.up, true),
        )
        .with("spark", crate::stats::sparkline(&hist, None))
        .with("ifname", esc(n.iface.as_deref().unwrap_or("")))
        .with("essid", esc(&essid))
        .with(
            "signalStrength",
            FmtValue::Num(f64::from(n.signal_pct.unwrap_or(0))),
        )
        .with("ipaddr", esc(n.ipv4.as_deref().unwrap_or("")))
        .with("state", status)
        .with("", esc(&essid));
    let default = match status {
        "wifi" => "{essid}",
        "disconnected" | "connecting" => "",
        _ => "{ifname}",
    };
    let mut sp = Spec::new(v, default).icon(icon).does(
        Gesture::Tap,
        "toggle",
        session(SessionOp::ToggleWifi),
    );
    sp.percentage = n.signal_pct.map(f64::from);
    sp.status = Some(status);
    sp.checked = n.wifi_enabled == Some(true) && status != "disconnected";
    if polled {
        sp.redraw(world.now.wall, poll_every(m, 5));
    }
    Some(sp)
}

pub(super) fn bluetooth(world: &World) -> Option<Spec> {
    let s = &world.session;
    let b = &s.bluetooth;
    if !b.available {
        return None;
    }
    let status = if !b.powered {
        "off"
    } else if b.connected.is_empty() {
        "on"
    } else {
        "connected"
    };
    let first = b.connected.first();
    let v = ValueMap::new()
        .with("status", status)
        .with("num_connections", b.connected.len() as i64)
        .with("device_alias", esc(first.map_or("", |d| d.name.as_str())))
        .with(
            "device_battery_percentage",
            FmtValue::Num(first.and_then(|d| d.battery).map_or(0.0, f64::from)),
        )
        .with("", esc(first.map_or("", |d| d.name.as_str())));
    let icon = match status {
        "off" => "bluetooth-disabled-symbolic",
        "on" => "bluetooth-active-symbolic",
        _ => "bluetooth-active-symbolic",
    };
    let default = if status == "connected" {
        "{device_alias}"
    } else {
        ""
    };
    let mut sp = Spec::new(v, default).icon(icon).does(
        Gesture::Tap,
        "toggle",
        session(SessionOp::ToggleBluetooth),
    );
    sp.status = Some(status);
    sp.checked = status == "connected";
    Some(sp)
}

pub(super) fn notifications(world: &World) -> Option<Spec> {
    let s = &world.session;
    let n = &s.notifications;
    let count = if n.count == 0 {
        FmtValue::Text(String::new())
    } else {
        FmtValue::Num(f64::from(n.count))
    };
    let (summary, app) = n
        .latest
        .as_ref()
        .map_or((String::new(), String::new()), |l| {
            (l.summary.clone(), l.app.clone())
        });
    let v = ValueMap::new()
        .with("count", count.clone())
        .with("summary", esc(&summary))
        .with("app", esc(&app))
        .with("", count);
    // The latest summary when there is one (a bell with just the number otherwise).
    let default = if n.latest.is_some() {
        "{summary}"
    } else {
        "{count}"
    };
    let mut sp = Spec::new(v, default)
        .icon(if n.dnd == Some(true) {
            "notifications-disabled-symbolic"
        } else {
            "notifications-symbolic"
        })
        .does(Gesture::Tap, "open", session(SessionOp::OpenNotification))
        .does(
            Gesture::SwipeLeft,
            "dismiss",
            session(SessionOp::DismissNotification),
        )
        .on(Gesture::SwipeRight, "dismiss");
    sp.status = Some(if n.count > 0 { "pending" } else { "none" });
    sp.extra_classes = n
        .dnd
        .filter(|d| *d)
        .map(|_| "dnd".to_string())
        .into_iter()
        .collect();
    Some(sp)
}

/// waybar's: failed units of the system and user managers. Hidden while none failed,
/// unless `hide-on-ok = false`. Tap lists them; an entry shows its status in a terminal.
pub(super) fn failed_units(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let s = &world.session;
    let f = &s.failed_units;
    let want = |k: &str| m.flag(k) != Some(false);
    let sys = f.system.as_ref().filter(|_| want("system"));
    let user = f.user.as_ref().filter(|_| want("user"));
    if sys.is_none() && user.is_none() {
        return None;
    }
    let n = |x: Option<&sliver_session::failed::Manager>| x.map_or(0, |m| m.count);
    let total = n(sys) + n(user);
    if total == 0 && want("hide-on-ok") {
        return None;
    }
    let names: Vec<(bool, &String)> = sys
        .iter()
        .flat_map(|m| m.units.iter().map(|u| (false, u)))
        .chain(user.iter().flat_map(|m| m.units.iter().map(|u| (true, u))))
        .collect();
    let list = names
        .iter()
        .map(|(_, u)| u.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let status = if total == 0 { "ok" } else { "degraded" };
    let v = ValueMap::new()
        .with("nr_failed", total as i64)
        .with("nr_failed_system", n(sys) as i64)
        .with("nr_failed_user", n(user) as i64)
        .with("units", esc(&list))
        .with("status", status)
        .with("", total as i64);
    let mut sp = Spec::new(
        v,
        if total == 0 {
            "✓"
        } else {
            "{nr_failed} failed"
        },
    );
    if total > 0 {
        sp = sp.icon("dialog-warning-symbolic").on(Gesture::Tap, "menu");
    }
    sp.status = Some(status);
    sp.menu = names
        .iter()
        .map(|(is_user, u)| {
            let mut argv = vec!["xdg-terminal-exec".to_owned(), "systemctl".to_owned()];
            if *is_user {
                argv.push("--user".into());
            }
            argv.extend(["status".to_owned(), (*u).clone()]);
            MenuItem {
                label: if *is_user {
                    format!("{u} (user)")
                } else {
                    (*u).clone()
                },
                icon: None,
                on_tap: Do::Run(Action::Exec(argv)),
            }
        })
        .collect();
    Some(sp)
}

/// Do Not Disturb, when the notification daemon reports it (dunst, mako, swaync, KDE).
pub(super) fn dnd(world: &World) -> Option<Spec> {
    let s = &world.session;
    let on = s.notifications.dnd?;
    let mut sp = Spec::new(
        ValueMap::new().with("state", if on { "on" } else { "off" }),
        "",
    )
    .icon(if on {
        "notifications-disabled-symbolic"
    } else {
        "notifications-symbolic"
    });
    sp.status = Some(if on { "on" } else { "off" });
    sp.checked = on;
    Some(sp.does(Gesture::Tap, "toggle", session(SessionOp::ToggleDnd)))
}

/// Light/dark colour scheme, when the portal reports one.
pub(super) fn dark_mode(world: &World) -> Option<Spec> {
    let s = &world.session;
    let dark = s.appearance.dark?;
    let mut sp = Spec::new(
        ValueMap::new().with("state", if dark { "dark" } else { "light" }),
        "",
    )
    .icon(if dark {
        "weather-clear-night-symbolic"
    } else {
        "weather-clear-symbolic"
    });
    sp.status = Some(if dark { "dark" } else { "light" });
    sp.checked = dark;
    Some(sp.does(Gesture::Tap, "toggle", session(SessionOp::ToggleDark)))
}

/// One-shot actions: no state, so they are always there.
pub(super) fn screenshot() -> Option<Spec> {
    let mut sp = Spec::new(ValueMap::new(), "")
        .icon("applets-screenshooter-symbolic")
        .does(Gesture::Tap, "screenshot", Do::Run(screenshot_action()));
    sp.face = Face::Label {
        fallback: Some("📷".into()),
    };
    Some(sp)
}

pub(super) fn lock() -> Option<Spec> {
    let mut sp = Spec::new(ValueMap::new(), "")
        .icon("system-lock-screen-symbolic")
        .does(Gesture::Tap, "lock", Do::Run(lock_action()));
    sp.face = Face::Label {
        fallback: Some("🔒".into()),
    };
    Some(sp)
}

/// Tap: lock and suspend. Hold: reboot and power off, so they can't be tapped by accident.
pub(super) fn power(key: &str) -> Option<Spec> {
    let more = Act::Menu {
        key: key.to_owned(),
        items: power_menu(true),
    };
    let mut sp = Spec::new(ValueMap::new(), "")
        .icon("system-shutdown-symbolic")
        .does(Gesture::Hold, "shutdown", more);
    sp.face = Face::Label {
        fallback: Some("⏻".into()),
    };
    sp.menu = power_menu(false);
    Some(sp)
}

pub(super) const GIB: f64 = (1u64 << 30) as f64;

/// Rounds like waybar's integer percentages and one-decimal sizes.
pub(super) fn pct0(v: f64) -> FmtValue {
    FmtValue::Num(v.round())
}

pub(super) fn gib(bytes: u64) -> FmtValue {
    FmtValue::Num((bytes as f64 / GIB * 10.0).round() / 10.0)
}

/// How often a polled module refreshes: its `interval`, else the module's own default.
pub(super) fn poll_every(m: &ModuleInstance, default_secs: u64) -> std::time::Duration {
    match m.common.interval {
        Some(sliver_config::Interval::Every(d)) => d,
        _ => std::time::Duration::from_secs(default_secs),
    }
}

/// `cpu`, `memory`, `load`, `temperature`, `disk`: read when drawn, redrawn on their interval
/// (the agent's loop wakes only while such a cell is on the bar and the display is on).
pub(super) fn stat_spec(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let stats = &world.stats;
    let mut sp = match m.id.kind.as_str() {
        "cpu" => {
            let (load, ghz, hist) = (stats.load, stats.cpu_ghz, &stats.cpu_history);
            let usage = stats.cpu?;
            let v = ValueMap::new()
                .with("usage", pct0(usage))
                .with("", pct0(usage))
                .with("load", load.map_or(0.0, |l| l.one))
                .with("avg_frequency", (ghz.unwrap_or(0.0) * 10.0).round() / 10.0)
                .with("spark", crate::stats::sparkline(hist, Some(100.0)));
            let mut sp = Spec::new(v, "CPU {usage}%");
            sp.percentage = Some(usage);
            sp
        }
        "memory" => {
            let mem = stats.mem?;
            let v = ValueMap::new()
                .with("percentage", pct0(mem.percent_used()))
                .with("", pct0(mem.percent_used()))
                .with("used", gib(mem.used()))
                .with("total", gib(mem.total))
                .with("avail", gib(mem.available))
                .with("swapPercentage", pct0(mem.swap_percent()))
                .with("swapUsed", gib(mem.swap_used()))
                .with("swapTotal", gib(mem.swap_total))
                .with("swapAvail", gib(mem.swap_free));
            let mut sp = Spec::new(v, "MEM {percentage}%");
            sp.percentage = Some(mem.percent_used());
            sp
        }
        "load" => {
            let l = stats.load?;
            let v = ValueMap::new()
                .with("load1", l.one)
                .with("load5", l.five)
                .with("load15", l.fifteen)
                .with("", l.one);
            Spec::new(v, "{load1}")
        }
        "temperature" => {
            let c = *stats.temps.get(&crate::stats::TempSource::of(m))?;
            let v = ValueMap::new()
                .with("temperatureC", c.round())
                .with("temperatureF", (c * 9.0 / 5.0 + 32.0).round())
                .with("temperatureK", (c + 273.15).round())
                .with("", c.round());
            let mut sp = Spec::new(v, "{temperatureC}°C");
            let critical = m.number("critical-threshold");
            sp.status = critical.filter(|t| c >= *t).map(|_| "critical");
            sp
        }
        "disk" => {
            let path = m.text("path").unwrap_or("/");
            let d = *stats.disks.get(path)?;
            let v = ValueMap::new()
                .with("percentage_used", pct0(d.percent_used()))
                .with("percentage_free", pct0(d.percent_free()))
                .with("used", gib(d.used()))
                .with("free", gib(d.free))
                .with("total", gib(d.total))
                .with("path", esc(path))
                .with("", pct0(d.percent_used()));
            let mut sp = Spec::new(v, "{percentage_used}%");
            sp.percentage = Some(d.percent_used());
            sp
        }
        _ => return None,
    };
    let default = match m.id.kind.as_str() {
        "memory" | "disk" => 30,
        _ => 10,
    };
    sp.redraw(world.now.wall, poll_every(m, default));
    Some(sp)
}

/// tiny-dfr's battery icon thresholds, on the built-in icon set.
pub(super) fn battery_icon(level: u8, charging: bool) -> &'static str {
    if charging {
        match level {
            0..=20 => "battery_charging_20",
            21..=30 => "battery_charging_30",
            31..=50 => "battery_charging_50",
            51..=60 => "battery_charging_60",
            61..=80 => "battery_charging_80",
            81..=99 => "battery_charging_90",
            _ => "battery_charging_full",
        }
    } else {
        match level {
            0 => "battery_0_bar",
            1..=20 => "battery_1_bar",
            21..=30 => "battery_2_bar",
            31..=50 => "battery_3_bar",
            51..=60 => "battery_4_bar",
            61..=80 => "battery_5_bar",
            81..=99 => "battery_6_bar",
            _ => "battery_full",
        }
    }
}
