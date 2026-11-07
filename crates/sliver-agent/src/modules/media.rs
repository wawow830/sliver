//! The `mpris` module's spec.

use super::*;
use crate::agent::SessionOp;
use sliver_session::mpris::Command;

pub(super) fn mpris(at: &At) -> Option<Spec> {
    let (m, world, ui) = (at.m, at.world, at.ui);
    let s = &world.session;
    // The picked player, if it is still there; else the session's choice.
    let media = ui
        .player
        .as_deref()
        .and_then(|p| s.players.iter().find(|m| m.player == p))
        .or(s.media.as_ref())?;
    let status = crate::context::media_status(media.status);
    let playing = media.status == sliver_session::PlaybackStatus::Playing;
    let glyph = if playing { "⏸" } else { "▶" };
    // Where playback is now: the last reported position, moved on by the time since while
    // playing (players report on seeks and state changes, not continuously).
    let length = media.length_us.filter(|l| *l > 0);
    let mut pos = media.position_us.unwrap_or(0).max(0);
    if playing && let Some(at) = media.position_at_ms {
        let now_ms = world.now.wall.timestamp_millis().max(0) as u64;
        pos += (now_ms.saturating_sub(at) as i64) * 1000;
    }
    let pos = length.map_or(pos, |l| pos.min(l));
    let v = ValueMap(
        [
            ("title", esc(&media.title)),
            ("artist", esc(&media.artist)),
            ("album", esc(&media.album)),
            ("player", esc(&media.identity)),
            ("status", FmtValue::Text(status.into())),
            ("status_icon", FmtValue::Text(glyph.into())),
            ("position", FmtValue::Text(clock_time(pos))),
            (
                "length",
                FmtValue::Text(length.map(clock_time).unwrap_or_default()),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
    );
    // Transport commands go to the picked player, else the session's.
    let command = |cmd: Command| {
        Act::Session(match &ui.player {
            Some(player) => SessionOp::MediaOn {
                player: player.clone(),
                cmd,
            },
            None => SessionOp::Media(cmd),
        })
    };
    let mut sp = Spec::new(v, "{title}")
        .does(Gesture::Tap, "play-pause", command(Command::PlayPause))
        .does(Gesture::SwipeLeft, "next", command(Command::Next))
        .does(Gesture::SwipeRight, "previous", command(Command::Previous));
    sp.status = Some(status);
    // Album art as the icon (when it is the session's own player and has been fetched).
    let art = m.flag("art").unwrap_or(true);
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
        let pick = Act::Ui {
            key: at.key.to_owned(),
            change: UiChange::Player(next.clone()),
        };
        sp = sp.does(Gesture::Hold, "next-player", pick);
    }
    // `{position}` moves while playing: redraw each second (only while it's on the bar).
    let c = &m.common;
    let shows_position = c
        .format
        .iter()
        .chain(&c.format_alt)
        .chain(c.format_variants.values())
        .any(|f| f.contains("{position"));
    if playing && shows_position {
        sp.redraw(world.now.wall, std::time::Duration::from_secs(1));
    }
    // `scrub = true`: a progress slider; drag to seek.
    if m.flag("scrub") == Some(true) {
        if let Some(len) = length {
            sp.face = Face::Slider((pos as f64 / len as f64).clamp(0.0, 1.0) as f32);
            sp.percentage = Some(pos as f64 * 100.0 / len as f64);
            if media.can_seek {
                sp.defaults
                    .retain(|(g, _)| *g != Gesture::SwipeLeft && *g != Gesture::SwipeRight);
                let seek = Act::Seek {
                    player: ui.player.clone(),
                };
                sp = sp.does(Gesture::Drag, "seek", seek);
            }
            if playing {
                sp.redraw(world.now.wall, std::time::Duration::from_secs(1));
            }
        }
    }
    Some(sp)
}

/// `m:ss`, or `h:mm:ss` from an hour.
fn clock_time(us: i64) -> String {
    let s = us.max(0) / 1_000_000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clock_time() {
        assert_eq!(super::clock_time(65_400_000), "1:05");
        assert_eq!(super::clock_time(0), "0:00");
        assert_eq!(super::clock_time(3_725_000_000), "1:02:05");
    }
}
