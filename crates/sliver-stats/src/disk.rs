use rustix::fs::statvfs;
use std::path::Path;

/// Space on one filesystem, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disk {
    pub total: u64,
    /// Free to unprivileged users (`f_bavail`), what `df` calls Avail.
    pub free: u64,
}

impl Disk {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn percent_used(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f64 * 100.0 / self.total as f64
        }
    }

    pub fn percent_free(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            100.0 - self.percent_used()
        }
    }
}

/// `statvfs(path)`; `None` if the path does not exist.
pub fn disk(path: &Path) -> Option<Disk> {
    let s = statvfs(path).ok()?;
    let frsize = if s.f_frsize > 0 {
        s.f_frsize
    } else {
        s.f_bsize
    };
    Some(Disk {
        total: s.f_blocks * frsize,
        free: s.f_bavail * frsize,
    })
}
