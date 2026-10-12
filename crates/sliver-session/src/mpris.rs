//! MPRIS: find players on the session bus, follow them, and pick the one to show.
//!
//! Event driven: `NameOwnerChanged` (player appears/vanishes), `PropertiesChanged` and `Seeked`.
//! On any of them every player is re-read (a few `GetAll`s; events are rare). `Position` is not
//! signalled by MPRIS, so it is only captured with each re-read.

use crate::{Hub, Media, PlaybackStatus, SessionState, props};
use std::collections::HashMap;
use zbus::blocking::Connection;

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";

pub fn parse_status(s: &str) -> PlaybackStatus {
    match s {
        "Playing" => PlaybackStatus::Playing,
        "Paused" => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
    }
}

/// Build a [`Media`] from the `Player` interface properties and the root `Identity`.
pub fn parse_player(bus_name: &str, identity: Option<String>, p: &props::Props) -> Media {
    let meta: props::Props = p
        .get("Metadata")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| props::Props::try_from(v).ok())
        .unwrap_or_default();
    let text = |k: &str| props::string(&meta, k).unwrap_or_default();
    let artist = meta
        .get("xesam:artist")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<String>::try_from(v).ok().or(None))
        .map(|a| a.join(", "))
        .or_else(|| props::string(&meta, "xesam:artist")) // some players send a plain string
        .unwrap_or_default();
    Media {
        player: bus_name.to_string(),
        identity: identity.unwrap_or_else(|| bus_name.trim_start_matches(PREFIX).to_string()),
        status: props::string(p, "PlaybackStatus")
            .map(|s| parse_status(&s))
            .unwrap_or_default(),
        title: text("xesam:title"),
        artist,
        album: text("xesam:album"),
        art_url: props::string(&meta, "mpris:artUrl").filter(|s| !s.is_empty()),
        length_us: meta.get("mpris:length").and_then(props::int),
        position_us: p.get("Position").and_then(props::int),
        can_play_pause: props::boolean(p, "CanPause").or(props::boolean(p, "CanPlay"))
            == Some(true),
        can_next: props::boolean(p, "CanGoNext") == Some(true),
        can_previous: props::boolean(p, "CanGoPrevious") == Some(true),
        can_seek: props::boolean(p, "CanSeek") == Some(true),
    }
}

/// Which player to show: a playing one beats a paused one beats a stopped one; ties go to the
/// one that changed most recently (`recency`, higher = newer), then to the lowest bus name.
pub fn pick_active(players: &[(Media, u64)]) -> Option<&Media> {
    let rank = |m: &Media| match m.status {
        PlaybackStatus::Playing => 2,
        PlaybackStatus::Paused => 1,
        PlaybackStatus::Stopped => 0,
    };
    players
        .iter()
        .max_by(|(a, ra), (b, rb)| {
            (rank(a), ra)
                .cmp(&(rank(b), rb))
                .then_with(|| b.player.cmp(&a.player))
        })
        .map(|(m, _)| m)
}

/// `playerctld` is a proxy that re-exports the last active player; counting it would duplicate.
fn is_real_player(name: &str) -> bool {
    name.starts_with(PREFIX) && name != "org.mpris.MediaPlayer2.playerctld"
}

fn read_players(conn: &Connection) -> Vec<Media> {
    let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(conn) else {
        return Vec::new();
    };
    let Ok(names) = bus.list_names() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for n in names
        .iter()
        .map(|n| n.as_str())
        .filter(|n| is_real_player(n))
    {
        let Some(p) = props::get_all(conn, n, PATH, "org.mpris.MediaPlayer2.Player") else {
            continue;
        };
        let identity = props::get_all(conn, n, PATH, "org.mpris.MediaPlayer2")
            .and_then(|r| props::string(&r, "Identity"));
        out.push(parse_player(n, identity, &p));
    }
    out
}

/// Remembers when each player last changed, so the newest of several is picked.
pub struct Tracker {
    seen: HashMap<String, (Fingerprint, u64)>,
    tick: u64,
}

type Fingerprint = (PlaybackStatus, String, String);

impl Tracker {
    /// Initial read into `state`.
    pub fn new(conn: &Connection, state: &mut SessionState) -> Self {
        let mut t = Self {
            seen: HashMap::new(),
            tick: 0,
        };
        state.media = t.refresh(read_players(conn));
        t
    }

    fn refresh(&mut self, players: Vec<Media>) -> Option<Media> {
        self.seen
            .retain(|k, _| players.iter().any(|m| &m.player == k));
        let mut ranked = Vec::with_capacity(players.len());
        for m in players {
            let fp = (m.status, m.title.clone(), m.artist.clone());
            let tick = &mut self.tick;
            let entry = self.seen.entry(m.player.clone()).or_insert_with(|| {
                *tick += 1;
                (fp.clone(), *tick)
            });
            if entry.0 != fp {
                self.tick += 1;
                *entry = (fp, self.tick);
            }
            ranked.push((m, entry.1));
        }
        pick_active(&ranked).cloned()
    }

    pub(crate) fn run(mut self, conn: Connection, signals: crate::Signals, hub: Hub) {
        for _ in signals {
            let media = self.refresh(read_players(&conn));
            if !hub.update(|s| s.media = media) {
                return;
            }
        }
    }
}

/// Match rules for [`Tracker::run`]; subscribe to them before [`Tracker::new`].
pub(crate) const RULES: [&str; 3] = [
    "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged',path='/org/mpris/MediaPlayer2'",
    "type='signal',interface='org.mpris.MediaPlayer2.Player',member='Seeked'",
    "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'",
];

/// Commands for [`control`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

/// Send a transport command to `player` (a full bus name from [`Media::player`]).
pub fn control(conn: &Connection, player: &str, cmd: Command) -> zbus::Result<()> {
    let method = match cmd {
        Command::PlayPause => "PlayPause",
        Command::Play => "Play",
        Command::Pause => "Pause",
        Command::Next => "Next",
        Command::Previous => "Previous",
        Command::Stop => "Stop",
    };
    conn.call_method(
        Some(player),
        PATH,
        Some("org.mpris.MediaPlayer2.Player"),
        method,
        &(),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(name: &str, status: PlaybackStatus) -> Media {
        Media {
            player: name.into(),
            status,
            ..Default::default()
        }
    }

    #[test]
    fn playing_beats_newer_paused() {
        let ps = [
            (m("a", PlaybackStatus::Playing), 1),
            (m("b", PlaybackStatus::Paused), 9),
            (m("c", PlaybackStatus::Stopped), 20),
        ];
        assert_eq!(pick_active(&ps).unwrap().player, "a");
    }

    #[test]
    fn newest_wins_among_equals_then_name() {
        let ps = [
            (m("a", PlaybackStatus::Paused), 3),
            (m("b", PlaybackStatus::Paused), 5),
        ];
        assert_eq!(pick_active(&ps).unwrap().player, "b");
        let ps = [
            (m("b", PlaybackStatus::Paused), 5),
            (m("a", PlaybackStatus::Paused), 5),
        ];
        assert_eq!(pick_active(&ps).unwrap().player, "a");
        assert!(pick_active(&[]).is_none());
    }

    #[test]
    fn playerctld_is_not_a_player() {
        assert!(!is_real_player("org.mpris.MediaPlayer2.playerctld"));
        assert!(is_real_player(
            "org.mpris.MediaPlayer2.firefox.instance_1_2"
        ));
        assert!(!is_real_player("org.freedesktop.Notifications"));
    }

    #[test]
    fn tracker_prefers_the_one_that_changed_last() {
        let mut t = Tracker {
            seen: HashMap::new(),
            tick: 0,
        };
        let a = m("a", PlaybackStatus::Paused);
        let b = m("b", PlaybackStatus::Paused);
        assert_eq!(t.refresh(vec![a.clone(), b.clone()]).unwrap().player, "b");
        // a starts a new track: it becomes the most recent
        let a2 = Media {
            title: "song".into(),
            ..a
        };
        assert_eq!(t.refresh(vec![a2, b.clone()]).unwrap().player, "a");
        // a goes away
        assert_eq!(t.refresh(vec![b]).unwrap().player, "b");
        assert!(t.refresh(vec![]).is_none());
    }
}
