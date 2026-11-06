//! Folding the desktop and session state into the rule [`Context`] (`CONTEXT_KEYS`).
//!
//! Unknown values are left out, never defaulted: in a rule, "unknown never matches"
//! (customization.md §6), so a missing UPower doesn't make `on-battery = false` true.

use sliver_config::Context;
use sliver_desktop::DesktopState;
use sliver_icons::desktop::DesktopEntries;
use sliver_session::{BatteryState, PlaybackStatus, SessionState};

fn kebab(s: BatteryState) -> &'static str {
    match s {
        BatteryState::Unknown => "unknown",
        BatteryState::Charging => "charging",
        BatteryState::Discharging => "discharging",
        BatteryState::Empty => "empty",
        BatteryState::FullyCharged => "fully-charged",
        BatteryState::PendingCharge => "pending-charge",
        BatteryState::PendingDischarge => "pending-discharge",
    }
}

/// Add everything the compositor tells us. `apps` resolves the focused app's `.desktop` id.
pub fn add_desktop(ctx: &mut Context, d: &DesktopState, apps: Option<&DesktopEntries>) {
    if !d.connected {
        return;
    }
    if let Some(w) = &d.window {
        ctx.set("app_id", w.class.as_str());
        ctx.set("title", w.title.as_str());
        if let Some(pid) = w.pid {
            ctx.set("pid", f64::from(pid));
        }
        ctx.set("fullscreen", w.fullscreen);
        ctx.set("floating", w.floating);
        ctx.set("xwayland", w.xwayland);
        if let Some(e) = apps.and_then(|a| a.find_by_app_id(&w.class)) {
            ctx.set("desktop_id", format!("{}.desktop", e.id));
        }
    }
    if let Some(ws) = &d.workspace {
        ctx.set("workspace", ws.id as f64);
        ctx.set("workspace_name", ws.name.as_str());
    }
    if let Some(s) = &d.special_workspace {
        ctx.set("special_workspace", s.as_str());
    }
    if let Some(m) = &d.monitor {
        ctx.set("monitor", m.as_str());
    }
    ctx.set("monitors", f64::from(d.monitors));
    ctx.set("external_display", d.external_display);
    if let Some(l) = &d.keyboard_layout {
        ctx.set("keyboard_layout", l.as_str());
    }
    if let Some(s) = &d.submap {
        ctx.set("submap", s.as_str());
    }
    ctx.set("screencast", d.screencast);
}

/// Add lock, power, media, audio, brightness and notification state.
pub fn add_session(ctx: &mut Context, s: &SessionState) {
    ctx.set("locked", s.locked);
    ctx.set("idle", s.idle);
    ctx.set("sleeping", s.sleeping);
    ctx.set("caffeine", s.caffeine || s.idle_inhibited);
    let p = &s.power;
    if let Some(b) = p.on_battery {
        ctx.set("on_battery", b);
    }
    if let Some(pct) = p.battery_pct {
        ctx.set("battery_pct", f64::from(pct));
        ctx.set("battery_state", kebab(p.battery_state));
    }
    if let Some(b) = p.lid_closed {
        ctx.set("lid_closed", b);
    }
    if let Some(pr) = &p.profile {
        ctx.set("power_profile", pr.as_str());
    }
    match &s.media {
        Some(m) => {
            ctx.set(
                "media_status",
                match m.status {
                    PlaybackStatus::Playing => "playing",
                    PlaybackStatus::Paused => "paused",
                    PlaybackStatus::Stopped => "stopped",
                },
            );
            ctx.set("media_player", m.identity.as_str());
            ctx.set("media_title", m.title.as_str());
            ctx.set("media_artist", m.artist.as_str());
        }
        None => {
            ctx.set("media_status", "none");
        }
    }
    let a = &s.audio;
    if let Some(m) = a.mic_muted {
        ctx.set("mic_muted", m);
    }
    if let Some(m) = a.muted {
        ctx.set("output_muted", m);
    }
    if let Some(v) = a.volume_pct {
        ctx.set("volume", f64::from(v));
    }
    if let Some(d) = s.notifications.dnd {
        ctx.set("dnd", d);
    }
    if let Some(d) = s.appearance.dark {
        ctx.set("dark_mode", d);
    }
    if let Some(b) = s.brightness.screen_pct {
        ctx.set("brightness", f64::from(b));
    }
}
