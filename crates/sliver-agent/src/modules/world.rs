//! What modules read: the [`World`] (desktop, session, outputs, time) and the per-cell
//! [`UiState`] that survives re-resolution.

use super::*;

/// The emoji picker's recents, shared by the world (drawing) and the agent (picking).
#[derive(Clone, Default)]
pub struct EmojiHandle(std::sync::Arc<std::sync::Mutex<sliver_emoji::Recents>>);

impl std::fmt::Debug for EmojiHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmojiHandle")
    }
}

impl EmojiHandle {
    /// Recents saved under `$XDG_STATE_HOME/sliver/emoji-recents`.
    pub fn persistent() -> EmojiHandle {
        let path = sliver_emoji::Recents::default_path(|k| std::env::var(k).ok());
        let r = path.map_or_else(sliver_emoji::Recents::new, sliver_emoji::Recents::at);
        EmojiHandle(std::sync::Arc::new(std::sync::Mutex::new(r)))
    }

    pub fn items(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items()
            .to_vec()
    }

    pub fn push(&self, text: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    }

    /// `e` in the skin tone last picked for it (or plain).
    pub fn preferred(&self, e: &sliver_emoji::Emoji) -> String {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .preferred(e)
    }
}

/// Everything modules and rules can read.
#[derive(Debug, Clone)]
pub struct World {
    /// When this world was seen: what clocks show, and what transitions and timers count from.
    pub now: crate::now::Now,
    pub session: SessionState,
    pub desktop: DesktopState,
    /// Rule context from the feeds (compositor + session keys); the agent adds its own.
    pub context: Context,
    /// Latest output of each user module (`custom/*` with `exec`), by module key.
    pub outputs: HashMap<String, Output>,
    /// What apps publish for the App controls / Status items placeholders.
    pub published: crate::published::Published,
    /// cpu, memory, load, temperature, disk and network throughput, as read for this frame.
    pub stats: crate::stats::Readings,
    /// Recently used emoji (the picker's first tab).
    pub emoji: EmojiHandle,
    /// Local file of the current player's album art (fetched by the feeds).
    pub media_art: Option<std::path::PathBuf>,
    /// Installed applications, for taskbar icons.
    pub apps: std::sync::Arc<sliver_icons::desktop::DesktopEntries>,
}

impl World {
    /// A world with no session or desktop data (headless previews, tests).
    pub fn at(wall: DateTime<Local>) -> World {
        World {
            now: crate::now::Now::at(wall),
            session: SessionState::default(),
            desktop: DesktopState::default(),
            context: Context::new(),
            outputs: HashMap::new(),
            published: Default::default(),
            stats: Default::default(),
            apps: Default::default(),
            emoji: Default::default(),
            media_art: None,
        }
    }

    /// Plausible sample data (tests), so data modules show something instead of
    /// hiding: battery, volume, brightness, now playing, focused window.
    pub fn sample(now: DateTime<Local>) -> World {
        use sliver_session::{Media, PlaybackStatus};
        let mut w = World::at(now);
        let s = &mut w.session;
        s.power.on_battery = Some(true);
        s.power.battery_pct = Some(76);
        s.power.battery_state = BatteryState::Discharging;
        s.power.profile = Some("balanced".into());
        s.audio.volume_pct = Some(40);
        s.audio.muted = Some(false);
        s.audio.mic_volume_pct = Some(80);
        s.audio.mic_muted = Some(false);
        s.brightness.screen_pct = Some(70);
        s.brightness.keyboard_pct = Some(30);
        s.media = Some(Media {
            player: "org.mpris.MediaPlayer2.preview".into(),
            identity: "Music".into(),
            status: PlaybackStatus::Playing,
            title: "Song Title".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            ..Media::default()
        });
        let d = &mut w.desktop;
        d.connected = true;
        d.keyboard_layout = Some("English (US)".into());
        d.window = Some(sliver_desktop::Window {
            class: "firefox".into(),
            title: "Mozilla Firefox".into(),
            ..Default::default()
        });
        w
    }
}

/// Per-module UI state that survives re-resolution, keyed by [`Cell::key`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiState {
    /// `format-alt` toggled on.
    pub show_alt: bool,
    /// A drawer group is open.
    pub expanded: bool,
    /// `timer` modules: the countdown.
    pub timer: Option<TimerState>,
    /// `emoji` strips: which tab and how far scrolled.
    pub emoji: EmojiUi,
    /// `mpris`: the player the user picked (hold), instead of the most recently active one.
    pub player: Option<String>,
    /// `graph`: the samples to draw, oldest first, 0..=1 (the agent keeps them, see
    /// [`graph_sample`]), and how many fit.
    pub graph: Option<(Vec<f32>, usize)>,
    /// `clock` with `timezones`: how far the user swiped through the list (wraps around).
    pub tz: i32,
}

/// Where an emoji strip is: tab 0 = recents, 1..=9 = the Unicode groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmojiUi {
    pub tab: usize,
    pub offset: usize,
    /// Choosing a skin tone for this emoji (its plain text): the page shows its variants.
    pub tones: Option<String>,
    /// Searching: the letters typed so far (`Some("")` = search is open and empty).
    pub search: Option<String>,
}

/// A countdown that survives re-resolution (it lives in [`UiState`], keyed by the cell).
#[derive(Debug, Clone, PartialEq)]
pub struct TimerState {
    /// Time left when it was last started, paused or created.
    pub remaining: std::time::Duration,
    /// When the current run started (monotonic µs); `None` while paused or finished.
    pub running_since: Option<u64>,
    pub finished: bool,
    /// Bumped on every start/pause so a stale "done" deadline is ignored.
    pub generation: u64,
}

impl TimerState {
    pub fn new(total: std::time::Duration) -> TimerState {
        TimerState {
            remaining: total,
            running_since: None,
            finished: false,
            generation: 0,
        }
    }

    /// What is left at `now_us` (monotonic).
    pub fn left(&self, now_us: u64) -> std::time::Duration {
        match self.running_since {
            Some(t) => self
                .remaining
                .saturating_sub(std::time::Duration::from_micros(now_us.saturating_sub(t))),
            None => self.remaining,
        }
    }
}

/// `MM:SS`, or `H:MM:SS` from an hour up.
pub fn clock_text(d: std::time::Duration) -> String {
    let s = d.as_secs() + u64::from(d.subsec_nanos() > 0); // round up: 0:00 means done
    match s {
        0..=3599 => format!("{:02}:{:02}", s / 60, s % 60),
        _ => format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60),
    }
}
