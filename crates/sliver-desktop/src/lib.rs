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

use serde::{Deserialize, Serialize};
use std::io;

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
}

/// A blocking stream of desktop state changes.
pub trait DesktopSource {
    /// Block until the state changed, then return it. `Ok(None)` = the source is finished
    /// (compositor closed the connection, or the script ran out).
    fn next_change(&mut self) -> io::Result<Option<DesktopState>>;
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
