use crate::watch::Watcher;
use sliver_config::{Config, Loaded};
use sliver_render::Stylesheet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// A config and stylesheet that are safe to use.
#[derive(Clone)]
pub struct Applied {
    pub config: Config,
    pub sheet: Stylesheet,
    /// The config file that was read (`None` = built-in defaults only).
    pub config_path: Option<PathBuf>,
    /// Config and stylesheet warnings, one line each.
    pub warnings: Vec<String>,
    /// Only on the very first load: problems that forced the built-in defaults.
    pub errors: Vec<String>,
}

pub enum Outcome {
    /// A new valid config/stylesheet: re-resolve and redraw.
    Applied(Box<Applied>),
    /// The files changed but the result is invalid; the last good one stays live.
    Rejected(Vec<String>),
    /// Nothing that matters changed (a touch, a save without edits).
    Unchanged,
}

pub struct Reloader {
    explicit: Option<PathBuf>,
    applied: Applied,
    fingerprint: u64,
    watcher: Watcher,
}

fn fingerprint(files: &[PathBuf]) -> u64 {
    let mut h = DefaultHasher::new();
    for f in files {
        f.hash(&mut h);
        std::fs::read(f).ok().hash(&mut h);
    }
    h.finish()
}

/// `stylesheet = …` (relative to the config), else `style.css` next to it, on top of the default
/// theme. The file must be readable; its own diagnostics are warnings.
fn load_sheet(
    config: &Config,
    config_path: Option<&Path>,
) -> Result<(Stylesheet, Vec<String>), String> {
    let mut sheet = Stylesheet::sliver();
    let dir = config_path.and_then(Path::parent);
    let user = match (&config.stylesheet, dir) {
        (Some(s), Some(d)) => Some(d.join(s)),
        (Some(s), None) => Some(PathBuf::from(s)),
        (None, Some(d)) => Some(d.join("style.css")).filter(|p| p.is_file()),
        (None, None) => None,
    };
    let mut warnings = Vec::new();
    if let Some(p) = user {
        let mut diags = Vec::new();
        sheet
            .add_file(&p, &mut diags)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        warnings.extend(diags.iter().map(|d| d.to_string()));
    }
    Ok((sheet, warnings))
}

struct Read {
    loaded: Loaded,
    config_path: Option<PathBuf>,
}

fn read(explicit: Option<&Path>) -> Read {
    let found = explicit
        .map(Path::to_path_buf)
        .or_else(sliver_config::find_config);
    let mut loaded = match &found {
        Some(p) => sliver_config::load_file(p),
        None => sliver_config::load(),
    };
    // Whole-config errors (a layer pointing at a missing layout, a group containing itself) block
    // applying just like parse errors. Without this they'd be applied and fail at render time.
    if loaded.diagnostics.ok() {
        let v = loaded.config.validate(sliver_config::Hardware::detect());
        loaded.diagnostics.extend(v);
    }
    Read {
        loaded,
        config_path: found,
    }
}

fn errors_of(l: &Loaded) -> Vec<String> {
    l.diagnostics.errors.iter().map(|e| e.to_string()).collect()
}

impl Reloader {
    /// Load for the first time. An invalid config falls back to the built-in defaults (the bar must
    /// come up), with the reasons in `Applied::errors`. `explicit` is `--config FILE`.
    pub fn start(explicit: Option<&Path>) -> io::Result<(Reloader, Applied)> {
        let r = read(explicit);
        let mut errors = errors_of(&r.loaded);
        let mut warnings: Vec<String> = r
            .loaded
            .diagnostics
            .warnings
            .iter()
            .map(|w| w.to_string())
            .collect();
        let config = if r.loaded.diagnostics.ok() {
            r.loaded.config.clone()
        } else {
            sliver_config::load_str("", Path::new(".")).config
        };
        let (sheet, sheet_warnings) = match load_sheet(&config, r.config_path.as_deref()) {
            Ok(s) => s,
            Err(e) => {
                errors.push(e);
                (Stylesheet::sliver(), Vec::new())
            }
        };
        warnings.extend(sheet_warnings);
        let applied = Applied {
            config,
            sheet,
            config_path: r.config_path.clone(),
            warnings,
            errors,
        };
        let mut me = Reloader {
            explicit: explicit.map(Path::to_path_buf),
            fingerprint: 0,
            watcher: Watcher::new()?,
            applied: applied.clone(),
        };
        me.rearm(&r.loaded.files);
        Ok((me, applied))
    }

    /// Watch every file that went into the result, the stylesheet's imports, and the places a
    /// config could appear.
    fn rearm(&mut self, config_files: &[PathBuf]) {
        let mut files: Vec<PathBuf> = config_files.to_vec();
        files.extend(self.applied.sheet.files().iter().cloned());
        match &self.explicit {
            Some(p) => files.push(p.clone()),
            None => files.extend(sliver_config::search_path()),
        }
        if let (Some(cfg), Some(dir)) = (
            &self.applied.config.stylesheet,
            self.applied.config_path.as_deref().and_then(Path::parent),
        ) {
            files.push(dir.join(cfg));
        }
        self.fingerprint = fingerprint(&files);
        self.watcher.set_files(files.iter().map(PathBuf::as_path));
    }

    pub fn current(&self) -> &Applied {
        &self.applied
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.watcher.as_fd()
    }

    /// Call when [`Reloader::as_fd`] is readable.
    pub fn on_readable(&mut self, now: Instant) {
        self.watcher.on_readable(now);
    }

    /// Poll timeout: when a debounced change becomes due.
    pub fn deadline(&self) -> Option<Instant> {
        self.watcher.deadline()
    }

    /// Reload if a change is due.
    pub fn poll_due(&mut self, now: Instant) -> Option<Outcome> {
        self.watcher.take_due(now).then(|| self.reload_now())
    }

    /// Reload now (`sliverctl reload`, SIGUSR2), whether or not anything changed on disk.
    pub fn reload_now(&mut self) -> Outcome {
        let r = read(self.explicit.as_deref());
        if !r.loaded.diagnostics.ok() {
            // Keep watching what the broken config references, plus the old files, so fixing
            // any of them triggers the next attempt.
            let mut files = r.loaded.files.clone();
            files.extend(self.watched_files());
            self.watcher.set_files(files.iter().map(PathBuf::as_path));
            return Outcome::Rejected(errors_of(&r.loaded));
        }
        let (sheet, sheet_warnings) = match load_sheet(&r.loaded.config, r.config_path.as_deref()) {
            Ok(s) => s,
            Err(e) => return Outcome::Rejected(vec![e]),
        };
        let mut warnings: Vec<String> = r
            .loaded
            .diagnostics
            .warnings
            .iter()
            .map(|w| w.to_string())
            .collect();
        warnings.extend(sheet_warnings);
        let next = Applied {
            config: r.loaded.config.clone(),
            sheet,
            config_path: r.config_path.clone(),
            warnings,
            errors: Vec::new(),
        };
        let before = self.fingerprint;
        let old = std::mem::replace(&mut self.applied, next);
        self.rearm(&r.loaded.files);
        if self.fingerprint == before && old.config == self.applied.config {
            self.applied = Applied {
                errors: Vec::new(),
                ..old
            };
            return Outcome::Unchanged;
        }
        Outcome::Applied(Box::new(self.applied.clone()))
    }

    fn watched_files(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.applied.config_path.iter().cloned().collect();
        v.extend(self.applied.sheet.files().iter().cloned());
        v
    }
}
