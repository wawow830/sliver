//! Waiting for sliverd's socket without polling (invariant 3): while tiny-dfr drives the bar
//! there is no `/run/sliver/sliverd.sock`, possibly for days, so the agent sleeps on inotify
//! until the socket (or its directory) appears.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::inotify;

/// Returns once `path` exists, or after `max` (a safety net in case a watch misses something).
pub fn wait_for_path(path: &Path, max: Duration) {
    let deadline = Instant::now() + max;
    while !path.exists() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        // Watch the nearest existing ancestor: the socket's directory may not exist yet
        // (`/run/sliver` is created when sliverd.socket starts).
        let Some(dir) = nearest_existing_dir(path) else {
            std::thread::sleep(left.min(Duration::from_secs(60)));
            continue;
        };
        let Ok(fd) = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)
        else {
            std::thread::sleep(left.min(Duration::from_secs(60)));
            continue;
        };
        let flags = inotify::WatchFlags::CREATE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::ATTRIB;
        if inotify::add_watch(&fd, &dir, flags).is_err() {
            std::thread::sleep(left.min(Duration::from_secs(60)));
            continue;
        }
        // Re-check after arming the watch, so a creation in between isn't missed.
        if path.exists() {
            return;
        }
        let timeout = Timespec {
            tv_sec: left.as_secs() as i64,
            tv_nsec: left.subsec_nanos() as i64,
        };
        let mut fds = [PollFd::new(&fd, PollFlags::IN)];
        let _ = poll(&mut fds, Some(&timeout));
        // Something changed in `dir` (or time's up): loop and look again.
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
        wait_for_path(&target, Duration::from_secs(10));
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
        wait_for_path(&root.join("never/there"), Duration::from_millis(150));
        assert!(start.elapsed() >= Duration::from_millis(150));
    }
}
