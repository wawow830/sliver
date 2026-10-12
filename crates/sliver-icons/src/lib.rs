//! `sliver-icons`: find the file for an icon name.
//!
//! * [`IconLoader`] implements the freedesktop Icon Theme Specification: `index.theme`,
//!   `Inherits`, the `hicolor` fallback, sizes and scales (`Fixed` / `Scalable` / `Threshold`
//!   directories), `-symbolic` preference, the "strip the last dash segment" name fallback and
//!   unthemed `pixmaps`. Results are cached; call [`IconLoader::clear_cache`] after a theme or
//!   config change.
//! * [`desktop::DesktopEntries`] maps a window's `app_id` / class to its `.desktop` entry, name
//!   and icon.
//!
//! It only locates files (path, kind, nominal size); rasterising SVG/PNG is the renderer's job.
//! Everything takes injectable search paths, so tests use fixture directories.

pub mod desktop;
mod ini;
mod paths;

pub use paths::SearchPaths;

use ini::Ini;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconKind {
    Svg,
    Png,
    Xpm,
}

impl IconKind {
    /// Preference within one directory: vector first.
    const ALL: [(IconKind, &'static str); 3] = [
        (IconKind::Svg, "svg"),
        (IconKind::Png, "png"),
        (IconKind::Xpm, "xpm"),
    ];

    pub fn from_path(p: &Path) -> Option<IconKind> {
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        Self::ALL
            .iter()
            .find(|(_, e)| *e == ext)
            .map(|(k, _)| *k)
            .or(match ext.as_str() {
                "svgz" => Some(IconKind::Svg),
                _ => None,
            })
    }
}

/// A located icon file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub path: PathBuf,
    pub kind: IconKind,
    /// Nominal size of the directory it came from (0 for unthemed and absolute paths).
    pub size: u32,
    pub scale: u32,
    /// Lives in a `Scalable` directory (or is an SVG): any size renders crisply.
    pub scalable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirKind {
    Fixed,
    Scalable,
    Threshold,
}

#[derive(Debug, Clone)]
struct Dir {
    path: String,
    size: u32,
    scale: u32,
    kind: DirKind,
    min: u32,
    max: u32,
    threshold: u32,
}

impl Dir {
    fn matches(&self, size: u32, scale: u32) -> bool {
        if self.scale != scale {
            return false;
        }
        match self.kind {
            DirKind::Fixed => self.size == size,
            DirKind::Scalable => self.min <= size && size <= self.max,
            DirKind::Threshold => {
                self.size.saturating_sub(self.threshold) <= size
                    && size <= self.size + self.threshold
            }
        }
    }

    fn distance(&self, size: u32, scale: u32) -> u32 {
        let want = i64::from(size) * i64::from(scale);
        let s = i64::from(self.scale);
        let d = match self.kind {
            DirKind::Fixed => (i64::from(self.size) * s - want).abs(),
            DirKind::Scalable => {
                if want < i64::from(self.min) * s {
                    i64::from(self.min) * s - want
                } else if want > i64::from(self.max) * s {
                    want - i64::from(self.max) * s
                } else {
                    0
                }
            }
            DirKind::Threshold => {
                let lo = (i64::from(self.size) - i64::from(self.threshold)) * s;
                let hi = (i64::from(self.size) + i64::from(self.threshold)) * s;
                if want < lo {
                    lo - want
                } else if want > hi {
                    want - hi
                } else {
                    0
                }
            }
        };
        d.min(i64::from(u32::MAX)) as u32
    }
}

#[derive(Debug)]
struct Theme {
    /// Base directories that contain `<base>/<theme name>/`.
    roots: Vec<PathBuf>,
    dirs: Vec<Dir>,
    inherits: Vec<String>,
}

fn parse_theme(name: &str, bases: &[PathBuf]) -> Option<Theme> {
    let roots: Vec<PathBuf> = bases
        .iter()
        .map(|b| b.join(name))
        .filter(|r| r.is_dir())
        .collect();
    // The first index.theme found (highest-priority base) describes the theme.
    let ini = roots
        .iter()
        .find_map(|r| std::fs::read_to_string(r.join("index.theme")).ok())
        .map(|t| Ini::parse(&t));
    if roots.is_empty() {
        return None;
    }
    let Some(ini) = ini else {
        // A theme directory without an index.theme is unusable for sizes but its name is valid.
        return Some(Theme {
            roots,
            dirs: Vec::new(),
            inherits: Vec::new(),
        });
    };
    let mut names = ini.list("Icon Theme", "Directories");
    names.extend(ini.list("Icon Theme", "ScaledDirectories"));
    let num = |sec: &str, key: &str| ini.get(sec, key).and_then(|v| v.parse::<u32>().ok());
    let dirs = names
        .into_iter()
        .filter_map(|d| {
            let size = num(&d, "Size")?;
            let kind = match ini.get(&d, "Type") {
                Some("Fixed") => DirKind::Fixed,
                Some("Scalable") => DirKind::Scalable,
                _ => DirKind::Threshold,
            };
            Some(Dir {
                size,
                scale: num(&d, "Scale").unwrap_or(1).max(1),
                kind,
                min: num(&d, "MinSize").unwrap_or(size),
                max: num(&d, "MaxSize").unwrap_or(size),
                threshold: num(&d, "Threshold").unwrap_or(2),
                path: d,
            })
        })
        .collect();
    Some(Theme {
        roots,
        dirs,
        inherits: ini.list("Icon Theme", "Inherits"),
    })
}

/// What to look up.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Query {
    pub name: String,
    /// Theme name; `None` uses the loader's default theme.
    pub theme: Option<String>,
    /// Wanted size in logical pixels.
    pub size: u32,
    pub scale: u32,
    /// Try `<name>-symbolic` first (monochrome icons that follow the bar's colours).
    pub prefer_symbolic: bool,
}

impl Query {
    pub fn new(name: impl Into<String>, size: u32) -> Self {
        Self {
            name: name.into(),
            theme: None,
            size,
            scale: 1,
            prefer_symbolic: false,
        }
    }

    pub fn theme(mut self, theme: impl Into<String>) -> Self {
        self.theme = Some(theme.into());
        self
    }

    pub fn symbolic(mut self) -> Self {
        self.prefer_symbolic = true;
        self
    }

    pub fn scale(mut self, scale: u32) -> Self {
        self.scale = scale.max(1);
        self
    }
}

type Cache = HashMap<(Query, String), Option<Icon>>;

pub struct IconLoader {
    bases: Vec<PathBuf>,
    pixmaps: Vec<PathBuf>,
    default_theme: String,
    themes: Mutex<HashMap<String, Option<Arc<Theme>>>>,
    cache: Mutex<Cache>,
}

impl IconLoader {
    /// `bases` are the `icons` directories in priority order; `pixmaps` the unthemed fallbacks.
    pub fn new(
        bases: Vec<PathBuf>,
        pixmaps: Vec<PathBuf>,
        default_theme: impl Into<String>,
    ) -> Self {
        Self {
            bases,
            pixmaps,
            default_theme: default_theme.into(),
            themes: Mutex::default(),
            cache: Mutex::default(),
        }
    }

    pub fn from_paths(paths: &SearchPaths, default_theme: impl Into<String>) -> Self {
        Self::new(paths.icon_dirs(), paths.pixmap_dirs(), default_theme)
    }

    /// The real environment, with the user's configured theme (see [`configured_theme`]).
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok();
        let paths = SearchPaths::from_env(get);
        let theme = configured_theme(&paths).unwrap_or_else(|| "hicolor".into());
        Self::from_paths(&paths, theme)
    }

    pub fn default_theme(&self) -> &str {
        &self.default_theme
    }

    /// Forget cached lookups and parsed themes (theme switched, icons installed).
    pub fn clear_cache(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.themes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Names of the themes installed (directories with an `index.theme`), sorted. For pickers.
    pub fn installed_themes(&self) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        for b in &self.bases {
            let Ok(rd) = std::fs::read_dir(b) else {
                continue;
            };
            for e in rd.filter_map(Result::ok) {
                if e.path().join("index.theme").is_file() {
                    out.insert(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        out.into_iter().collect()
    }

    fn theme(&self, name: &str) -> Option<Arc<Theme>> {
        let mut themes = self.themes.lock().unwrap_or_else(|e| e.into_inner());
        themes
            .entry(name.to_string())
            .or_insert_with(|| parse_theme(name, &self.bases).map(Arc::new))
            .clone()
    }

    /// Find the icon, or `None`. Absolute paths pass through if the file exists.
    pub fn lookup(&self, q: &Query) -> Option<Icon> {
        if q.name.starts_with('/') {
            let p = PathBuf::from(&q.name);
            let kind = IconKind::from_path(&p)?;
            return p.is_file().then_some(Icon {
                path: p,
                kind,
                size: 0,
                scale: 1,
                scalable: kind == IconKind::Svg,
            });
        }
        let theme = q
            .theme
            .clone()
            .unwrap_or_else(|| self.default_theme.clone());
        let key = (q.clone(), theme.clone());
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return hit.clone();
        }
        let found = self.lookup_uncached(q, &theme);
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, found.clone());
        found
    }

    /// Shorthand for the common case.
    pub fn find(&self, name: &str, size: u32) -> Option<Icon> {
        self.lookup(&Query::new(name, size))
    }

    fn lookup_uncached(&self, q: &Query, theme: &str) -> Option<Icon> {
        // `foo-bar-baz` falls back to `foo-bar`, then `foo` (spec: generic icon names).
        let mut name = q.name.as_str();
        loop {
            if q.prefer_symbolic && !name.ends_with("-symbolic") {
                if let Some(i) = self.lookup_name(&format!("{name}-symbolic"), q, theme) {
                    return Some(i);
                }
            }
            if let Some(i) = self.lookup_name(name, q, theme) {
                return Some(i);
            }
            match name.rfind('-') {
                Some(i) if i > 0 => name = &name[..i],
                _ => return None,
            }
        }
    }

    fn lookup_name(&self, name: &str, q: &Query, theme: &str) -> Option<Icon> {
        let mut visited = HashSet::new();
        if let Some(i) = self.in_theme_chain(theme, name, q, &mut visited) {
            return Some(i);
        }
        if let Some(i) = self.in_theme_chain("hicolor", name, q, &mut visited) {
            return Some(i);
        }
        self.unthemed(name)
    }

    /// Depth-first through `Inherits`, as the spec's FindIconHelper does.
    fn in_theme_chain(
        &self,
        theme: &str,
        name: &str,
        q: &Query,
        visited: &mut HashSet<String>,
    ) -> Option<Icon> {
        if !visited.insert(theme.to_string()) {
            return None;
        }
        let t = self.theme(theme)?;
        if let Some(i) = find_in_theme(&t, name, q.size, q.scale) {
            return Some(i);
        }
        t.inherits
            .iter()
            .find_map(|p| self.in_theme_chain(p, name, q, visited))
    }

    fn unthemed(&self, name: &str) -> Option<Icon> {
        let dirs = self.pixmaps.iter().chain(self.bases.iter());
        for d in dirs {
            for (kind, ext) in IconKind::ALL {
                let p = d.join(format!("{name}.{ext}"));
                if p.is_file() {
                    return Some(Icon {
                        path: p,
                        kind,
                        size: 0,
                        scale: 1,
                        scalable: kind == IconKind::Svg,
                    });
                }
            }
        }
        None
    }
}

fn find_in_theme(t: &Theme, name: &str, size: u32, scale: u32) -> Option<Icon> {
    let file_in = |dir: &Dir| -> Option<(PathBuf, IconKind)> {
        for root in &t.roots {
            for (kind, ext) in IconKind::ALL {
                let p = root.join(&dir.path).join(format!("{name}.{ext}"));
                if p.is_file() {
                    return Some((p, kind));
                }
            }
        }
        None
    };
    let mk = |dir: &Dir, (path, kind): (PathBuf, IconKind)| Icon {
        path,
        kind,
        size: dir.size,
        scale: dir.scale,
        scalable: dir.kind == DirKind::Scalable || kind == IconKind::Svg,
    };
    // Pass 1: a directory that matches the size exactly.
    for d in t.dirs.iter().filter(|d| d.matches(size, scale)) {
        if let Some(f) = file_in(d) {
            return Some(mk(d, f));
        }
    }
    // Pass 2: the closest directory that has the file.
    let mut best: Option<(u32, Icon)> = None;
    for d in &t.dirs {
        let dist = d.distance(size, scale);
        if best.as_ref().is_some_and(|(b, _)| *b <= dist) {
            continue;
        }
        if let Some(f) = file_in(d) {
            best = Some((dist, mk(d, f)));
        }
    }
    best.map(|(_, i)| i)
}

/// The icon theme the user configured: GTK 4/3 `settings.ini` (`gtk-icon-theme-name`), then
/// KDE `kdeglobals` (`[Icons] Theme`).
pub fn configured_theme(paths: &SearchPaths) -> Option<String> {
    let cfg = paths.config_home.as_ref()?;
    for gtk in ["gtk-4.0", "gtk-3.0"] {
        if let Ok(t) = std::fs::read_to_string(cfg.join(gtk).join("settings.ini")) {
            if let Some(v) = Ini::parse(&t).get("Settings", "gtk-icon-theme-name") {
                if !v.is_empty() {
                    return Some(v.trim_matches('"').to_string());
                }
            }
        }
    }
    let t = std::fs::read_to_string(cfg.join("kdeglobals")).ok()?;
    Ini::parse(&t)
        .get("Icons", "Theme")
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests;
