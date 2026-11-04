//! The `mpris` module's spec.

use super::*;

pub(super) fn mpris(m: &ModuleInstance, world: &World, ui: &UiState) -> Option<Spec> {
    let s = &world.session;
    // The picked player, if it is still there; else the session's choice.
    let media = ui
        .player
        .as_deref()
        .and_then(|p| s.players.iter().find(|m| m.player == p))
        .or(s.media.as_ref())?;
    let status = crate::context::media_status(media.status);
    let glyph = if status == "playing" { "⏸" } else { "▶" };
    let v = ValueMap(
        [
            ("title", esc(&media.title)),
            ("artist", esc(&media.artist)),
            ("album", esc(&media.album)),
            ("player", esc(&media.identity)),
            ("status", FmtValue::Text(status.into())),
            ("status_icon", FmtValue::Text(glyph.into())),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
    );
    let mut sp = Spec::new(v, "{title}")
        .on(Gesture::Tap, "play-pause")
        .on(Gesture::SwipeLeft, "next")
        .on(Gesture::SwipeRight, "previous");
    sp.status = Some(status);
    // Album art as the icon (when it is the session's own player and has been fetched).
    let art = m
        .config
        .get("art")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let is_session_player = s.media.as_ref().is_some_and(|x| x.player == media.player);
    if art && is_session_player {
        if let Some(p) = &world.media_art {
            sp.default_icon = Some(p.display().to_string());
        }
    }
    // Hold: switch to the next player when several are running.
    if s.players.len() > 1 {
        let idx = s
            .players
            .iter()
            .position(|m| m.player == media.player)
            .unwrap_or(0);
        let next = &s.players[(idx + 1) % s.players.len()].player;
        sp.defaults.push((
            Gesture::Hold,
            Action::ModuleAction(format!("player:{next}")),
        ));
    }
    // `scrub = true`: a progress slider; drag to seek.
    if m.config.get("scrub").and_then(|v| v.as_bool()) == Some(true) {
        if let Some(len) = media.length_us.filter(|l| *l > 0) {
            let now_ms = world.now.timestamp_millis().max(0) as u64;
            let mut pos = media.position_us.unwrap_or(0).max(0);
            if media.status == sliver_session::PlaybackStatus::Playing {
                if let Some(at) = media.position_at_ms {
                    pos += (now_ms.saturating_sub(at) as i64) * 1000;
                }
            }
            sp.slider = Some((pos as f64 / len as f64).clamp(0.0, 1.0) as f32);
            sp.percentage = Some(pos as f64 * 100.0 / len as f64);
            if media.can_seek {
                sp.defaults
                    .retain(|(g, _)| *g != Gesture::SwipeLeft && *g != Gesture::SwipeRight);
                sp.defaults
                    .push((Gesture::Drag, Action::ModuleAction("seek".into())));
            }
            if media.status == sliver_session::PlaybackStatus::Playing {
                sp.refresh = Some(std::time::Duration::from_secs(1));
            }
        }
    }
    Some(sp)
}
