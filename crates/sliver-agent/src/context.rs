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

/// Whether to believe logind's `LockedHint` (integration.md §3: it's verified to read `true` on
/// an unlocked Hyprland session). On compositors whose lockers are separate programs, trust it
/// only while one of them runs. GNOME and KDE maintain the hint, so trust it there.
/// `running` lists process names (`/proc/*/comm`; injectable for tests).
pub fn plausibly_locked(
    hint: bool,
    backend: sliver_desktop::Backend,
    running: impl FnOnce() -> Vec<String>,
) -> bool {
    use sliver_desktop::Backend::*;
    match (hint, backend) {
        (false, _) => false,
        (true, Gnome | Kde) => true,
        (true, _) => running().iter().any(|p| LOCKERS.contains(&p.as_str())),
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
    fn stale_hint_on_hyprland_is_ignored() {
        let none = || Vec::new();
        assert!(!plausibly_locked(true, Backend::Hyprland, none));
        assert!(plausibly_locked(true, Backend::Hyprland, || vec![
            "bash".into(),
            "hyprlock".into()
        ]));
        assert!(plausibly_locked(true, Backend::Gnome, none));
        assert!(!plausibly_locked(false, Backend::Gnome, none));
    }
}
