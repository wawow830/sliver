//! `sliver-desktop`: what the desktop tells the session agent.
//!
//! The agent decides which Touch Bar layout to show from a [`DesktopState`] (focused app,
//! workspace, keyboard layout, ...). Each compositor gets a backend implementing
//! [`DesktopSource`]; [`hyprland`] is the first. [`fake::ScriptedSource`] drives tests without a
//! compositor. Backends are blocking and runtime-agnostic (plain threads, no busy loops): they
//! sleep in `read(2)` until the compositor says something changed.
//!
//! Survey and rationale for the other backends: `docs/integration.md`.

pub mod fake;
pub mod hyprland;
pub mod sway;

use serde::{Deserialize, Serialize};
use std::io;
use std::ops::ControlFlow;

/// The focused window, as far as the compositor exposes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Compositor-specific opaque id (Hyprland: hex address without `0x`).
    pub address: String,
    /// Wayland `app_id` / X11 `WM_CLASS` class. Match this to a `.desktop` id for icons.
    pub class: String,
    pub title: String,
    pub pid: Option<u32>,
    pub floating: bool,
    pub fullscreen: bool,
    pub xwayland: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: i64,
    /// Workspaces can be named; the name equals the id as a string when they aren't.
    pub name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Hyprland,
    Sway,
    Niri,
    Gnome,
    Kde,
    X11,
    #[default]
    Unknown,
}

impl Backend {
    /// Pick the backend from the session environment. `get` looks up an env var (inject for tests).
    ///
    /// Compositor-specific variables win over `XDG_CURRENT_DESKTOP`, which is a colon list
    /// (`GNOME:ubuntu`, `KDE`, ...) that may also be set under nested sessions.
    pub fn detect(get: impl Fn(&str) -> Option<String>) -> Backend {
        let set = |k: &str| get(k).is_some_and(|v| !v.is_empty());
        if set("HYPRLAND_INSTANCE_SIGNATURE") {
            return Backend::Hyprland;
        }
        if set("NIRI_SOCKET") {
            return Backend::Niri;
        }
        if set("SWAYSOCK") {
            return Backend::Sway;
        }
        let desktop = get("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase();
        for part in desktop.split(':') {
            match part {
                "gnome" => return Backend::Gnome,
                "kde" => return Backend::Kde,
                "hyprland" => return Backend::Hyprland,
                "sway" => return Backend::Sway,
                "niri" => return Backend::Niri,
                _ => {}
            }
        }
        if set("DISPLAY") && !set("WAYLAND_DISPLAY") {
            return Backend::X11;
        }
        Backend::Unknown
    }
}

/// Everything the compositor tells us. Only fields that changed behaviour-relevantly are here;
/// system signals (power, media, lock...) come from other sources, see `docs/integration.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopState {
    /// False while the compositor connection is down (the agent should fall back to defaults).
    pub connected: bool,
    pub backend: Backend,
    pub window: Option<Window>,
    pub workspace: Option<Workspace>,
    /// Name of the open special/scratchpad workspace on the focused monitor, if any.
    pub special_workspace: Option<String>,
    pub monitor: Option<String>,
    /// Active keyboard layout, human readable (`English (US)`).
    pub keyboard_layout: Option<String>,
    /// Active keybind submap / sway mode. `None` = default.
    pub submap: Option<String>,
    /// Something is screen-sharing (a recording indicator widget can use it).
    pub screencast: bool,
    /// Names of the enabled outputs (`eDP-1`, `HDMI-A-1`, ...).
    pub monitor_names: Vec<String>,
    /// Number of enabled outputs.
    pub monitors: u32,
    /// Any enabled output that is not the built-in panel (see [`is_internal_output`]).
    pub external_display: bool,
}

/// Is this connector the laptop's own panel? (`eDP-*`, `LVDS-*`, `DSI-*`.)
pub fn is_internal_output(name: &str) -> bool {
    ["eDP", "LVDS", "DSI"].iter().any(|p| name.starts_with(p))
}

impl DesktopState {
    /// Set the output list and the derived `monitors` / `external_display`.
    pub fn set_monitors(&mut self, names: Vec<String>) {
        self.monitors = names.len() as u32;
        self.external_display = names.iter().any(|n| !is_internal_output(n));
        self.monitor_names = names;
    }
}

/// A blocking stream of desktop state changes.
pub trait DesktopSource {
    /// The state as of the last change (the snapshot right after connecting, initially).
    fn state(&self) -> &DesktopState;

    /// Block until the state changed, then return it. `Ok(None)` = the source is finished
    /// (compositor closed the connection, or the script ran out).
    fn next_change(&mut self) -> io::Result<Option<DesktopState>>;
}

/// Run until `on_change` breaks: connect, deliver the initial state and every change, and on
/// disconnect deliver a `connected: false` state, then retry with backoff (0.25s .. 5s). The
/// disconnected state is reported once, even if the compositor never comes back.
pub fn watch<S: DesktopSource>(
    mut connect: impl FnMut() -> io::Result<S>,
    mut on_change: impl FnMut(&DesktopState) -> ControlFlow<()>,
) {
    use std::time::Duration;
    let mut backoff = Duration::from_millis(250);
    let mut told_disconnected = false;
    loop {
        match connect() {
            Ok(mut src) => {
                backoff = Duration::from_millis(250);
                if on_change(src.state()).is_break() {
                    return;
                }
                while let Ok(Some(s)) = src.next_change() {
                    if on_change(&s).is_break() {
                        return;
                    }
                }
                told_disconnected = true;
                if on_change(&DesktopState::default()).is_break() {
                    return;
                }
            }
            Err(_) if !told_disconnected => {
                told_disconnected = true;
                if on_change(&DesktopState::default()).is_break() {
                    return;
                }
            }
            Err(_) => {}
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn detect_prefers_compositor_vars() {
        let e = env(&[
            ("HYPRLAND_INSTANCE_SIGNATURE", "x"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
        ]);
        assert_eq!(Backend::detect(e), Backend::Hyprland);
        assert_eq!(Backend::detect(env(&[("SWAYSOCK", "/s")])), Backend::Sway);
        assert_eq!(
            Backend::detect(env(&[("NIRI_SOCKET", "/s")])),
            Backend::Niri
        );
    }

    #[test]
    fn external_display_derivation() {
        let mut s = DesktopState::default();
        s.set_monitors(vec!["eDP-1".into()]);
        assert_eq!((s.monitors, s.external_display), (1, false));
        s.set_monitors(vec!["eDP-1".into(), "HDMI-A-1".into()]);
        assert_eq!((s.monitors, s.external_display), (2, true));
        s.set_monitors(vec![]);
        assert_eq!((s.monitors, s.external_display), (0, false));
    }

    #[test]
    fn detect_by_desktop_and_x11() {
        assert_eq!(
            Backend::detect(env(&[("XDG_CURRENT_DESKTOP", "ubuntu:GNOME")])),
            Backend::Gnome
        );
        assert_eq!(
            Backend::detect(env(&[("XDG_CURRENT_DESKTOP", "KDE")])),
            Backend::Kde
        );
        assert_eq!(Backend::detect(env(&[("DISPLAY", ":0")])), Backend::X11);
        assert_eq!(
            Backend::detect(env(&[("DISPLAY", ":0"), ("WAYLAND_DISPLAY", "wayland-1")])),
            Backend::Unknown
        );
        assert_eq!(Backend::detect(env(&[])), Backend::Unknown);
    }
}
