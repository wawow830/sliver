//! Specs for modules showing the session and hardware: power, audio, network, notifications
//! and services.

use super::*;

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

pub(super) fn backlight(m: &ModuleInstance, world: &World, ui: &UiState) -> Option<Spec> {
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
    let mut sp = Spec::new(
        ValueMap::new().with("percent", level as f64),
        if kind.ends_with("/slider") {
            ""
        } else {
            "{percent}%"
        },
    )
    .icon(icon);
    sp.percentage = pct(Some(level));
    if kind.ends_with("/slider") {
        sp = slider(sp, m, ui, level);
    }
    Some(sp)
}

pub(super) fn audio(m: &ModuleInstance, world: &World, ui: &UiState) -> Option<Spec> {
    let s = &world.session;
    let kind = m.id.kind.as_str();
    let a = &s.audio;
    let mic = m.config.get("node").and_then(|v| v.as_str()) == Some("source");
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
    let mut sp = Spec::new(
        ValueMap::new().with("volume", vol as f64),
        if kind.ends_with("/slider") {
            ""
        } else {
            "{volume}%"
        },
    )
    .icon(icon);
    sp.percentage = pct(Some(vol));
    sp.status = muted.then_some("muted");
    sp = sp.on(Gesture::Tap, "toggle-mute");
    if kind == "audio/slider" {
        sp.defaults.clear();
        sp = slider(sp, m, ui, vol.min(100));
    }
    Some(sp)
}

pub(super) fn idle_inhibitor(world: &World) -> Option<Spec> {
    let s = &world.session;
    let on = s.caffeine;
    let mut sp = Spec::new(
        ValueMap::new().with("status", if on { "activated" } else { "deactivated" }),
        "☕",
    )
    .on(Gesture::Tap, "toggle");
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
    // plain network cell still never wakes the bar.
    let polled = matches!(m.common.interval, Some(sliver_config::Interval::Every(_)));
    let (rates, hist) = if polled {
        world.stats.with(|s| {
            let r = s.net_rates(std::time::Instant::now());
            (r.unwrap_or_default(), s.net_history())
        })
    } else {
        Default::default()
    };
    let v = ValueMap::new()
        .with(
            "bandwidthDownBytes",
            sliver_stats::human_rate(rates.down, false),
        )
        .with(
            "bandwidthUpBytes",
            sliver_stats::human_rate(rates.up, false),
        )
        .with(
            "bandwidthTotalBytes",
            sliver_stats::human_rate(rates.down + rates.up, false),
        )
        .with(
            "bandwidthDownBits",
            sliver_stats::human_rate(rates.down, true),
        )
        .with("bandwidthUpBits", sliver_stats::human_rate(rates.up, true))
        .with(
            "bandwidthTotalBits",
            sliver_stats::human_rate(rates.down + rates.up, true),
        )
        .with("spark", sliver_stats::sparkline(&hist, None))
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
    let mut sp = Spec::new(v, default).icon(icon).on(Gesture::Tap, "toggle");
    sp.percentage = n.signal_pct.map(f64::from);
    sp.status = Some(status);
    sp.checked = n.wifi_enabled == Some(true) && status != "disconnected";
    if polled {
        sp.refresh = Some(poll_every(m, 5));
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
    let mut sp = Spec::new(v, default).icon(icon).on(Gesture::Tap, "toggle");
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
        .on(Gesture::Tap, "open")
        .on(Gesture::SwipeLeft, "dismiss")
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
    let want = |k: &str| m.config.get(k).and_then(|v| v.as_bool()) != Some(false);
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
        sp = sp
            .icon("dialog-warning-symbolic")
            .on(Gesture::Tap, "menu:open");
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
            sliver_config::MenuEntry {
                label: if *is_user {
                    format!("{u} (user)")
                } else {
                    (*u).clone()
                },
                icon: None,
                on_tap: Action::Exec(argv),
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
    sp.defaults
        .push((Gesture::Tap, Action::ModuleAction("toggle".into())));
    Some(sp)
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
    sp.defaults
        .push((Gesture::Tap, Action::ModuleAction("toggle".into())));
    Some(sp)
}

/// Who is recording the microphone, the camera or the screen (PipeWire). One item per kind
/// in use: icon only by default, the apps' names in `{app}`.
pub(super) fn privacy(world: &World, item: Option<usize>) -> Option<Spec> {
    let s = &world.session;
    let kinds = s.capture.kinds();
    let (kind, apps) = *kinds.get(item?)?;
    let names = apps.join(", ");
    let v = ValueMap::new()
        .with("kind", kind)
        .with("app", esc(&names))
        .with("count", apps.len() as i64)
        .with("", esc(&names));
    let (icon, text) = match kind {
        "screen" => ("video-display-symbolic", "🖥"),
        "camera" => ("camera-web-symbolic", "📷"),
        _ => ("audio-input-microphone-symbolic", "🎙"),
    };
    let mut sp = Spec::new(v, "").icon(icon);
    sp.fallback_text = Some(text);
    sp.status = Some(match kind {
        "screen" => "screen",
        "camera" => "camera",
        _ => "mic",
    });
    sp.extra_classes = vec![format!("privacy-{kind}")];
    Some(sp)
}

/// One-shot actions: no state, so they are always there.
pub(super) fn screenshot() -> Option<Spec> {
    let mut sp = Spec::new(ValueMap::new(), "")
        .icon("applets-screenshooter-symbolic")
        .on(Gesture::Tap, "screenshot");
    sp.fallback_text = Some("📷");
    Some(sp)
}

pub(super) fn lock() -> Option<Spec> {
    let mut sp = Spec::new(ValueMap::new(), "")
        .icon("system-lock-screen-symbolic")
        .on(Gesture::Tap, "lock");
    sp.fallback_text = Some("🔒");
    Some(sp)
}

/// Tap: lock and suspend. Hold: reboot and power off, so they can't be tapped by accident.
pub(super) fn power() -> Option<Spec> {
    let mut sp = Spec::new(ValueMap::new(), "")
        .icon("system-shutdown-symbolic")
        .on(Gesture::Hold, "power:hold");
    sp.fallback_text = Some("⏻");
    sp.menu = power_menu(false);
    Some(sp)
}
