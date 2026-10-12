//! Icon names from config (`icon = "media-playback-start-symbolic"`, a path, or a built-in name)
//! to drawable [`sliver_render::Icon`]s.
//!
//! Lookup order: a path (contains `/`), the XDG icon theme (sliver-icons), then sliver-render's
//! built-in Material icons, by name or by an XDG alias. The built-ins mean the default layouts
//! always have icons, even with no icon theme installed.

use std::collections::HashMap;

use sliver_icons::{IconLoader, Query};
use sliver_render::Icon;

/// XDG icon names (without `-symbolic`) that have a built-in stand-in.
const BUILTIN_ALIASES: &[(&str, &str)] = &[
    ("display-brightness-low", "brightness_low"),
    ("display-brightness-high", "brightness_high"),
    ("display-brightness", "brightness_high"),
    ("keyboard-brightness-low", "backlight_low"),
    ("keyboard-brightness-high", "backlight_high"),
    ("keyboard-brightness", "backlight_high"),
    ("microphone-disabled", "mic_off"),
    ("microphone-sensitivity-muted", "mic_off"),
    ("system-search", "search"),
    ("media-skip-backward", "fast_rewind"),
    ("media-seek-backward", "fast_rewind"),
    ("media-playback-start", "play_pause"),
    ("media-playback-pause", "play_pause"),
    ("media-skip-forward", "fast_forward"),
    ("media-seek-forward", "fast_forward"),
    ("audio-volume-muted", "volume_off"),
    ("audio-volume-low", "volume_down"),
    ("audio-volume-medium", "volume_down"),
    ("audio-volume-high", "volume_up"),
    ("battery-full", "battery_full"),
    ("battery-good", "battery_5_bar"),
    ("battery-low", "battery_2_bar"),
    ("battery-caution", "battery_1_bar"),
    ("battery-empty", "battery_0_bar"),
];

pub struct Icons {
    loader: Option<IconLoader>,
    cache: HashMap<(String, Option<String>, u32), Option<Icon>>,
    /// Relative icon paths are relative to the config file's directory.
    base: Option<std::path::PathBuf>,
}

impl Icons {
    /// The user's icon theme, with built-in fallbacks.
    pub fn system() -> Icons {
        Icons {
            loader: Some(IconLoader::from_env()),
            cache: HashMap::new(),
            base: None,
        }
    }

    /// Only the built-in icons: deterministic, for tests and the headless preview.
    pub fn builtin_only() -> Icons {
        Icons {
            loader: None,
            cache: HashMap::new(),
            base: None,
        }
    }

    /// Where relative icon paths (`icons/x.png`, `logo.svg`) are looked up: the directory of
    /// the active config file. `None`: the working directory.
    pub fn set_base_dir(&mut self, dir: Option<&std::path::Path>) {
        if self.base.as_deref() != dir {
            self.base = dir.map(Into::into);
            self.cache.clear();
        }
    }

    /// Forget everything (theme or config changed).
    pub fn clear(&mut self) {
        self.cache.clear();
        if let Some(l) = &self.loader {
            l.clear_cache();
        }
    }

    pub fn get(&mut self, name: &str, theme: Option<&str>, size: u32) -> Option<Icon> {
        let key = (name.to_owned(), theme.map(str::to_owned), size);
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let icon = self.lookup(name, theme, size);
        self.cache.insert(key, icon.clone());
        icon
    }

    fn lookup(&self, name: &str, theme: Option<&str>, size: u32) -> Option<Icon> {
        if is_path(name) {
            let path = expand_home(name);
            let path = match &self.base {
                Some(base) if path.is_relative() => base.join(path),
                _ => path,
            };
            return Icon::load(&path).ok();
        }
        if let Some(icon) = Icon::builtin(name) {
            return Some(icon);
        }
        if let Some(loader) = &self.loader {
            let mut q = Query::new(name, size);
            if let Some(t) = theme {
                q = q.theme(t);
            }
            if let Some(found) = loader.lookup(&q)
                && let Ok(icon) = Icon::load(&found.path)
            {
                return Some(icon);
            }
        }
        let base = name.strip_suffix("-symbolic").unwrap_or(name);
        BUILTIN_ALIASES
            .iter()
            .find(|(xdg, _)| *xdg == base)
            .and_then(|(_, b)| Icon::builtin(b))
    }
}

/// A file rather than a theme icon name: anything with a `/` (absolute, `~/`, relative), or a
/// bare file name with an image extension (`logo.svg` next to the config).
fn is_path(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.contains('/') || [".png", ".svg", ".svgz"].iter().any(|e| lower.ends_with(e))
}

fn expand_home(p: &str) -> std::path::PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => p.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_fallbacks() {
        let mut icons = Icons::builtin_only();
        assert!(
            icons
                .get("media-playback-start-symbolic", None, 36)
                .is_some()
        );
        assert!(icons.get("play_pause", None, 36).is_some());
        assert!(icons.get("no-such-icon", None, 36).is_none());
        let i = icons.get("audio-volume-high", None, 36).unwrap();
        assert!(i.symbolic, "built-ins follow the foreground color");
    }

    #[test]
    fn file_paths_resolve_next_to_the_config() {
        let dir = std::env::temp_dir().join(format!("sliver-icons-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("icons")).unwrap();
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8"/></svg>"#;
        std::fs::write(dir.join("logo.svg"), svg).unwrap();
        std::fs::write(dir.join("icons/x.svg"), svg).unwrap();
        let mut icons = Icons::builtin_only();
        assert!(
            icons.get("logo.svg", None, 36).is_none(),
            "not in the working directory"
        );
        icons.set_base_dir(Some(&dir));
        assert!(icons.get("logo.svg", None, 36).is_some(), "bare file name");
        assert!(
            icons.get("icons/x.svg", None, 36).is_some(),
            "relative path"
        );
        let abs = dir.join("icons/x.svg");
        assert!(
            icons.get(abs.to_str().unwrap(), None, 36).is_some(),
            "absolute"
        );
        assert!(
            icons.get("audio-volume-high", None, 36).is_some(),
            "names still names"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
