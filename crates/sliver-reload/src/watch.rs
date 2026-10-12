//! inotify on the *directories* of the watched files, filtered by file name.
//!
//! Editors save by writing a temporary file and renaming it over the original, which replaces the
//! inode: a watch on the file itself would go deaf after the first save. Watching the parent
//! directory and matching names survives that, and also notices a config file being created.
//! No polling: the fd is readable only when something in a watched directory changed, and the
//! only timer is the debounce deadline after such an event.

use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Editors and `git checkout` write several times in a burst; wait for quiet.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct Watcher {
    fd: OwnedFd,
    /// Watch descriptor → directory.
    by_wd: HashMap<i32, PathBuf>,
    dirs: HashMap<PathBuf, i32>,
    /// Directory → file names that matter in it.
    names: HashMap<PathBuf, HashSet<OsString>>,
    /// Time of the last relevant event, until the debounce elapses.
    pending: Option<Instant>,
    debounce: Duration,
}

/// The directory to watch (canonical, so `profiles/../` or a symlinked config dir and the plain
/// path share one entry: inotify gives one watch descriptor per inode, and two keys for it would
/// lose one key's names) and the file name in it.
fn split(p: &Path) -> (PathBuf, OsString) {
    let dir = match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    (
        dir,
        p.file_name().map(|n| n.to_os_string()).unwrap_or_default(),
    )
}

impl Watcher {
    pub fn new() -> io::Result<Watcher> {
        Ok(Watcher {
            fd: inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?,
            by_wd: HashMap::new(),
            dirs: HashMap::new(),
            names: HashMap::new(),
            pending: None,
            debounce: DEBOUNCE,
        })
    }

    /// Change how long events must be quiet before [`Watcher::take_due`] reports them.
    pub fn with_debounce(mut self, d: Duration) -> Watcher {
        self.debounce = d;
        self
    }

    /// (Re)arm for exactly these files: new directories are watched, directories nobody needs any
    /// more are released. Call it after every reload, because includes come and go. Files whose
    /// directory does not exist yet are skipped quietly and retried on the next call.
    pub fn set_files<'a>(&mut self, files: impl IntoIterator<Item = &'a Path>) {
        let mut want: HashMap<PathBuf, HashSet<OsString>> = HashMap::new();
        for f in files {
            let (dir, name) = split(f);
            want.entry(dir).or_default().insert(name);
        }
        let stale: Vec<PathBuf> = self
            .dirs
            .keys()
            .filter(|d| !want.contains_key(*d))
            .cloned()
            .collect();
        for d in stale {
            if let Some(wd) = self.dirs.remove(&d) {
                self.by_wd.remove(&wd);
                let _ = inotify::remove_watch(&self.fd, wd);
            }
        }
        let flags = WatchFlags::CLOSE_WRITE
            | WatchFlags::MOVED_TO
            | WatchFlags::MOVED_FROM
            | WatchFlags::CREATE
            | WatchFlags::DELETE
            | WatchFlags::DELETE_SELF
            | WatchFlags::MOVE_SELF;
        for dir in want.keys() {
            if self.dirs.contains_key(dir) {
                continue;
            }
            if let Ok(wd) = inotify::add_watch(&self.fd, dir, flags) {
                self.dirs.insert(dir.clone(), wd);
                self.by_wd.insert(wd, dir.clone());
            }
        }
        self.names = want;
    }

    /// Poll this fd with the agent's others.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Read every queued event; remember if one concerned a watched file.
    pub fn on_readable(&mut self, now: Instant) {
        let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        let mut touched = false;
        let mut gone = Vec::new();
        loop {
            match reader.next() {
                Ok(ev) => {
                    let flags = ev.events();
                    if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                        touched = true;
                        continue;
                    }
                    if flags.contains(ReadFlags::IGNORED)
                        || flags.contains(ReadFlags::DELETE_SELF)
                        || flags.contains(ReadFlags::MOVE_SELF)
                    {
                        // The directory itself went away: forget it and treat it as a change.
                        gone.push(ev.wd());
                        touched = true;
                        continue;
                    }
                    if flags.contains(ReadFlags::ISDIR) {
                        continue;
                    }
                    let (Some(dir), Some(name)) = (self.by_wd.get(&ev.wd()), ev.file_name()) else {
                        continue;
                    };
                    let name = OsString::from_vec(name.to_bytes().to_vec());
                    if self.names.get(dir).is_some_and(|n| n.contains(&name)) {
                        touched = true;
                    }
                }
                Err(Errno::INTR) => continue,
                Err(_) => break, // WouldBlock: drained
            }
        }
        for wd in gone {
            if let Some(d) = self.by_wd.remove(&wd) {
                self.dirs.remove(&d);
            }
        }
        if touched {
            self.pending = Some(now);
        }
    }

    /// When the debounced change becomes due, if one is pending: use it as the poll timeout.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.map(|t| t + self.debounce)
    }

    /// True once, when the debounce has elapsed since the last relevant event.
    pub fn take_due(&mut self, now: Instant) -> bool {
        match self.deadline() {
            Some(d) if now >= d => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }

    /// Mark a change by hand (`sliverctl reload`, SIGUSR2): due immediately.
    pub fn force(&mut self, now: Instant) {
        self.pending = Some(now.checked_sub(self.debounce).unwrap_or(now));
    }

    #[cfg(test)]
    pub(crate) fn watched_dirs(&self) -> usize {
        self.dirs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rearming_releases_stale_directories() {
        let root = std::env::temp_dir().join(format!("sliver-watch-unit-{}", std::process::id()));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let mut w = Watcher::new().unwrap();
        w.set_files([a.join("x").as_path(), b.join("y").as_path()]);
        assert_eq!(w.watched_dirs(), 2);
        w.set_files([a.join("x").as_path(), a.join("z").as_path()]);
        assert_eq!(w.watched_dirs(), 1);
        w.set_files([root.join("missing/dir/file").as_path()]);
        assert_eq!(
            w.watched_dirs(),
            0,
            "a missing directory is skipped, not an error"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn debounce_and_force() {
        let mut w = Watcher::new()
            .unwrap()
            .with_debounce(Duration::from_millis(100));
        let t0 = Instant::now();
        assert!(!w.take_due(t0));
        w.pending = Some(t0);
        assert_eq!(w.deadline(), Some(t0 + Duration::from_millis(100)));
        assert!(!w.take_due(t0 + Duration::from_millis(99)));
        assert!(w.take_due(t0 + Duration::from_millis(100)));
        assert!(!w.take_due(t0 + Duration::from_secs(5)), "reported once");
        w.force(t0 + Duration::from_secs(1));
        assert!(w.take_due(t0 + Duration::from_secs(1)));
    }
}
