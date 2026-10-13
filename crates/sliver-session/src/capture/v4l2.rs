//! A camera opened straight through V4L2 (Zoom, Chrome and others on Intel T2 Macs, whose
//! FaceTime camera isn't behind PipeWire): PipeWire never sees it, so we ask who holds a
//! `/dev/video*` node open.
//!
//! Event-driven: inotify `IN_OPEN`/`IN_CLOSE` on each node, and on `/dev` itself for nodes that
//! come and go. After an event (and a short settle, so a udev probe opening and closing the node
//! in a few milliseconds is not a camera in use) `/proc/*/fd` is scanned for links to the nodes.
//! Nothing runs between events. Our own scan only reads symlinks, which never opens the device.
//!
//! What is excluded: PipeWire and WirePlumber (they hold nodes while enumerating and while
//! streaming; real PipeWire camera use is counted through its stream nodes), and udev's probes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::inotify::{self, CreateFlags, WatchFlags};

/// Processes that touch camera nodes without being a camera user.
const IGNORED: &[&str] = &[
    "pipewire",
    "pipewire-media-session",
    "wireplumber",
    "wireplumber-main",
    "v4l_id",
    "udevadm",
    "systemd-udevd",
    "(udev-worker)",
    "sliver-agent",
];

/// How long after an open/close event before looking, so that a probe is not a camera in use.
const SETTLE: Duration = if cfg!(test) {
    Duration::from_millis(60)
} else {
    Duration::from_millis(400)
};

/// The `video0`, `video1`... nodes in `dev`.
fn nodes(dev: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dev) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.strip_prefix("video")
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

/// Names of the processes holding any of `dev`'s video nodes, sorted, without duplicates.
pub fn holders(dev: &Path, proc_root: &Path) -> Vec<String> {
    let nodes: BTreeSet<PathBuf> = nodes(dev).into_iter().collect();
    let mut out = BTreeSet::new();
    let Ok(rd) = std::fs::read_dir(proc_root) else {
        return Vec::new();
    };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name();
        if !name.to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // Other users' processes refuse this: they are not ours to report.
        let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else {
            continue;
        };
        let holds = fds
            .filter_map(Result::ok)
            .filter_map(|f| std::fs::read_link(f.path()).ok())
            .any(|target| nodes.contains(&target));
        if !holds {
            continue;
        }
        let comm = std::fs::read_to_string(e.path().join("comm"))
            .map(|c| c.trim().to_owned())
            .unwrap_or_default();
        if !comm.is_empty() && !IGNORED.contains(&comm.as_str()) {
            out.insert(comm);
        }
    }
    out.into_iter().collect()
}

/// Watches `dev` and calls `report` with the holders at start and after every open/close.
/// Blocks forever (or until `report` returns `false`): run it on its own thread.
pub fn watch(dev: &Path, proc_root: &Path, mut report: impl FnMut(Vec<String>) -> bool) {
    let Ok(fd) = inotify::init(CreateFlags::CLOEXEC) else {
        return;
    };
    let mut watched = BTreeSet::new();
    // `/dev` itself: nodes appearing and disappearing.
    let _ = inotify::add_watch(&fd, dev, WatchFlags::CREATE | WatchFlags::DELETE);
    let mut last: Option<Vec<String>> = None;
    loop {
        for n in nodes(dev) {
            if watched.insert(n.clone()) {
                let _ = inotify::add_watch(
                    &fd,
                    &n,
                    WatchFlags::OPEN | WatchFlags::CLOSE_WRITE | WatchFlags::CLOSE_NOWRITE,
                );
            }
        }
        let now = holders(dev, proc_root);
        if last.as_ref() != Some(&now) {
            last = Some(now.clone());
            if !report(now) {
                return;
            }
        }
        // Sleep in the kernel until something happens, then let a burst settle.
        let mut buf = [0u8; 4096];
        if rustix::io::read(&fd, &mut buf).is_err() {
            return;
        }
        std::thread::sleep(SETTLE);
        // Everything that arrived meanwhile is one look.
        let nb = rustix::fs::fcntl_getfl(&fd).unwrap_or(rustix::fs::OFlags::empty());
        let _ = rustix::fs::fcntl_setfl(&fd, nb | rustix::fs::OFlags::NONBLOCK);
        while rustix::io::read(&fd, &mut buf).is_ok_and(|n| n > 0) {}
        let _ = rustix::fs::fcntl_setfl(&fd, nb);
        // Deleted nodes drop their watch by themselves; forget them so a returning node is re-added.
        watched.retain(|n| n.exists());
    }
}

#[cfg(test)]
mod tests;
