//! Preset bundles (`.sliver`, ux.md §9): a whole setup in one file you can share.
//!
//! A bundle is a TOML document, so you can read it before using it:
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
//! * [`export`] collects the config, its includes, the stylesheet (and its `@import`s and `url()`s
//!   under the config directory) and the icons and images the config points at. Files outside
//!   the config directory move to `assets/` or `include/`, and the references are rewritten.
//!   System files (`/usr`, `/etc`, `/opt`) are referenced but not copied. `env` tables are left
//!   out, since they're where secrets live.
//! * [`Bundle::runs`] lists everything the preset would run (commands, scripts, D-Bus calls,
//!   opened files and URLs), so the user can review it before importing.
//! * [`Bundle::without_commands`] drops all of it (the import's "scripts start disabled").
//! * [`Bundle::check`] validates a bundle in a scratch directory. [`Bundle::install`] does the
//!   same, then backs up the files it replaces and moves the new ones into the config directory,
//!   writing `config.toml` last so the agent's hot reload sees a complete set.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use toml::{Table, Value};

use crate::diag::{Diagnostics, join};
use crate::load::load_file;

/// The bundle format version this build writes and reads.
pub const FORMAT: i64 = 1;
/// The file extension of a preset bundle.
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

    /// Everything the preset would run or open: module scripts (`exec`, `exec-if`) and the
    /// command, D-Bus, open and URL actions of every gesture and menu entry, in every TOML file.
    pub fn runs(&self) -> Vec<Run> {
        let mut out = Vec::new();
        for (name, text) in self.toml_files() {
            match text.parse::<Table>() {
                Ok(t) => walk_runs(&Value::Table(t), name, "", &mut out),
                Err(_) => out.push(Run {
                    at: name.clone(),
                    what: "unreadable TOML (it won't load)".into(),
                }),
            }
        }
        out
    }

    /// The same preset with every script and command action removed (the review's "start
    /// disabled"). Returns how many were removed.
    pub fn without_commands(&self) -> (Bundle, usize) {
        let mut b = self.clone();
        let mut removed = 0;
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
            removed += strip_table(doc.as_table_mut());
            *data = doc.to_string().into_bytes();
        }
        (b, removed)
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
        // Only the config: don't leave an empty files dir behind (fails harmlessly otherwise).
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

fn walk_runs(v: &Value, file: &str, path: &str, out: &mut Vec<Run>) {
    match v {
        Value::Table(t) => {
            for (k, x) in t {
                let p = join(path, k);
                if k == "exec" || k == "exec-if" {
                    describe(x, &format!("{file}: {p}"), out);
                } else if k.starts_with("on-") {
                    actions(x, &format!("{file}: {p}"), out);
                } else {
                    walk_runs(x, file, &p, out);
                }
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                walk_runs(x, file, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// A module's `exec`: a shell string or an argv.
fn describe(v: &Value, at: &str, out: &mut Vec<Run>) {
    let what = match v {
        Value::String(s) => format!("sh: {s}"),
        Value::Array(a) => format!("exec: {}", argv(a)),
        other => format!("exec: {other}"),
    };
    out.push(Run {
        at: at.to_owned(),
        what,
    });
}

fn argv(a: &[Value]) -> String {
    a.iter()
        .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_owned))
        .collect::<Vec<_>>()
        .join(" ")
}

/// An action (`on-*`): what it runs, if anything. Keys, layers and layouts run nothing.
fn actions(v: &Value, at: &str, out: &mut Vec<Run>) {
    let mut push = |what: String| {
        out.push(Run {
            at: at.to_owned(),
            what,
        })
    };
    match v {
        Value::String(s) => push(format!("sh: {s}")),
        Value::Array(steps) => {
            for (i, s) in steps.iter().enumerate() {
                actions(s, &format!("{at}[{i}]"), out);
            }
        }
        Value::Table(t) => {
            if let Some(x) = t.get("exec") {
                match x {
                    Value::Array(a) => push(format!("exec: {}", argv(a))),
                    other => push(format!("exec: {other}")),
                }
            }
            if let Some(Value::Table(d)) = t.get("dbus") {
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("");
                let bus = if s("bus").is_empty() {
                    "session"
                } else {
                    s("bus")
                };
                push(format!(
                    "dbus: {bus} {} {} {}.{}",
                    s("dest"),
                    s("path"),
                    s("interface"),
                    s("method")
                ));
            }
            for k in ["open", "url"] {
                if let Some(x) = t.get(k) {
                    push(format!(
                        "{k}: {}",
                        x.as_str().map_or_else(|| x.to_string(), str::to_owned)
                    ));
                }
            }
        }
        _ => {}
    }
}

fn runs_something(v: &Value) -> bool {
    let mut out = Vec::new();
    actions(v, "", &mut out);
    !out.is_empty()
}

fn item_value(i: &toml_edit::Item) -> Option<Value> {
    // Round-trip one item through text to judge it with the same rules as `runs`.
    let mut doc = toml_edit::DocumentMut::new();
    doc["x"] = i.clone();
    doc.to_string()
        .parse::<Table>()
        .ok()
        .and_then(|mut t| t.remove("x"))
}

fn strip_table(t: &mut dyn toml_edit::TableLike) -> usize {
    let doomed: Vec<String> = t
        .iter()
        .filter(|(k, i)| {
            *k == "exec"
                || *k == "exec-if"
                || (k.starts_with("on-") && item_value(i).is_some_and(|v| runs_something(&v)))
        })
        .map(|(k, _)| k.to_owned())
        .collect();
    for k in &doomed {
        t.remove(k);
    }
    let mut n = doomed.len();
    for (_, item) in t.iter_mut() {
        n += strip_item(item);
    }
    n
}

fn strip_item(i: &mut toml_edit::Item) -> usize {
    match i {
        toml_edit::Item::Table(t) => strip_table(t),
        toml_edit::Item::ArrayOfTables(a) => a.iter_mut().map(|t| strip_table(t)).sum(),
        toml_edit::Item::Value(v) => strip_value(v),
        toml_edit::Item::None => 0,
    }
}

fn strip_value(v: &mut toml_edit::Value) -> usize {
    match v {
        toml_edit::Value::InlineTable(t) => strip_table(t),
        toml_edit::Value::Array(a) => a.iter_mut().map(strip_value).sum(),
        _ => 0,
    }
}

// ---- export ----

/// Where a referenced file goes in the bundle.
struct Placer {
    root: PathBuf,
    home: Option<PathBuf>,
    /// canonical source → bundle name
    placed: BTreeMap<PathBuf, String>,
    /// bundle name → canonical source, in placement order
    sources: Vec<(String, PathBuf)>,
}

impl Placer {
    fn resolve(&self, s: &str, dir: &Path) -> Option<PathBuf> {
        let p = match (s.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => dir.join(s),
        };
        p.canonicalize().ok().filter(|p| p.is_file())
    }

    fn is_system(&self, p: &Path) -> bool {
        let s = p.to_string_lossy();
        !p.starts_with(&self.root) && SYSTEM_ROOTS.iter().any(|r| s.starts_with(r))
    }

    /// The bundle name for `p`, placing it on first sight (`None` = a system file, not copied).
    fn place(&mut self, p: &Path, kind: &str) -> Option<String> {
        if let Some(n) = self.placed.get(p) {
            return Some(n.clone());
        }
        if self.is_system(p) {
            return None;
        }
        let name = match p.strip_prefix(&self.root) {
            Ok(rel) if check_name(&rel.to_string_lossy()).is_ok() => {
                rel.to_string_lossy().into_owned()
            }
            _ => {
                let base = p.file_name()?.to_string_lossy().into_owned();
                let (stem, ext) = match base.rsplit_once('.') {
                    Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
                    _ => (base.clone(), String::new()),
                };
                let mut n = format!("{kind}/{base}");
                let mut i = 2;
                while self.sources.iter().any(|(x, _)| *x == n) {
                    n = format!("{kind}/{stem}-{i}{ext}");
                    i += 1;
                }
                n
            }
        };
        self.placed.insert(p.to_path_buf(), name.clone());
        self.sources.push((name.clone(), p.to_path_buf()));
        Some(name)
    }
}

/// `target` (a bundle name) as seen from the directory of `from` (a bundle name).
fn relative(from: &str, target: &str) -> String {
    let up = from.matches('/').count();
    format!("{}{target}", "../".repeat(up))
}

/// Bundles the config at `config` (its directory is the bundle root). Refuses a config that
/// doesn't load.
pub fn export(config: &Path, name: Option<&str>) -> Result<Export, String> {
    let loaded = load_file(config);
    if !loaded.diagnostics.ok() {
        let errors: Vec<String> = loaded
            .diagnostics
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect();
        return Err(format!(
            "fix the config before exporting it:\n  {}",
            errors.join("\n  ")
        ));
    }
    let config = config
        .canonicalize()
        .map_err(|e| format!("{}: {e}", config.display()))?;
    let root = config.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut placer = Placer {
        root: root.clone(),
        home: std::env::var_os("HOME").map(PathBuf::from),
        placed: BTreeMap::new(),
        sources: Vec::new(),
    };
    placer.placed.insert(config.clone(), CONFIG.into());
    placer.sources.push((CONFIG.into(), config.clone()));
    let mut notes = Vec::new();
    let mut files = BTreeMap::new();
    let mut total = 0;
    // `stylesheet` absent = `style.css` next to the config.
    if loaded.config.stylesheet.is_none()
        && let Some(css) = placer.resolve("style.css", &root)
    {
        placer.place(&css, "style");
    }
    let mut i = 0;
    while i < placer.sources.len() {
        let (name, src) = placer.sources[i].clone();
        i += 1;
        let data = std::fs::read(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        total += data.len();
        if data.len() > MAX_FILE || total > MAX_TOTAL || placer.sources.len() > MAX_FILES {
            return Err(format!(
                "{}: the preset would be too large (limits: {} MiB per file, {} MiB in all, \
                 {MAX_FILES} files)",
                src.display(),
                MAX_FILE >> 20,
                MAX_TOTAL >> 20
            ));
        }
        let dir = src.parent().unwrap_or(Path::new("/")).to_path_buf();
        let data = if name.ends_with(".toml") {
            export_toml(&data, &name, &dir, &mut placer, &mut notes)?
        } else if name.ends_with(".css") {
            scan_css(&data, &name, &dir, &mut placer, &mut notes);
            data
        } else {
            data
        };
        files.insert(name, data);
    }
    let bundle = Bundle {
        name: name.map(str::to_owned),
        files,
    };
    if let Some(h) = placer
        .home
        .as_ref()
        .map(|h| h.to_string_lossy().into_owned())
    {
        for r in bundle.runs() {
            if r.what.contains(&h) || r.what.contains("~/") {
                notes.push(format!(
                    "{} refers to your home directory, which may not exist elsewhere: {}",
                    r.at, r.what
                ));
            }
        }
    }
    Ok(Export { bundle, notes })
}

/// Rewrites a TOML file's file references to their bundle names and drops `env` tables.
fn export_toml(
    data: &[u8],
    name: &str,
    dir: &Path,
    placer: &mut Placer,
    notes: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(data).map_err(|_| format!("{name}: not UTF-8"))?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{name}: {e}"))?;
    let mut ctx = Rewrite {
        name,
        dir,
        placer,
        notes,
    };
    ctx.table(doc.as_table_mut(), "");
    Ok(doc.to_string().into_bytes())
}

struct Rewrite<'a> {
    name: &'a str,
    dir: &'a Path,
    placer: &'a mut Placer,
    notes: &'a mut Vec<String>,
}

impl Rewrite<'_> {
    fn table(&mut self, t: &mut dyn toml_edit::TableLike, path: &str) {
        if t.contains_key("env") && !path.is_empty() {
            t.remove("env");
            self.notes.push(format!(
                "{}: {}: env left out (it may hold secrets)",
                self.name,
                join(path, "env")
            ));
        }
        for (k, item) in t.iter_mut() {
            let p = join(path, &k);
            let is_ref = PATH_KEYS.contains(&k.get());
            self.item(item, &p, is_ref);
        }
    }

    fn item(&mut self, i: &mut toml_edit::Item, path: &str, is_ref: bool) {
        match i {
            toml_edit::Item::Table(t) => self.table(t, path),
            toml_edit::Item::ArrayOfTables(a) => {
                for (n, t) in a.iter_mut().enumerate() {
                    self.table(t, &format!("{path}[{n}]"));
                }
            }
            toml_edit::Item::Value(v) => self.value(v, path, is_ref),
            toml_edit::Item::None => {}
        }
    }

    fn value(&mut self, v: &mut toml_edit::Value, path: &str, is_ref: bool) {
        match v {
            toml_edit::Value::InlineTable(t) => self.table(t, path),
            toml_edit::Value::Array(a) => {
                for (n, x) in a.iter_mut().enumerate() {
                    self.value(x, &format!("{path}[{n}]"), is_ref);
                }
            }
            toml_edit::Value::String(s) if is_ref => {
                let orig = s.value().clone();
                let Some(src) = self.placer.resolve(&orig, self.dir) else {
                    return;
                };
                let kind = if path.ends_with("include") || path.contains("include[") {
                    "include"
                } else {
                    "assets"
                };
                match self.placer.place(&src, kind) {
                    Some(target) => {
                        let new = relative(self.name, &target);
                        // A relative reference that still points at the same file from the
                        // referrer's place in the bundle stays as written.
                        let still_valid = Path::new(&orig).is_relative()
                            && !orig.starts_with("~/")
                            && self.dir.join(&new).canonicalize().ok().as_ref() == Some(&src);
                        if !still_valid {
                            let decor = s.decor().clone();
                            let mut nv = toml_edit::Value::from(new);
                            *nv.decor_mut() = decor;
                            *v = nv;
                        }
                    }
                    None => self.notes.push(format!(
                        "{}: {path}: {} is a system file; it's referenced, not copied",
                        self.name,
                        src.display()
                    )),
                }
            }
            _ => {}
        }
    }
}

/// Bundles a stylesheet's `@import`s and `url()`s that live under the config directory. Others
/// are noted, not rewritten (CSS paths stay as written).
fn scan_css(data: &[u8], name: &str, dir: &Path, placer: &mut Placer, notes: &mut Vec<String>) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut refs = Vec::new();
    for (i, _) in text.match_indices("@import") {
        let rest = text[i + 7..].trim_start();
        let rest = rest.strip_prefix("url(").unwrap_or(rest);
        if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'')
            && let Some(end) = rest[1..].find(q)
        {
            refs.push(rest[1..1 + end].to_owned());
        }
    }
    for (i, _) in text.match_indices("url(") {
        let rest = text[i + 4..].trim_start();
        let (q, rest) = match rest.chars().next() {
            Some(c @ ('"' | '\'')) => (c, &rest[1..]),
            _ => (')', rest),
        };
        if let Some(end) = rest.find(q) {
            refs.push(rest[..end].trim().to_owned());
        }
    }
    refs.sort();
    refs.dedup();
    for r in refs {
        if r.starts_with("sliver:") || r.contains("://") {
            continue;
        }
        let Some(src) = placer.resolve(&r, dir) else {
            continue;
        };
        if src.starts_with(&placer.root) {
            placer.place(&src, "assets");
        } else if !placer.is_system(&src) {
            notes.push(format!(
                "{name}: {r} is outside the config directory; it isn't included"
            ));
        }
    }
}

// ---- base64 (RFC 4648, padded) ----

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= c.len() {
                s.push(B64[(n >> shift & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let clean: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if clean.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for c in clean.chunks(4) {
        let mut n = 0u32;
        let mut pad = 0;
        for &b in c {
            let v = match b {
                b'=' => {
                    pad += 1;
                    0
                }
                _ if pad > 0 => return None,
                _ => B64.iter().position(|x| *x == b)? as u32,
            };
            n = n << 6 | v;
        }
        if pad > 2 {
            return None;
        }
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/test-tmp"
        ))
        .join(format!("preset-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn base64_round_trips() {
        for n in 0..20 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert!(base64_decode("TW=u").is_none());
        assert!(base64_decode("TWF").is_none());
    }

    #[test]
    fn names_stay_inside() {
        for ok in ["config.toml", "assets/a.png", "include/x/y.toml"] {
            check_name(ok).unwrap();
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            "./a",
            "a//b",
            "a/",
            "a\\b",
            "a/b/c/d/e",
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        let evil = "[preset]\nformat = 1\n[files]\n\"config.toml\" = \"\"\n\"../x\" = \"\"\n";
        assert!(Bundle::parse(evil).unwrap_err().contains("../x"));
    }

    #[test]
    fn parse_wants_a_preset() {
        assert!(Bundle::parse("version = 1").is_err());
        assert!(
            Bundle::parse("[preset]\nformat = 9\n[files]\n")
                .unwrap_err()
                .contains("9")
        );
        assert!(
            Bundle::parse("[preset]\nformat = 1\n[files]\n")
                .unwrap_err()
                .contains(CONFIG)
        );
    }

    #[test]
    fn lists_and_strips_everything_that_runs() {
        let config = r#"
version = 1
[modules."custom/vpn"]
exec = "~/bin/vpn --json"
exec-if = "test -x ~/bin/vpn"
on-tap = { exec = ["foot", "-e", "htop"] }
on-hold = { layer = "fn" }
on-swipe-left = [{ key = "VolumeDown" }, { dbus = { dest = "org.x", path = "/x", interface = "org.x.Y", method = "Go" } }]
menu = [{ label = "Lock", on-tap = "loginctl lock-session" }, { label = "Web", on-tap = { url = "https://x" } }]
[modules.clock]
on-tap = { key = "F1" }
"#;
        let b = Bundle {
            name: None,
            files: BTreeMap::from([(CONFIG.to_owned(), config.as_bytes().to_vec())]),
        };
        let runs: Vec<String> = b.runs().iter().map(|r| r.what.clone()).collect();
        assert_eq!(
            runs,
            [
                "sh: ~/bin/vpn --json",
                "sh: test -x ~/bin/vpn",
                "sh: loginctl lock-session",
                "url: https://x",
                "dbus: session org.x /x org.x.Y.Go",
                "exec: foot -e htop",
            ]
            .map(String::from)
            .to_vec(),
            "{:#?}",
            b.runs()
        );
        assert!(
            b.runs()[0]
                .at
                .starts_with("config.toml: modules.\"custom/vpn\".exec")
        );
        let (clean, n) = b.without_commands();
        assert_eq!(n, 6);
        assert!(clean.runs().is_empty(), "{:?}", clean.runs());
        let text = String::from_utf8(clean.files[CONFIG].clone()).unwrap();
        assert!(text.contains("on-hold = { layer = \"fn\" }"), "{text}");
        assert!(text.contains("on-tap = { key = \"F1\" }"), "{text}");
        assert!(text.contains("label = \"Lock\""), "{text}");
    }

    #[test]
    fn export_import_round_trip() {
        let home = tmp("home");
        let cfg = home.join("cfg");
        let elsewhere = home.join("pics");
        std::fs::create_dir_all(cfg.join("parts")).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(
            elsewhere.join("logo.png"),
            [0x89, b'P', b'N', b'G', 0, 1, 2],
        )
        .unwrap();
        std::fs::write(
            elsewhere.join("extra.toml"),
            "[modules.\"custom/x\"]\nformat = \"x\"\nicon = \"../pics/logo.png\"\n",
        )
        .unwrap();
        std::fs::write(
            cfg.join("parts/keys.toml"),
            "[modules.\"custom/k\"]\nformat = \"k\"\n",
        )
        .unwrap();
        std::fs::write(cfg.join("bg.svg"), "<svg/>").unwrap();
        std::fs::write(
            cfg.join("style.css"),
            "#clock { background-image: url(\"bg.svg\"); }\n",
        )
        .unwrap();
        let logo = elsewhere.join("logo.png").display().to_string();
        std::fs::write(
            cfg.join(CONFIG),
            format!(
                "version = 1\ninclude = [\"parts/keys.toml\", \"../pics/extra.toml\"]\n\
                 [modules.\"custom/vpn\"]\nformat = \"vpn\"\nicon = \"{logo}\" # logo\n\
                 env = {{ TOKEN = \"hunter2\" }}\n\
                 [modules.\"custom/sys\"]\nformat = \"s\"\nicon = \"/usr/share/icons/x.svg\"\n"
            ),
        )
        .unwrap();
        let ex = export(&cfg.join(CONFIG), Some("Desk")).unwrap();
        let names: Vec<&str> = ex.bundle.files.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "assets/logo.png",
                "bg.svg",
                CONFIG,
                "include/extra.toml",
                "parts/keys.toml",
                "style.css"
            ]
        );
        let main = String::from_utf8(ex.bundle.files[CONFIG].clone()).unwrap();
        assert!(main.contains("icon = \"assets/logo.png\" # logo"), "{main}");
        assert!(
            main.contains("\"parts/keys.toml\", \"include/extra.toml\""),
            "{main}"
        );
        assert!(main.contains("/usr/share/icons/x.svg"), "{main}");
        assert!(!main.contains("hunter2"), "{main}");
        assert!(
            ex.notes.iter().any(|n| n.contains("env left out")),
            "{:?}",
            ex.notes
        );
        let extra = String::from_utf8(ex.bundle.files["include/extra.toml"].clone()).unwrap();
        assert!(extra.contains("icon = \"../assets/logo.png\""), "{extra}");

        // Through text and back, then into a fresh config dir.
        let text = ex.bundle.to_text();
        assert!(text.contains("base64 = "), "{text}");
        assert!(text.starts_with("# A sliver preset"));
        assert!(text.find("[preset]") < text.find("[files]"), "{text}");
        let back = Bundle::parse(&text).unwrap();
        assert_eq!(back, ex.bundle);
        let scratch = tmp("scratch");
        assert!(back.check(&scratch).unwrap().ok());
        let dest = tmp("dest");
        std::fs::write(dest.join(CONFIG), "version = 1 # old\n").unwrap();
        let inst = back.install(&dest).unwrap();
        assert_eq!(inst.written.last().unwrap(), &dest.join(CONFIG));
        let backup = inst.backup.unwrap();
        assert_eq!(
            std::fs::read_to_string(backup.join(CONFIG)).unwrap(),
            "version = 1 # old\n"
        );
        let installed = std::fs::read_to_string(dest.join(CONFIG)).unwrap();
        let abs_logo = dest.join("assets/logo.png").display().to_string();
        assert!(
            installed.contains(&format!("icon = \"{abs_logo}\" # logo")),
            "{installed}"
        );
        let extra = std::fs::read_to_string(dest.join("include/extra.toml")).unwrap();
        assert!(extra.contains(&abs_logo), "{extra}");
        // Exporting the installed config gives the same bundle back (absolute paths under the
        // config directory turn relative again).
        let again = export(&dest.join(CONFIG), Some("Desk")).unwrap().bundle;
        assert_eq!(
            again.files.keys().collect::<Vec<_>>(),
            ex.bundle.files.keys().collect::<Vec<_>>()
        );
        assert_eq!(again.files[CONFIG], ex.bundle.files[CONFIG]);
        let loaded = load_file(&dest.join(CONFIG));
        assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
        assert!(
            loaded
                .config
                .modules
                .keys()
                .any(|k| k.to_string() == "custom/x")
        );
        assert!(
            loaded
                .config
                .modules
                .keys()
                .any(|k| k.to_string() == "custom/k")
        );
        assert_eq!(
            std::fs::read(dest.join("assets/logo.png")).unwrap(),
            [0x89, b'P', b'N', b'G', 0, 1, 2]
        );
        let leftovers: Vec<_> = std::fs::read_dir(&dest)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".preset-"))
            .collect();
        assert!(leftovers.is_empty());
        for d in [home, scratch, dest] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn export_from_a_profile_and_install_as_one() {
        // A profile (profiles/work.toml) overlays the main config, like `profile::create` makes.
        let home = tmp("prof");
        let cfg = home.join("sliver");
        std::fs::create_dir_all(cfg.join("profiles")).unwrap();
        std::fs::write(cfg.join("logo.svg"), "<svg/>").unwrap();
        std::fs::write(cfg.join("style.css"), "#clock { color: red; }\n").unwrap();
        std::fs::write(
            cfg.join(CONFIG),
            "version = 1\n[modules.\"custom/a\"]\nformat = \"a\"\nicon = \"logo.svg\"\n",
        )
        .unwrap();
        std::fs::write(
            cfg.join("profiles/work.toml"),
            "version = 1\ninclude = [\"../config.toml\"]\nstylesheet = \"../style.css\"\n\
             [modules.\"custom/w\"]\nformat = \"w\"\n",
        )
        .unwrap();
        let ex = export(&cfg.join("profiles/work.toml"), None).unwrap();
        let names: Vec<&str> = ex.bundle.files.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "assets/logo.svg",
                "assets/style.css",
                CONFIG,
                "include/config.toml"
            ],
            "{:?}",
            ex.notes
        );
        let main = String::from_utf8(ex.bundle.files[CONFIG].clone()).unwrap();
        assert!(
            main.contains("include = [\"include/config.toml\"]"),
            "{main}"
        );
        assert!(main.contains("stylesheet = \"assets/style.css\""), "{main}");
        let inc = String::from_utf8(ex.bundle.files["include/config.toml"].clone()).unwrap();
        assert!(inc.contains("icon = \"../assets/logo.svg\""), "{inc}");

        // Installed as profile "shared" elsewhere: profiles/shared.toml + profiles/shared/….
        let dest = tmp("prof-dest");
        let prof = dest.join("profiles/shared.toml");
        let files = dest.join("profiles/shared");
        let inst = ex.bundle.install_at(&prof, &files, &dest).unwrap();
        assert!(inst.backup.is_none());
        assert_eq!(inst.written.last().unwrap(), &prof);
        let text = std::fs::read_to_string(&prof).unwrap();
        let abs = |n: &str| files.join(n).display().to_string();
        assert!(text.contains(&abs("include/config.toml")), "{text}");
        assert!(text.contains(&abs("assets/style.css")), "{text}");
        let loaded = load_file(&prof);
        assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
        let ids: Vec<String> = loaded
            .config
            .modules
            .keys()
            .map(|k| k.to_string())
            .collect();
        assert!(ids.contains(&"custom/a".into()) && ids.contains(&"custom/w".into()));
        // An implicit style.css is named explicitly when the config moves.
        let b = Bundle {
            name: None,
            files: BTreeMap::from([
                (CONFIG.to_owned(), b"version = 1\n".to_vec()),
                ("style.css".to_owned(), b"x {}".to_vec()),
            ]),
        };
        b.install_at(
            &dest.join("profiles/plain.toml"),
            &dest.join("profiles/plain"),
            &dest,
        )
        .unwrap();
        let text = std::fs::read_to_string(dest.join("profiles/plain.toml")).unwrap();
        assert!(
            text.contains(&format!(
                "stylesheet = \"{}\"",
                dest.join("profiles/plain/style.css").display()
            )),
            "{text}"
        );
        for d in [home, dest] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_broken_preset_changes_nothing() {
        let dest = tmp("broken");
        std::fs::write(dest.join(CONFIG), "version = 1\n").unwrap();
        let b = Bundle {
            name: None,
            files: BTreeMap::from([
                (CONFIG.to_owned(), b"version = 1\nlayouts = 3\n".to_vec()),
                ("style.css".to_owned(), b"x".to_vec()),
            ]),
        };
        assert!(
            b.install(&dest)
                .unwrap_err()
                .contains("nothing was changed")
        );
        assert_eq!(
            std::fs::read_to_string(dest.join(CONFIG)).unwrap(),
            "version = 1\n"
        );
        assert!(!dest.join("style.css").exists());
        let _ = std::fs::remove_dir_all(dest);
    }
}
