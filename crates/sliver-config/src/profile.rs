//! Profiles (customization.md §1b, ux.md §9): named full configs, one active at a time.
//!
//! - `default` is the ordinary config (`config.toml`, found on the search path).
//! - Any other profile is `profiles/<name>.toml` in the user's config dir. A new profile starts as
//!   `include = ["../config.toml"]`, so it is your main config plus what you change in it.
//! - The active name is persisted in `active-profile` in the user's config dir. The agent watches
//!   that file, so `sliverctl profile`, the settings app, `Control1.SetProfile` and a
//!   `{ profile = "…" }` action all switch the same way: by writing it.
//!
//! An active name whose file is gone falls back to `default`, so a deleted profile can't leave
//! the bar without a config.

use std::path::{Path, PathBuf};

use crate::load::search_path;

pub const DEFAULT: &str = "default";
const MARKER: &str = "active-profile";
const DIR: &str = "profiles";

/// The user's config dir (`$XDG_CONFIG_HOME/sliver` or `~/.config/sliver`), if there is a home.
pub fn user_dir() -> Option<PathBuf> {
    let has_home = ["XDG_CONFIG_HOME", "HOME"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()));
    if !has_home {
        return None;
    }
    search_path().first()?.parent().map(Path::to_path_buf)
}

/// Where `active-profile` lives.
pub fn marker_path() -> Option<PathBuf> {
    user_dir().map(|d| d.join(MARKER))
}

/// Letters, digits, `-` and `_`; at most 64 characters. Keeps names usable as file names, in
/// TOML without quoting, and in a D-Bus string.
pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("a profile name needs 1 to 64 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "profile name {name:?}: use letters, digits, - and _ only"
        ));
    }
    Ok(())
}

/// The file of a profile other than `default` (it may not exist yet).
pub fn path(name: &str) -> Option<PathBuf> {
    if name == DEFAULT || valid_name(name).is_err() {
        return None;
    }
    user_dir().map(|d| d.join(DIR).join(format!("{name}.toml")))
}

/// `default` first, then the profiles in `profiles/`, by name.
pub fn list() -> Vec<String> {
    let mut names: Vec<String> = user_dir()
        .and_then(|d| std::fs::read_dir(d.join(DIR)).ok())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "toml").then_some(())?;
            let n = p.file_stem()?.to_str()?.to_owned();
            (n != DEFAULT && valid_name(&n).is_ok() && p.is_file()).then_some(n)
        })
        .collect();
    names.sort();
    names.insert(0, DEFAULT.to_owned());
    names
}

/// The active profile: the name in `active-profile` if that profile's file exists, else `default`.
pub fn active() -> String {
    let name = marker_path()
        .and_then(|m| std::fs::read_to_string(m).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    match path(&name) {
        Some(p) if p.is_file() => name,
        _ => DEFAULT.to_owned(),
    }
}

/// The active profile's file, if a profile other than `default` is active.
pub fn active_path() -> Option<PathBuf> {
    path(&active()).filter(|p| p.is_file())
}

/// Makes `name` the active profile (persisted; a running agent switches when it sees the file
/// change).
pub fn set_active(name: &str) -> Result<(), String> {
    let marker = marker_path().ok_or("no home directory to keep the active profile in")?;
    if name == DEFAULT {
        return match std::fs::remove_file(&marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", marker.display()))
            }
            _ => Ok(()),
        };
    }
    valid_name(name)?;
    match path(name) {
        Some(p) if p.is_file() => {}
        _ => return Err(format!("no profile {name:?} (have: {})", list().join(", "))),
    }
    write_atomic(&marker, &format!("{name}\n"))
}

/// Creates profile `name`. With `from = None` (or `default`) it is your main config plus what you
/// change in it (`include = ["../config.toml"]`, and your stylesheet); with another profile it is
/// a copy of that profile's file.
pub fn create(name: &str, from: Option<&str>) -> Result<PathBuf, String> {
    valid_name(name)?;
    if name == DEFAULT {
        return Err("\"default\" is your main config.toml".into());
    }
    let dest = path(name).ok_or("no home directory for profiles")?;
    if dest.exists() {
        return Err(format!("profile {name:?} already exists"));
    }
    let text = match from {
        None | Some(DEFAULT) => overlay_text(name),
        Some(src) => {
            let p = path(src)
                .filter(|p| p.is_file())
                .ok_or_else(|| format!("no profile {src:?}"))?;
            std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?
        }
    };
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    write_atomic(&dest, &text)?;
    Ok(dest)
}

fn overlay_text(name: &str) -> String {
    let mut t = format!(
        "# Profile {name:?}: your main config plus what you change here (sliver profiles,\n\
         # customization.md §1b). Switch with `sliverctl profile {name}` or in Sliver's settings.\n\
         version = 1\n"
    );
    let Some(dir) = user_dir() else { return t };
    let main = dir.join("config.toml");
    if main.is_file() {
        t.push_str("include = [\"../config.toml\"]\n");
    }
    // An implicit style.css is looked up beside the config in use, which is now profiles/: name
    // the main one. (A `stylesheet = …` in config.toml comes along with the include, anchored
    // to config.toml's directory by the loader.)
    let explicit = main.is_file() && crate::load::load_file(&main).config.stylesheet.is_some();
    if !explicit && dir.join("style.css").is_file() {
        t.push_str("stylesheet = \"../style.css\"\n");
    }
    t
}

/// Renames a profile (and keeps it active if it was).
pub fn rename(from: &str, to: &str) -> Result<(), String> {
    valid_name(to)?;
    let (Some(src), Some(dest)) = (path(from), path(to)) else {
        return Err("\"default\" can't be renamed".into());
    };
    if !src.is_file() {
        return Err(format!("no profile {from:?}"));
    }
    if dest.exists() {
        return Err(format!("profile {to:?} already exists"));
    }
    let was_active = active() == from;
    std::fs::rename(&src, &dest).map_err(|e| format!("{}: {e}", src.display()))?;
    if was_active {
        set_active(to)?;
    }
    Ok(())
}

/// Deletes a profile. If it was active, `default` becomes active first.
pub fn remove(name: &str) -> Result<(), String> {
    let p = path(name).ok_or("\"default\" is your main config and can't be removed")?;
    if !p.is_file() {
        return Err(format!("no profile {name:?}"));
    }
    if active() == name {
        set_active(DEFAULT)?;
    }
    std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Files whose changes can switch the profile: watch them for hot reload.
pub fn watch_paths() -> Vec<PathBuf> {
    marker_path().into_iter().chain(active_path()).collect()
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(err)?;
    }
    let tmp = path.with_extension("sliver-tmp");
    std::fs::write(&tmp, text).map_err(err)?;
    std::fs::rename(&tmp, path).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Tests change XDG_CONFIG_HOME: one at a time.
    static ENV: Mutex<()> = Mutex::new(());

    struct Sandbox {
        _lock: std::sync::MutexGuard<'static, ()>,
        old: Option<std::ffi::OsString>,
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            // SAFETY: still holding ENV.
            unsafe {
                match &self.old {
                    Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
        }
    }

    fn sandbox(name: &str) -> (Sandbox, PathBuf) {
        let g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp/profile")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sliver")).unwrap();
        let old = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: tests touching the environment hold ENV.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &root) };
        (Sandbox { _lock: g, old }, root.join("sliver"))
    }

    #[test]
    fn create_switch_rename_remove() {
        let (_g, dir) = sandbox("lifecycle");
        std::fs::write(dir.join("config.toml"), "version = 1\n").unwrap();
        std::fs::write(dir.join("style.css"), "bar {}\n").unwrap();
        assert_eq!(list(), ["default"]);
        assert_eq!(active(), "default");
        assert_eq!(crate::find_config(), Some(dir.join("config.toml")));

        let p = create("work", None).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("include = [\"../config.toml\"]"), "{text}");
        assert!(text.contains("stylesheet = \"../style.css\""), "{text}");
        assert!(crate::load_file(&p).diagnostics.ok());
        assert!(create("work", None).is_err());
        assert!(create("no/slash", None).is_err());
        assert!(set_active("nope").is_err());

        set_active("work").unwrap();
        assert_eq!(active(), "work");
        assert_eq!(crate::find_config(), Some(p.clone()));
        assert_eq!(crate::edit_path(), p);
        assert_eq!(watch_paths(), [dir.join("active-profile"), p.clone()]);

        create("talk", Some("work")).unwrap();
        assert_eq!(list(), ["default", "talk", "work"]);
        rename("work", "office").unwrap();
        assert_eq!(active(), "office", "renaming keeps it active");
        remove("office").unwrap();
        assert_eq!(active(), "default");
        assert_eq!(crate::find_config(), Some(dir.join("config.toml")));
        assert_eq!(crate::edit_path(), dir.join("config.toml"));
        assert!(remove("default").is_err());

        // A marker naming a deleted profile falls back to default.
        std::fs::write(dir.join("active-profile"), "gone\n").unwrap();
        assert_eq!(active(), "default");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
