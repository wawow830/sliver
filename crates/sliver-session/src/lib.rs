//! `sliver-session`: session and system state that is not the compositor's.
//!
//! Lock/idle/sleep (logind), power (UPower, power-profiles-daemon) and media (MPRIS), as one
//! [`SessionState`]. Compositor state lives in `sliver-desktop`.
//!
//! - Sources wake on a socket read, a D-Bus signal or a long-lived child's output. Where only
//!   polling exists, they poll only while a module showing the value is on the lit bar.
//!   [`Handle::set_awake`] drops the NetworkManager, BlueZ and UPower match rules while the bar is
//!   off; logind's stay, since lock and sleep must always work.
//! - Subscribe before the first read, or a change between the two is lost.
//! - A missing service leaves its fields unknown; a context key with no known value is left out,
//!   so it never matches a rule.
//! - Reads use `NoAutoStart`, so reading never starts a service.
//!
//! [`fake::ScriptedSession`] replays states; the real sources are tested against mock services on
//! a private `dbus-daemon` (see `tests/`).

pub mod appearance;
pub mod art;
pub mod audio;
pub mod bluetooth;
pub mod brightness;
pub mod capture;
pub mod failed;
pub mod fake;
pub mod logind;
pub mod menu;
pub mod mpris;
pub mod network;
pub mod notifications;
pub mod power;
pub(crate) mod props;
pub mod screensaver;
pub mod tray;

use serde::{Deserialize, Serialize};
use std::io;
use std::sync::{Arc, Condvar, Mutex};

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
    /// Full bus name, e.g. `org.mpris.MediaPlayer2.firefox.instance_1_42`.
    pub player: String,
    /// Human name (`Identity`), e.g. "Mozilla Firefox".
    pub identity: String,
    pub status: PlaybackStatus,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub art_url: Option<String>,
    pub length_us: Option<i64>,
    /// Position when this state was read. MPRIS never signals position changes, so extrapolate
    /// from `position_at_ms` while `Playing`.
    pub position_us: Option<i64>,
    pub can_play_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    /// `mpris:trackid`, needed to seek (`SetPosition`).
    pub track_id: Option<String>,
    /// When `position_us` was read (unix ms).
    pub position_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    /// logind `LockedHint` plus `Lock`/`Unlock` edges. A hint the locker sets, not a fact: the
    /// agent weighs it against the desktop's own signal and running lockers (`plausibly_locked`).
    pub locked: bool,
    /// The desktop's own lock/screen-saver state (`ActiveChanged`), when it has one. It overrides
    /// `locked`.
    pub screensaver: Option<bool>,
    /// logind `IdleHint`.
    pub idle: bool,
    /// Between `PrepareForSleep(true)` and `PrepareForSleep(false)`.
    pub sleeping: bool,
    /// Anyone (including us) holds a logind `idle` block inhibitor.
    pub idle_inhibited: bool,
    /// We hold the "caffeine" inhibitor (see [`Handle::set_caffeine`]).
    pub caffeine: bool,
    pub audio: audio::Audio,
    /// Who is recording the microphone, the camera or the screen (PipeWire; see [`capture`]).
    pub capture: capture::Capture,
    pub notifications: notifications::Notifications,
    pub appearance: appearance::Appearance,
    /// StatusNotifierItem tray, in registration order (empty until `Handle::start_tray`).
    pub tray: Vec<tray::TrayItem>,
    pub brightness: brightness::Levels,
    pub power: Power,
    pub network: network::Network,
    pub bluetooth: bluetooth::Bluetooth,
    pub media: Option<Media>,
    /// Every real player, the chosen one first (for a player picker).
    pub players: Vec<Media>,
    /// Failed systemd units (only watched once [`Handle::start_failed_units`] ran).
    #[serde(default)]
    pub failed_units: failed::FailedUnits,
}

pub trait SessionSource {
    /// Blocks until the state changed. `Ok(None)`: the source is finished.
    fn next_change(&mut self) -> io::Result<Option<SessionState>>;
}

/// State the source threads update. A reader gets only the latest state, not every step to it.
#[derive(Clone)]
pub(crate) struct Hub(Arc<(Mutex<Shared>, Condvar)>);

struct Shared {
    state: SessionState,
    version: u64,
    /// The reader ([`Latest`]) is gone: sources stop.
    closed: bool,
}

/// The reading end of a [`Hub`]. Dropping it tells the sources to stop.
pub(crate) struct Latest {
    hub: Hub,
    seen: u64,
}

impl Hub {
    pub(crate) fn new(initial: SessionState) -> (Self, Latest) {
        let shared = Shared {
            state: initial,
            version: 0,
            closed: false,
        };
        let hub = Hub(Arc::new((Mutex::new(shared), Condvar::new())));
        (hub.clone(), Latest { hub, seen: 0 })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.0.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn snapshot(&self) -> SessionState {
        self.lock().state.clone()
    }

    /// Apply `f`; if the state changed, the reader wakes. `false` = the reader is gone, stop.
    pub(crate) fn update(&self, f: impl FnOnce(&mut SessionState)) -> bool {
        let mut s = self.lock();
        if s.closed {
            return false;
        }
        let before = s.state.clone();
        f(&mut s.state);
        if s.state != before {
            s.version += 1;
            self.0.1.notify_all();
        }
        true
    }
}

impl Latest {
    /// The state, once it's newer than the last one returned. `None`: the hub was closed.
    fn wait(&mut self) -> Option<SessionState> {
        let mut s = self.hub.lock();
        while s.version == self.seen && !s.closed {
            s = self.hub.0.1.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        (!s.closed).then(|| {
            self.seen = s.version;
            s.state.clone()
        })
    }

    #[cfg(test)]
    fn try_take(&mut self) -> Option<SessionState> {
        let s = self.hub.lock();
        (s.version != self.seen).then(|| {
            self.seen = s.version;
            s.state.clone()
        })
    }
}

impl Drop for Latest {
    fn drop(&mut self) {
        self.hub.lock().closed = true;
    }
}

/// The live session: logind + UPower + power-profiles on the system bus, MPRIS on the session bus.
pub struct Session {
    first: Option<SessionState>,
    latest: Latest,
    handle: Handle,
}

/// Cloneable side of a [`Session`]: sources without a signal to wait on push their readings
/// through it, and actions that change session state live here.
#[derive(Clone)]
pub struct Handle {
    hub: Hub,
    system: zbus::blocking::Connection,
    caffeine: Arc<Mutex<Option<zbus::zvariant::OwnedFd>>>,
    /// Are the chatty system-bus subscriptions on? (see [`Handle::set_awake`])
    awake: Arc<std::sync::atomic::AtomicBool>,
    awake_gate: Arc<Mutex<()>>,
    /// What the bus actually has subscribed (changes only under `awake_gate`).
    chatty_on: Arc<std::sync::atomic::AtomicBool>,
    /// KDE's `Inhibit` cookie while Do Not Disturb is on (see [`Handle::set_dnd`]).
    dnd_cookie: Arc<Mutex<Option<u32>>>,
    failed_started: Arc<std::sync::atomic::AtomicBool>,
    /// The PipeWire capture follower's on/off switch ([`Handle::start_capture`]).
    capture_gate: Arc<capture::Gate>,
    capture_started: Arc<std::sync::atomic::AtomicBool>,
}

impl Handle {
    /// Re-read backlight levels from sysfs. Sysfs brightness has no reliable change event, so
    /// call this on our own writes, on wake, and on a slow timer only while a slider is shown.
    /// `false`: the receiving side is gone.
    pub fn refresh_brightness(&self, sysfs: &brightness::Sysfs) -> bool {
        let levels = sysfs.levels();
        self.hub.update(|s| s.brightness = levels)
    }

    /// The bar is on (`true`) or off (`false`). While off, the NetworkManager, BlueZ and UPower
    /// match rules are dropped; on wake all three are re-read. The bus work runs on its own thread.
    pub fn set_awake(&self, on: bool) {
        use std::sync::atomic::Ordering;
        if self.awake.swap(on, Ordering::SeqCst) == on {
            return;
        }
        self.capture_gate.set(on);
        let me = self.clone();
        std::thread::spawn(move || {
            let _one_at_a_time = me.awake_gate.lock().unwrap_or_else(|e| e.into_inner());
            // Whatever was asked last wins if calls piled up.
            let on = me.awake.load(Ordering::SeqCst);
            let Ok(bus) = zbus::blocking::fdo::DBusProxy::new(&me.system) else {
                return;
            };
            let have = me.chatty_on.swap(on, Ordering::SeqCst);
            if have == on {
                return;
            }
            for r in &SYSTEM_RULES[..CHATTY_RULES] {
                if let Ok(rule) = zbus::MatchRule::try_from(*r) {
                    let _ = if on {
                        bus.add_match_rule(rule)
                    } else {
                        bus.remove_match_rule(rule)
                    };
                }
            }
            if on {
                // Changes while we weren't listening.
                let mut n = me.hub.snapshot().network;
                network::read_into(&me.system, &mut n);
                me.hub.update(|s| s.network = n);
                let mut b = me.hub.snapshot().bluetooth;
                bluetooth::read_into(&me.system, &mut b);
                me.hub.update(|s| s.bluetooth = b);
                let mut p = me.hub.snapshot().power;
                power::read_into(&me.system, &mut p);
                me.hub.update(|s| s.power = p);
            }
        });
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
    /// Read everything once, then spawn detached watcher threads, which end when the `Session` is
    /// dropped and the next signal arrives. Missing sources leave their fields at `None`/default.
    pub fn start(system: zbus::blocking::Connection, session: zbus::blocking::Connection) -> Self {
        let sys_signals = subscribe(&system, &SYSTEM_RULES);
        let mpris_signals = subscribe(&session, &mpris::RULES);
        let mut initial = SessionState::default();
        let sys_watch = logind::read_into(&system, &mut initial);
        power::read_into(&system, &mut initial.power);
        network::read_into(&system, &mut initial.network);
        bluetooth::read_into(&system, &mut initial.bluetooth);
        let mpris_watch = mpris::Tracker::new(&session, &mut initial);
        let (hub, latest) = Hub::new(initial.clone());
        let handle = Handle {
            hub: hub.clone(),
            system: system.clone(),
            caffeine: Arc::default(),
            awake: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            awake_gate: Arc::default(),
            chatty_on: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            dnd_cookie: Arc::default(),
            failed_started: Arc::default(),
            capture_gate: capture::Gate::new(),
            capture_started: Arc::default(),
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
            latest,
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
        Ok(self.latest.wait())
    }
}

/// How many of the first [`SYSTEM_RULES`] are dropped while the bar is off
/// ([`Handle::set_awake`]).
const CHATTY_RULES: usize = 3;

const SYSTEM_RULES: [&str; 6] = [
    "type='signal',sender='org.freedesktop.NetworkManager'",
    "type='signal',sender='org.bluez'",
    "type='signal',sender='org.freedesktop.UPower'",
    "type='signal',sender='org.freedesktop.login1'",
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
        } else if path.starts_with("/org/freedesktop/NetworkManager") {
            let mut n = hub.snapshot().network;
            network::read_into(&conn, &mut n);
            hub.update(|s| s.network = n)
        } else if path.starts_with("/org/bluez") {
            let mut b = hub.snapshot().bluetooth;
            bluetooth::read_into(&conn, &mut b);
            hub.update(|s| s.bluetooth = b)
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

/// Signals from `subscribe`. Rules the bus refuses are skipped. Subscribe before the initial
/// read, or a change between the read and the subscription is lost.
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
        let (hub, mut latest) = Hub::new(SessionState::default());
        assert!(hub.update(|s| s.idle = false), "no-op is fine");
        assert!(latest.try_take().is_none(), "no-op is not published");
        assert!(hub.update(|s| s.idle = true));
        assert!(latest.try_take().unwrap().idle);
        assert!(latest.try_take().is_none(), "each state is read once");
        drop(latest);
        assert!(!hub.update(|s| s.locked = true), "reader gone -> stop");
    }

    /// Changes the reader hasn't read yet don't queue: it reads the latest state, once.
    #[test]
    fn hub_coalesces() {
        let (hub, mut latest) = Hub::new(SessionState::default());
        for _ in 0..1000 {
            hub.update(|s| s.idle = !s.idle);
        }
        hub.update(|s| s.locked = true);
        let s = latest.wait().unwrap();
        assert!(s.locked && !s.idle);
        assert!(latest.try_take().is_none());
    }

    #[test]
    fn battery_state_mapping() {
        assert_eq!(BatteryState::from_upower(4), BatteryState::FullyCharged);
        assert_eq!(BatteryState::from_upower(2), BatteryState::Discharging);
        assert_eq!(BatteryState::from_upower(99), BatteryState::Unknown);
    }
}
