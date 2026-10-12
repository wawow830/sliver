//! The rule context. The compositor and session keys come from `sliver-feeds`
//! (`Aggregate::context`); the agent adds what only it knows: time, weekday, the physical Esc
//! key, the current layer, and a sanity check on logind's lock hint.

use chrono::{DateTime, Local};
use sliver_config::Context;
use sliver_session::PlaybackStatus;

pub fn media_status(s: PlaybackStatus) -> &'static str {
    match s {
        PlaybackStatus::Playing => "playing",
        PlaybackStatus::Paused => "paused",
        PlaybackStatus::Stopped => "stopped",
    }
}

/// `base` (from the feeds) plus the agent's own keys. `app_controls`: an app fills App controls.
pub fn build(
    mut base: Context,
    now: DateTime<Local>,
    physical_esc: bool,
    layer: &str,
    app_controls: bool,
) -> Context {
    base.set("physical_esc", physical_esc);
    base.set("app_controls", app_controls);
    base.set("layer", layer);
    base.set("time", now.format("%H:%M").to_string());
    base.set("weekday", now.format("%a").to_string().to_ascii_lowercase());
    base
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

/// Whether the screen is locked, from every source we have, most trustworthy first:
///
/// 1. The desktop's own `ActiveChanged` (GNOME, KDE, anything with `org.freedesktop.ScreenSaver`).
/// 2. On GNOME and KDE without it: logind's `LockedHint`, which those shells keep honest.
/// 3. Elsewhere, whether a known locker process is running: lockers started by a keybind or
///    `swayidle` never touch logind (the hint stays `false`: a leak), and others never clear it
///    (integration.md §3: the hint stays `yes` after unlocking: a stuck privacy layout). The
///    hint alone is not believed.
///
/// `running` lists process names (`/proc/*/comm`; injectable for tests); it is only called when
/// the answer needs it.
pub fn plausibly_locked(
    hint: bool,
    screensaver: Option<bool>,
    backend: sliver_desktop::Backend,
    running: impl FnOnce() -> Vec<String>,
) -> bool {
    use sliver_desktop::Backend::*;
    if let Some(active) = screensaver {
        return active;
    }
    match backend {
        Gnome | Kde => hint,
        _ => running().iter().any(|p| LOCKERS.contains(&p.as_str())),
    }
}

/// [`plausibly_locked`] with the process scan cached for a moment: world snapshots are built on
/// every render, and `/proc` has hundreds of entries. The cache is dropped on [`LockWatch::wake`]
/// so a bar waking up never shows a stale "unlocked".
#[derive(Debug, Default)]
pub struct LockWatch {
    cache: std::sync::Mutex<Option<(std::time::Instant, bool)>>,
}

/// How long a process scan result is reused.
const SCAN_TTL: std::time::Duration = std::time::Duration::from_millis(1500);

impl LockWatch {
    pub fn locked(
        &self,
        hint: bool,
        screensaver: Option<bool>,
        backend: sliver_desktop::Backend,
    ) -> bool {
        self.locked_with(hint, screensaver, backend, process_names)
    }

    pub fn locked_with(
        &self,
        hint: bool,
        screensaver: Option<bool>,
        backend: sliver_desktop::Backend,
        running: impl FnOnce() -> Vec<String>,
    ) -> bool {
        use sliver_desktop::Backend::*;
        match (screensaver, backend) {
            (Some(active), _) => active,
            (None, Gnome | Kde) => hint,
            (None, _) => self.locker_running(running),
        }
    }

    fn locker_running(&self, running: impl FnOnce() -> Vec<String>) -> bool {
        let now = std::time::Instant::now();
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, v)) = *cache
            && now.saturating_duration_since(t) < SCAN_TTL
        {
            return v;
        }
        let v = running().iter().any(|p| LOCKERS.contains(&p.as_str()));
        *cache = Some((now, v));
        v
    }

    /// The display woke up: look again.
    pub fn wake(&self) {
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
mod tests {
    use super::*;
    use chrono::TimeZone;
    use sliver_config::{CtxValue, load_str};
    use sliver_desktop::Backend;

    #[test]
    fn agent_keys_on_top_of_feeds() {
        let now = Local.with_ymd_and_hms(2026, 10, 12, 23, 30, 0).unwrap();
        let ctx = build(
            Context::new().with("app_id", "kitty"),
            now,
            true,
            "base",
            false,
        );
        let cfg = load_str(
            r#"
            [[rules]]
            when = { app-id = "kitty", time = { regex = "^23:" } }
            layout = "fkeys"
            "#,
            std::path::Path::new("."),
        )
        .config;
        assert_eq!(cfg.resolve("base", &ctx).unwrap().layout, "fkeys");
        assert!(matches!(ctx.get("weekday"), Some(CtxValue::Str(s)) if s == "mon"));
    }

    #[test]
    fn app_controls_key_lets_layouts_fill_the_gap() {
        let now = Local.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap();
        let cfg = load_str(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["app-controls", "key/F1"]
            [modules."key/F1"]
            when = { app-controls = false }
            "#,
            std::path::Path::new("."),
        );
        assert!(cfg.diagnostics.warnings.is_empty(), "{:?}", cfg.diagnostics);
        let n = |app: bool| {
            let r = cfg
                .config
                .resolve("base", &build(Context::new(), now, true, "base", app))
                .unwrap();
            r.zones[0].modules.len()
        };
        assert_eq!((n(false), n(true)), (2, 1));
    }

    #[test]
    fn lock_sources_in_order_of_trust() {
        let none = || Vec::new();
        let hyprlock = || vec!["bash".to_string(), "hyprlock".to_string()];
        // a stale hint on Hyprland is not believed; a running locker is, hint or not
        assert!(!plausibly_locked(true, None, Backend::Hyprland, none));
        assert!(plausibly_locked(true, None, Backend::Hyprland, hyprlock));
        assert!(
            plausibly_locked(false, None, Backend::Sway, || vec!["swaylock".into()]),
            "a locker started by a keybind never touches logind"
        );
        // GNOME and KDE keep the hint honest
        assert!(plausibly_locked(true, None, Backend::Gnome, none));
        assert!(!plausibly_locked(false, None, Backend::Kde, hyprlock));
        // the desktop's own ActiveChanged beats everything
        assert!(plausibly_locked(false, Some(true), Backend::Hyprland, none));
        assert!(!plausibly_locked(true, Some(false), Backend::Gnome, none));
        assert!(!plausibly_locked(
            true,
            Some(false),
            Backend::Hyprland,
            hyprlock
        ));
    }

    #[test]
    fn lock_watch_caches_the_scan_until_woken() {
        use std::cell::Cell;
        let w = LockWatch::default();
        let scans = Cell::new(0);
        let locked = |names: &'static [&'static str]| {
            w.locked_with(false, None, Backend::Sway, || {
                scans.set(scans.get() + 1);
                names.iter().map(|s| s.to_string()).collect()
            })
        };
        assert!(!locked(&["bash"]));
        assert!(!locked(&["swaylock"]), "cached: still the first answer");
        assert_eq!(scans.get(), 1);
        w.wake();
        assert!(locked(&["swaylock"]), "a wake looks again");
        assert_eq!(scans.get(), 2);
        // no scan at all when the shell says
        assert!(w.locked_with(false, Some(true), Backend::Sway, || panic!("scanned")));
    }
}
