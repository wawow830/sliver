//! Waiting for sliverd's socket without polling (invariant 3): while tiny-dfr drives the bar
//! there is no `/run/sliver/sliverd.sock`, possibly for days, so the agent sleeps on inotify
//! until the socket (or its directory) appears.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::inotify;

/// Returns once `path` exists, or after `max` if given (`None`: wait as long as it takes, with
/// no wakeups at all while nothing changes).
///
/// It can't miss the path: the watch is on the nearest existing ancestor and is re-armed every
/// time something changes there (a directory on the way appeared), the path is re-checked
/// after each arming, and a watched directory being deleted or moved (`DELETE_SELF`,
/// `MOVE_SELF`) also wakes us to re-arm on whatever exists now.
pub fn wait_for_path(path: &Path, max: Option<Duration>) {
    let deadline = max.map(|m| Instant::now() + m);
    while !path.exists() {
        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if left.is_some_and(|l| l.is_zero()) {
            return;
        }
        // Only if inotify itself fails (not expected): retry slowly.
        let fallback = left
            .unwrap_or(Duration::from_secs(60))
            .min(Duration::from_secs(60));
        let Some(dir) = nearest_existing_dir(path) else {
            std::thread::sleep(fallback);
            continue;
        };
        let Ok(fd) = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)
        else {
            std::thread::sleep(fallback);
            continue;
        };
        let flags = inotify::WatchFlags::CREATE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::ATTRIB
            | inotify::WatchFlags::DELETE_SELF
            | inotify::WatchFlags::MOVE_SELF;
        if inotify::add_watch(&fd, &dir, flags).is_err() {
            std::thread::sleep(fallback);
            continue;
        }
        // Re-check after arming the watch, so a creation in between isn't missed.
        if path.exists() {
            return;
        }
        let timeout = left.map(|l| Timespec {
            tv_sec: l.as_secs() as i64,
            tv_nsec: l.subsec_nanos() as i64,
        });
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let _ = poll(&mut fds, timeout.as_ref());
        // Something changed in or to `dir` (or time's up): look again and re-arm.
    }
}

fn nearest_existing_dir(path: &Path) -> Option<PathBuf> {
    let mut p = path.parent()?;
    loop {
        if p.is_dir() {
            return Some(p.to_path_buf());
        }
        p = p.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakes_when_the_path_appears_even_in_a_new_directory() {
        let root = std::env::temp_dir().join(format!("sliver-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("sub/sliverd.sock");
        let t = target.clone();
        let maker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::create_dir(t.parent().unwrap()).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(&t, b"").unwrap();
        });
        let start = Instant::now();
        wait_for_path(&target, Some(Duration::from_secs(10)));
        let took = start.elapsed();
        maker.join().unwrap();
        assert!(target.exists());
        assert!(
            took < Duration::from_secs(2),
            "woke by inotify, not the timeout: {took:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
        // Times out when nothing comes.
        let start = Instant::now();
        wait_for_path(&root.join("never/there"), Some(Duration::from_millis(150)));
        assert!(start.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn survives_the_watched_directory_being_replaced() {
        let root = std::env::temp_dir().join(format!("sliver-wait2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("run")).unwrap();
        let target = root.join("run/sliverd.sock");
        let (r, t) = (root.clone(), target.clone());
        let maker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::remove_dir(r.join("run")).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            std::fs::create_dir(r.join("run")).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            std::fs::write(&t, b"").unwrap();
        });
        let start = Instant::now();
        wait_for_path(&target, Some(Duration::from_secs(10)));
        maker.join().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
