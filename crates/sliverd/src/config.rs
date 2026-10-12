//! `/etc/sliver/sliverd.toml`: the daemon's few system settings. Everything user-facing lives in
//! the agent's config. A broken file is reported and ignored: the daemon must always start
//! (invariant 1), unlike tiny-dfr, which panics into its crash screen.
//!
//! ```toml
//! physical_esc = true          # override the bar-length heuristic (≥ 2170 px = no Esc)
//! media_layer_default = false  # safe layer shows media keys, F-keys while Fn is held
//! ```

use std::path::Path;

use serde::Deserialize;

use crate::daemon::Config;

pub const CONFIG_PATH: &str = "/etc/sliver/sliverd.toml";

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct File {
    physical_esc: Option<bool>,
    media_layer_default: bool,
}

/// Parses the config text. Errors are returned as a message along with the defaults.
pub fn parse(text: &str) -> (Config, Option<String>) {
    match toml::from_str::<File>(text) {
        Ok(f) => (
            Config {
                physical_esc: f.physical_esc,
                media_layer_default: f.media_layer_default,
            },
            None,
        ),
        Err(e) => (Config::default(), Some(e.to_string())),
    }
}

/// Loads `path`. A missing file is the defaults. Anything wrong is a warning, never an error.
pub fn load(path: &Path) -> (Config, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let (c, err) = parse(&text);
            (
                c,
                err.map(|e| format!("{}: {e}; using defaults", path.display())),
            )
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
        Err(e) => (
            Config::default(),
            Some(format!("{}: {e}; using defaults", path.display())),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_never_fails() {
        let (c, e) = parse("physical_esc = false\nmedia_layer_default = true\n");
        assert_eq!(e, None);
        assert_eq!(c.physical_esc, Some(false));
        assert!(c.media_layer_default);
        let (c, e) = parse("physical_esc = \"maybe\"");
        assert_eq!(c, Config::default());
        assert!(e.is_some());
        let (_, e) = parse("typo_key = 1");
        assert!(e.unwrap().contains("typo_key"));
        assert_eq!(
            load(Path::new("/nonexistent/sliverd.toml")),
            (Config::default(), None)
        );
    }
}
