//! `sliver-session`: session and system state that is not the compositor's.
//!
//! Lock/idle/sleep (logind), power (UPower, power-profiles-daemon) and media (MPRIS) reduced to
//! one flat, comparable [`SessionState`]. Sources are event driven: a thread sleeps in a D-Bus
//! read and re-reads a handful of properties when a relevant signal arrives. Downstream only
//! hears about real changes. Compositor state lives in `sliver-desktop`; the design and what is
//! still missing (audio, brightness, notifications) is in `docs/integration.md`.
//!
//! Testing: [`fake::ScriptedSession`] replays states; the real sources are tested against mock
//! services on a private `dbus-daemon` (see `tests/`).

pub mod audio;
pub mod brightness;
pub mod fake;
pub mod logind;
pub mod mpris;
pub mod power;
pub(crate) mod props;

use serde::{Deserialize, Serialize};
use std::io;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatteryState {
    #[default]
    Unknown,
    Charging,
    Discharging,
    Empty,
    FullyCharged,
    PendingCharge,
    PendingDischarge,
}

impl BatteryState {
    /// UPower `Device.State` (u32).
    pub fn from_upower(n: u32) -> Self {
        match n {
            1 => Self::Charging,
            2 => Self::Discharging,
            3 => Self::Empty,
            4 => Self::FullyCharged,
            5 => Self::PendingCharge,
            6 => Self::PendingDischarge,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Power {
    /// `None` = UPower unavailable.
    pub on_battery: Option<bool>,
    /// 0..=100, from UPower's DisplayDevice. `None` = no battery.
    pub battery_pct: Option<u8>,
    pub battery_state: BatteryState,
    pub lid_closed: Option<bool>,
    /// power-profiles-daemon: `power-saver` / `balanced` / `performance`.
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

/// The one player the Touch Bar should show (see [`mpris::pick_active`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    /// Full bus name, e.g. `org.mpris.MediaPlayer2.firefox.instance_1_42`. Use it to control.
    pub player: String,
    /// Human name (`Identity`), e.g. "Mozilla Firefox".
    pub identity: String,
    pub status: PlaybackStatus,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    pub length_us: Option<i64>,
    /// Position when this state was read. MPRIS never signals position changes: while
    /// `Playing`, extrapolate from the time of the last state change instead of polling.
    pub position_us: Option<i64>,
    pub can_play_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
}

/// Everything in this crate, flat and comparable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    /// logind `LockedHint` plus `Lock`/`Unlock` edges. A hint, not a fact: see integration.md §3.
    pub locked: bool,
    /// logind `IdleHint`.
    pub idle: bool,
    /// Between `PrepareForSleep(true)` and `PrepareForSleep(false)`.
    pub sleeping: bool,
    /// Anyone (including us) holds a logind `idle` block inhibitor.
    pub idle_inhibited: bool,
    /// We hold the "caffeine" inhibitor (see [`Handle::set_caffeine`]).
    pub caffeine: bool,
    pub audio: audio::Audio,
    pub brightness: brightness::Levels,
    pub power: Power,
    pub media: Option<Media>,
}

/// A blocking stream of session state changes.
pub trait SessionSource {
    /// Block until the state changed, then return it. `Ok(None)` = the source is finished.
    fn next_change(&mut self) -> io::Result<Option<SessionState>>;
}

/// Shared state that the source threads update; only real changes are forwarded.
#[derive(Clone)]
pub(crate) struct Hub {
    state: Arc<Mutex<SessionState>>,
    tx: Sender<SessionState>,
}

impl Hub {
    pub(crate) fn new(initial: SessionState) -> (Self, Receiver<SessionState>) {
        let (tx, rx) = channel();
        (
            Self {
                state: Arc::new(Mutex::new(initial)),
                tx,
            },
            rx,
        )
    }

    pub(crate) fn snapshot(&self) -> SessionState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Apply `f`; if the state changed, publish it. `false` = the receiver is gone, stop.
    pub(crate) fn update(&self, f: impl FnOnce(&mut SessionState)) -> bool {
        let new = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let before = s.clone();
            f(&mut s);
            (*s != before).then(|| s.clone())
        };
        match new {
            Some(s) => self.tx.send(s).is_ok(),
            None => true,
        }
    }
}

/// The live session: logind + UPower + power-profiles on the system bus, MPRIS on the session bus.
pub struct Session {
    first: Option<SessionState>,
    rx: Receiver<SessionState>,
    handle: Handle,
}

/// Cloneable control/feeding side of a [`Session`]: sources without a signal to wait on push
/// their readings through it, and actions that change session state live here.
#[derive(Clone)]
pub struct Handle {
    hub: Hub,
    system: zbus::blocking::Connection,
    caffeine: Arc<Mutex<Option<zbus::zvariant::OwnedFd>>>,
}

impl Handle {
    /// Re-read backlight levels from sysfs. Sysfs brightness has no reliable change event, so
    /// call this on our own writes, on wake, and on a slow timer only while a slider is shown.
    /// `false` = the receiving side is gone.
    pub fn refresh_brightness(&self, sysfs: &brightness::Sysfs) -> bool {
        let levels = sysfs.levels();
        self.hub.update(|s| s.brightness = levels)
    }

    /// Hold (or release) a logind `idle` inhibitor, which idle daemons such as hypridle honour.
    /// The inhibitor dies with our connection, so a crashed agent can't leave the screen awake.
    pub fn set_caffeine(&self, on: bool) -> zbus::Result<()> {
        let mut held = self.caffeine.lock().unwrap_or_else(|e| e.into_inner());
        if on && held.is_none() {
            *held = Some(logind::inhibit_idle(
                &self.system,
                "Sliver",
                "Caffeine toggle",
            )?);
        } else if !on {
            *held = None; // dropping the fd releases the inhibitor
        }
        let now = held.is_some();
        drop(held);
        self.hub.update(|s| s.caffeine = now);
        Ok(())
    }
}

impl Session {
    /// Read everything once, then spawn the watcher threads. Sources that are missing (no
    /// UPower, no player) leave their fields at `None`/default; they never fail startup.
    /// The threads are detached; they end when the `Session` is dropped and the next signal
    /// arrives.
    pub fn start(system: zbus::blocking::Connection, session: zbus::blocking::Connection) -> Self {
        let sys_signals = subscribe(&system, &SYSTEM_RULES);
        let mpris_signals = subscribe(&session, &mpris::RULES);
        let mut initial = SessionState::default();
        let sys_watch = logind::read_into(&system, &mut initial);
        power::read_into(&system, &mut initial.power);
        let mpris_watch = mpris::Tracker::new(&session, &mut initial);
        let (hub, rx) = Hub::new(initial.clone());
        let handle = Handle {
            hub: hub.clone(),
            system: system.clone(),
            caffeine: Arc::default(),
        };
        let h = hub.clone();
        std::thread::Builder::new()
            .name("sliver-session-system".into())
            .spawn(move || watch_system(system, sys_signals, sys_watch, h))
            .expect("spawn thread");
        std::thread::Builder::new()
            .name("sliver-session-mpris".into())
            .spawn(move || mpris_watch.run(session, mpris_signals, hub))
            .expect("spawn thread");
        Self {
            first: Some(initial),
            rx,
            handle,
        }
    }

    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }
}

impl SessionSource for Session {
    fn next_change(&mut self) -> io::Result<Option<SessionState>> {
        if let Some(s) = self.first.take() {
            return Ok(Some(s));
        }
        Ok(self.rx.recv().ok())
    }
}

const SYSTEM_RULES: [&str; 4] = [
    "type='signal',sender='org.freedesktop.login1'",
    "type='signal',sender='org.freedesktop.UPower'",
    "type='signal',sender='org.freedesktop.UPower.PowerProfiles'",
    "type='signal',sender='net.hadess.PowerProfiles'",
];

fn watch_system(
    conn: zbus::blocking::Connection,
    signals: Signals,
    logind: logind::Watch,
    hub: Hub,
) {
    for msg in signals {
        // Signals carry the sender's unique name, so route by object path instead.
        let path = msg
            .header()
            .path()
            .map(|p| p.as_str().to_owned())
            .unwrap_or_default();
        let keep = if path.starts_with("/org/freedesktop/login1") {
            logind.on_message(&conn, &msg, &hub)
        } else {
            let mut p = hub.snapshot().power;
            power::read_into(&conn, &mut p);
            hub.update(|s| s.power = p)
        };
        if !keep {
            return;
        }
    }
}

/// Subscribe to `rules` on `conn`; the result yields the signals that arrive. Rules the bus
/// refuses are skipped (a missing service must not stop the others). Call this **before** the
/// initial read so no change between the read and the subscription is lost.
pub(crate) type Signals = Box<dyn Iterator<Item = zbus::Message> + Send>;

pub(crate) fn subscribe(conn: &zbus::blocking::Connection, rules: &[&str]) -> Signals {
    if let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(conn) {
        for r in rules {
            if let Ok(rule) = zbus::MatchRule::try_from(*r) {
                let _ = bus.add_match_rule(rule);
            }
        }
    }
    Box::new(
        zbus::blocking::MessageIterator::from(conn.clone())
            .filter_map(Result::ok)
            .filter(|m| m.message_type() == zbus::message::Type::Signal),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_forwards_only_changes() {
        let (hub, rx) = Hub::new(SessionState::default());
        assert!(hub.update(|s| s.idle = false), "no-op is fine");
        assert!(rx.try_recv().is_err(), "no-op is not published");
        assert!(hub.update(|s| s.idle = true));
        assert!(rx.try_recv().unwrap().idle);
        drop(rx);
        assert!(!hub.update(|s| s.locked = true), "receiver gone -> stop");
    }

    #[test]
    fn battery_state_mapping() {
        assert_eq!(BatteryState::from_upower(4), BatteryState::FullyCharged);
        assert_eq!(BatteryState::from_upower(2), BatteryState::Discharging);
        assert_eq!(BatteryState::from_upower(99), BatteryState::Unknown);
    }
}
