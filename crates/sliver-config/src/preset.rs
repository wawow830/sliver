//! Preset bundles (`.sliver`): a whole setup in one TOML document.
//!
//! ```toml
//! [preset]
//! format = 1
//! name = "My desk"
//!
//! [files]
//! "config.toml" = """
//! version = 1
//! ..."""
//! "style.css" = "..."
//! "assets/logo.png" = { base64 = "iVBORw0..." }
//! ```
//!
//! [`export`] collects the config, its includes, the stylesheet (and its `@import`s and `url()`s
//! under the config directory) and the icons and images the config points at. Files outside the
//! config directory move to `assets/` or `include/`, with references rewritten. System files
//! (`/usr`, `/etc`, `/opt`) are referenced, not copied. `env` tables, where secrets live, are left
//! out.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use toml::{Table, Value};

use crate::diag::{Diagnostics, join};
use crate::load::load_file;

/// The bundle format version this build writes and reads.
pub const FORMAT: i64 = 1;
pub const EXTENSION: &str = "sliver";
/// The main config's name inside a bundle.
pub const CONFIG: &str = "config.toml";

const MAX_FILE: usize = 4 << 20;
const MAX_TOTAL: usize = 16 << 20;
const MAX_FILES: usize = 256;
const MAX_DEPTH: usize = 4;
/// Keys whose string values may name a file: these are bundled and rewritten.
const PATH_KEYS: &[&str] = &["include", "stylesheet", "icon", "path"];
/// Roots of files every sliver install has (or that come from distro packages): not copied.
const SYSTEM_ROOTS: &[&str] = &["/usr/", "/etc/", "/opt/", "/nix/store/"];

/// A preset: files relative to the config directory.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bundle {
    pub name: Option<String>,
    pub files: BTreeMap<String, Vec<u8>>,
}

/// One thing a preset would run or open, for the review list.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Where it is: `config.toml: modules."custom/vpn".exec`.
    pub at: String,
    /// What it does: `sh: ~/bin/vpn --json`, `exec: foot -e htop`, `dbus: …`, `open: …`, `url: …`.
    pub what: String,
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}  ({})", self.what, self.at)
    }
}

/// The result of [`export`].
#[derive(Debug, Clone)]
pub struct Export {
    pub bundle: Bundle,
    /// What the user should know: files left out, references kept, `env` dropped.
    pub notes: Vec<String>,
}

/// The result of [`Bundle::install`].
#[derive(Debug, Clone)]
pub struct Installed {
    /// Files written, in the config directory.
    pub written: Vec<PathBuf>,
    /// Where the replaced files went (`None` if nothing was replaced).
    pub backup: Option<PathBuf>,
}

/// A bundle file name is relative, plain (no `.`, `..` or empty parts) and shallow.
pub fn check_name(name: &str) -> Result<(), String> {
    let p = Path::new(name);
    let parts: Vec<Component> = p.components().collect();
    let plain = !name.is_empty()
        && !name.contains('\\')
        && !name.ends_with('/')
        && parts.iter().all(|c| matches!(c, Component::Normal(_)))
        && parts.len() == name.split('/').count();
    if !plain {
        return Err(format!("{name:?}: not a plain relative path"));
    }
    if parts.len() > MAX_DEPTH {
        return Err(format!("{name:?}: nested deeper than {MAX_DEPTH}"));
    }
    if name.chars().any(char::is_control) {
        return Err(format!("{name:?}: control characters"));
    }
    Ok(())
}

impl Bundle {
    /// Reads a bundle, checking its shape, file names and sizes. It doesn't validate the config
    /// (that's [`Bundle::check`]).
    pub fn parse(text: &str) -> Result<Bundle, String> {
        let t: Table = text.parse().map_err(|e| format!("not a preset: {e}"))?;
        let head = t
            .get("preset")
            .and_then(Value::as_table)
            .ok_or("not a preset: no [preset] table")?;
        match head.get("format").and_then(Value::as_integer) {
            Some(FORMAT) => {}
            Some(n) => return Err(format!("preset format {n}; this sliver reads {FORMAT}")),
            None => return Err("not a preset: [preset] has no format".into()),
        }
        let name = head.get("name").and_then(Value::as_str).map(str::to_owned);
        let files = t
            .get("files")
            .and_then(Value::as_table)
            .ok_or("the preset has no [files]")?;
        if files.len() > MAX_FILES {
            return Err(format!("the preset has more than {MAX_FILES} files"));
        }
        let mut out = BTreeMap::new();
        let mut total = 0;
        for (n, v) in files {
            check_name(n)?;
            let data = match v {
                Value::String(s) => s.as_bytes().to_vec(),
                Value::Table(b) => match b.get("base64").and_then(Value::as_str) {
                    Some(s) => base64_decode(s).ok_or_else(|| format!("{n:?}: bad base64"))?,
                    None => return Err(format!("{n:?}: expected text or {{ base64 = … }}")),
                },
                _ => return Err(format!("{n:?}: expected text or {{ base64 = … }}")),
            };
            if data.len() > MAX_FILE {
                return Err(format!("{n:?} is larger than {} MiB", MAX_FILE >> 20));
            }
            total += data.len();
            out.insert(n.clone(), data);
        }
        if total > MAX_TOTAL {
            return Err(format!("the preset is larger than {} MiB", MAX_TOTAL >> 20));
        }
        if !out.contains_key(CONFIG) {
            return Err(format!("the preset has no {CONFIG}"));
        }
        Ok(Bundle { name, files: out })
    }

    /// The bundle as a `.sliver` document. Text files stay readable; others are base64.
    pub fn to_text(&self) -> String {
        let mut head = Table::new();
        head.insert("format".into(), Value::Integer(FORMAT));
        if let Some(n) = &self.name {
            head.insert("name".into(), Value::String(n.clone()));
        }
        let mut files = Table::new();
        for (n, data) in &self.files {
            let v = match std::str::from_utf8(data) {
                Ok(s) if !s.contains('\0') => Value::String(s.to_owned()),
                _ => {
                    let mut b = Table::new();
                    b.insert("base64".into(), Value::String(base64_encode(data)));
                    Value::Table(b)
                }
            };
            files.insert(n.clone(), v);
        }
        let section = |k: &str, v: Table| {
            let mut t = Table::new();
            t.insert(k.into(), Value::Table(v));
            toml::to_string_pretty(&t).unwrap_or_default()
        };
        format!(
            "# A sliver preset: `sliverctl preset show FILE` lists what it runs,\n\
             # `sliverctl preset import FILE` uses it (see sliverctl(1)).\n\n{}\n{}",
            section("preset", head),
            section("files", files)
        )
    }

    fn toml_files(&self) -> impl Iterator<Item = (&String, &str)> {
        self.files
            .iter()
            .filter(|(n, _)| n.ends_with(".toml"))
            .filter_map(|(n, d)| Some((n, std::str::from_utf8(d).ok()?)))
    }

    /// Validates the preset as it would load, in a scratch directory under `scratch_parent`
    /// (which must exist; it's removed again).
    pub fn check(&self, scratch_parent: &Path) -> Result<Diagnostics, String> {
        let dir = scratch_dir(scratch_parent)?;
        let r = self
            .write_to(&dir)
            .map(|()| load_file(&dir.join(CONFIG)).diagnostics);
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    /// Validates, then installs into `config_dir`. Files it replaces are moved to
    /// `config_dir/backups/<unix time>/` first. Refuses (and changes nothing) if the config
    /// wouldn't load.
    pub fn install(&self, config_dir: &Path) -> Result<Installed, String> {
        self.install_at(&config_dir.join(CONFIG), config_dir, config_dir)
    }

    /// Like [`Bundle::install`], but `config.toml` goes to `config` (a profile's
    /// `profiles/<name>.toml`) and the other files under `files_dir`. References from the config
    /// to bundled files become absolute (a profile's own paths resolve from `profiles/`), and
    /// a bundled `style.css` is named explicitly. Backups go to `backup_root/backups/<time>/`.
    pub fn install_at(
        &self,
        config: &Path,
        files_dir: &Path,
        backup_root: &Path,
    ) -> Result<Installed, String> {
        std::fs::create_dir_all(files_dir).map_err(|e| format!("{}: {e}", files_dir.display()))?;
        let d = self.check(files_dir)?;
        if !d.ok() {
            let errors: Vec<String> = d.errors.iter().map(|e| e.to_string()).collect();
            return Err(format!(
                "the preset's config has errors; nothing was changed:\n  {}",
                errors.join("\n  ")
            ));
        }
        let moved = config.parent() != Some(files_dir);
        let b = self.anchored(files_dir, moved);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let backup_dir = backup_root.join("backups").join(stamp.to_string());
        let mut backup = None;
        let mut written = Vec::new();
        // config.toml last: the reloader then sees includes and the stylesheet already there.
        let order = b
            .files
            .keys()
            .filter(|n| *n != CONFIG)
            .chain(std::iter::once(&CONFIG.to_owned()))
            .cloned()
            .collect::<Vec<_>>();
        for name in order {
            let dest = if name == CONFIG {
                config.to_path_buf()
            } else {
                files_dir.join(&name)
            };
            if dest.symlink_metadata().is_ok() {
                let shown = dest.strip_prefix(backup_root).unwrap_or(Path::new(&name));
                let to = backup_dir.join(shown);
                mkdir_parent(&to)?;
                std::fs::copy(&dest, &to).map_err(|e| format!("backing up {name}: {e}"))?;
                backup = Some(backup_dir.clone());
            }
            mkdir_parent(&dest)?;
            // Write beside, then rename: a watcher never sees half a file.
            let tmp = dest.with_file_name(format!(
                ".{}.preset-{}",
                dest.file_name().unwrap_or_default().to_string_lossy(),
                std::process::id()
            ));
            std::fs::write(&tmp, &b.files[&name]).map_err(|e| format!("{}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, &dest).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", dest.display())
            })?;
            written.push(dest);
        }
        // Removes `files_dir` only if empty, i.e. the bundle had just the config.
        let _ = std::fs::remove_dir(files_dir);
        Ok(Installed { written, backup })
    }

    /// Icons and images are looked up by absolute path (or icon name), not relative to the config,
    /// so `icon`/`path` references to bundled files become absolute paths in `files_dir`. When
    /// the config itself lives elsewhere (`config_moved`), its `include` and `stylesheet` do too,
    /// and an implicit `style.css` is made explicit.
    fn anchored(&self, files_dir: &Path, config_moved: bool) -> Bundle {
        let mut b = self.clone();
        let names: Vec<String> = self.files.keys().cloned().collect();
        for (name, data) in &mut b.files {
            if !name.ends_with(".toml") {
                continue;
            }
            let Some(mut doc) = std::str::from_utf8(data)
                .ok()
                .and_then(|s| s.parse::<toml_edit::DocumentMut>().ok())
            else {
                continue;
            };
            let from_dir = name.rsplit_once('/').map_or("", |(d, _)| d);
            let mut fix = |s: &str| -> Option<String> {
                if s.starts_with('/') || s.starts_with('~') {
                    return None;
                }
                let target = normalize(from_dir, s)?;
                names
                    .contains(&target)
                    .then(|| files_dir.join(&target).to_string_lossy().into_owned())
            };
            let all = config_moved && name == CONFIG;
            let keys: &[&str] = if all { PATH_KEYS } else { &["icon", "path"] };
            anchor_table(doc.as_table_mut(), keys, &mut fix);
            if all && !doc.contains_key("stylesheet") && names.iter().any(|n| n == "style.css") {
                doc["stylesheet"] =
                    toml_edit::value(files_dir.join("style.css").to_string_lossy().into_owned());
            }
            *data = doc.to_string().into_bytes();
        }
        b
    }

    fn write_to(&self, dir: &Path) -> Result<(), String> {
        for (name, data) in &self.files {
            check_name(name)?;
            let p = dir.join(name);
            mkdir_parent(&p)?;
            std::fs::write(&p, data).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok(())
    }
}

/// `rel` seen from bundle directory `dir`, as a bundle name (`None` if it leaves the bundle).
fn normalize(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

fn anchor_table(
    t: &mut dyn toml_edit::TableLike,
    keys: &[&str],
    fix: &mut dyn FnMut(&str) -> Option<String>,
) {
    for (k, item) in t.iter_mut() {
        let is_ref = keys.contains(&k.get());
        anchor_item(item, is_ref, keys, fix);
    }
}

fn anchor_item(
    i: &mut toml_edit::Item,
    is_ref: bool,
    keys: &[&str],
    fix: &mut dyn FnMut(&str) -> Option<String>,
) {
    match i {
        toml_edit::Item::Table(t) => anchor_table(t, keys, fix),
        toml_edit::Item::ArrayOfTables(a) => {
            for t in a.iter_mut() {
                anchor_table(t, keys, fix);
            }
        }
        toml_edit::Item::Value(v) => anchor_value(v, is_ref, keys, fix),
        toml_edit::Item::None => {}
    }
}

fn anchor_value(
    v: &mut toml_edit::Value,
    is_ref: bool,
    keys: &[&str],
    fix: &mut dyn FnMut(&str) -> Option<String>,
) {
    match v {
        toml_edit::Value::InlineTable(t) => anchor_table(t, keys, fix),
        toml_edit::Value::Array(a) => {
            for x in a.iter_mut() {
                anchor_value(x, is_ref, keys, fix);
            }
        }
        toml_edit::Value::String(s) if is_ref => {
            if let Some(new) = fix(s.value()) {
                let decor = s.decor().clone();
                let mut nv = toml_edit::Value::from(new);
                *nv.decor_mut() = decor;
                *v = nv;
            }
        }
        _ => {}
    }
}

fn mkdir_parent(p: &Path) -> Result<(), String> {
    match p.parent() {
        Some(d) => std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display())),
        None => Ok(()),
    }
}

fn scratch_dir(parent: &Path) -> Result<PathBuf, String> {
    let dir = parent.join(format!(".preset-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

mod export;
mod review;

pub use export::*;

#[cfg(test)]
mod tests;
