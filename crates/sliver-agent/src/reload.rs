//! Config hot reload (customization.md §1, T26): watch the config, its includes and the
//! stylesheets with inotify. Nothing polls.
//!
//! We watch the *directories*, not the files. Editors and `sliver_config::Editor` save by writing
//! a temp file and renaming it over the old one, which a file watch would lose. Missing
//! directories (no `~/.config/sliver` yet) are watched through their nearest existing ancestor,
//! so creating the config later still triggers a reload.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

/// Editors save in several steps. Wait this long after the last event before reloading.
pub const SETTLE: Duration = Duration::from_millis(200);

pub struct Watcher {
    fd: OwnedFd,
    /// Watch descriptor → (directory, names in it that matter).
    watches: HashMap<i32, (PathBuf, HashSet<OsString>)>,
}

impl Watcher {
    pub fn new() -> io::Result<Watcher> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        Ok(Watcher {
            fd,
            watches: HashMap::new(),
        })
    }

    /// Readable when [`Watcher::drain`] has events.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Watches exactly `files` (replacing earlier watches). Call again after each reload: the set
    /// of includes may have changed, and a created directory can now be watched directly.
    pub fn watch(&mut self, files: &[PathBuf]) {
        for wd in std::mem::take(&mut self.watches).into_keys() {
            let _ = inotify::remove_watch(&self.fd, wd);
        }
        let mut want: HashMap<PathBuf, HashSet<OsString>> = HashMap::new();
        for f in files {
            if let Some((dir, name)) = watch_point(f) {
                want.entry(dir).or_default().insert(name);
            }
        }
        let flags = WatchFlags::CLOSE_WRITE
            | WatchFlags::MOVED_TO
            | WatchFlags::MOVED_FROM
            | WatchFlags::CREATE
            | WatchFlags::DELETE
            | WatchFlags::ONLYDIR;
        for (dir, names) in want {
            match inotify::add_watch(&self.fd, &dir, flags) {
                Ok(wd) => {
                    let e = self
                        .watches
                        .entry(wd)
                        .or_insert_with(|| (dir.clone(), HashSet::new()));
                    e.1.extend(names);
                }
                Err(e) => eprintln!("sliver-agent: can't watch {}: {e}", dir.display()),
            }
        }
    }

    /// Reads all pending events. Returns whether any of them touched a watched name.
    pub fn drain(&mut self) -> bool {
        let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        let mut relevant = false;
        loop {
            match reader.next() {
                Ok(ev) => {
                    if ev.events().contains(ReadFlags::QUEUE_OVERFLOW) {
                        relevant = true;
                        continue;
                    }
                    let name = ev
                        .file_name()
                        .map(|n| OsString::from(n.to_string_lossy().into_owned()));
                    if let (Some((_, names)), Some(n)) = (self.watches.get(&ev.wd()), name) {
                        relevant |= names.contains(&n);
                    }
                }
                Err(rustix::io::Errno::AGAIN) | Err(rustix::io::Errno::INTR) => break,
                Err(_) => break,
            }
        }
        relevant
    }

    #[cfg(test)]
    fn dirs(&self) -> Vec<PathBuf> {
        let mut d: Vec<PathBuf> = self.watches.values().map(|(p, _)| p.clone()).collect();
        d.sort();
        d
    }
}

/// The directory to watch for `file`, and the name in it that matters: the file's own parent if
/// it exists, else the nearest existing ancestor and the first missing component under it.
fn watch_point(file: &Path) -> Option<(PathBuf, OsString)> {
    let file = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(file)
    };
    let mut dir = file.parent()?.to_path_buf();
    let mut name = file.file_name()?.to_os_string();
    while !dir.is_dir() {
        name = dir.file_name()?.to_os_string();
        dir = dir.parent()?.to_path_buf();
    }
    // Normalize `..`/`.` lexically so equal directories share one watch.
    let dir: PathBuf = dir
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    Some((dir, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("agent-reload-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// inotify delivers asynchronously but promptly; give it a moment.
    fn settle(w: &mut Watcher) -> bool {
        std::thread::sleep(Duration::from_millis(50));
        w.drain()
    }

    #[test]
    fn edits_renames_and_creation() {
        let d = tmp("edits");
        let cfg = d.join("sliver/config.toml");
        let mut w = Watcher::new().unwrap();
        // The directory doesn't exist yet: watch its parent for `sliver`.
        w.watch(std::slice::from_ref(&cfg));
        assert_eq!(w.dirs(), std::slice::from_ref(&d));
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        assert!(settle(&mut w), "creating the directory counts");
        w.watch(std::slice::from_ref(&cfg));
        assert_eq!(w.dirs(), [d.join("sliver")]);

        std::fs::write(&cfg, "version = 1\n").unwrap();
        assert!(settle(&mut w), "write");
        std::fs::write(d.join("sliver/unrelated.txt"), "x").unwrap();
        assert!(!settle(&mut w), "other files in the directory don't count");
        // Atomic save, as sliver_config::Editor does it.
        std::fs::write(d.join("sliver/.config.toml.tmp"), "version = 1\n# new\n").unwrap();
        std::fs::rename(d.join("sliver/.config.toml.tmp"), &cfg).unwrap();
        assert!(settle(&mut w), "rename over the file");
        assert!(!settle(&mut w), "drained");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
