//! Who owns the active session on seat0: only that uid's agent may drive the bar (api.md §6).
//!
//! sd-login keeps this in `/run/systemd/seats/<seat>` (`ACTIVE_UID=…`), rewritten by rename on
//! every session switch. [`SeatWatch`] reads it and watches the directory with inotify, which is
//! exactly what `sd_seat_get_active` + `sd_login_monitor` do, without linking libsystemd or
//! talking D-Bus from the privileged daemon.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::inotify;

pub const LOGIND_SEATS: &str = "/run/systemd/seats";

/// A pollable source of the active uid.
pub trait SessionSource: AsFd {
    /// Drains change notifications (non-blocking) and returns the current active uid.
    fn active_uid(&mut self) -> Option<u32>;
}

pub struct SeatWatch {
    file: PathBuf,
    inotify: OwnedFd,
}

impl SeatWatch {
    /// Watches `dir/seat` (normally [`LOGIND_SEATS`]/`seat0`).
    pub fn new(dir: impl AsRef<Path>, seat: &str) -> io::Result<SeatWatch> {
        let dir = dir.as_ref();
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
        inotify::add_watch(
            &fd,
            dir,
            inotify::WatchFlags::MOVED_TO
                | inotify::WatchFlags::CLOSE_WRITE
                | inotify::WatchFlags::CREATE
                | inotify::WatchFlags::DELETE,
        )?;
        Ok(SeatWatch {
            file: dir.join(seat),
            inotify: fd,
        })
    }

    fn read(&self) -> Option<u32> {
        parse_active_uid(&std::fs::read_to_string(&self.file).ok()?)
    }
}

impl AsFd for SeatWatch {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.inotify.as_fd()
    }
}

impl SessionSource for SeatWatch {
    fn active_uid(&mut self) -> Option<u32> {
        let mut buf = [std::mem::MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.inotify, &mut buf);
        // Any event means "re-read". Stop at EAGAIN (or any error: re-reading is always safe).
        while reader.next().is_ok() {}
        self.read()
    }
}

/// `ACTIVE_UID=1000` from an sd-login seat file.
pub fn parse_active_uid(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|l| l.strip_prefix("ACTIVE_UID="))
        .and_then(|v| v.trim().parse().ok())
}

/// Always the same uid, for `--fake-hardware` dev runs and tests. Never readable.
pub struct FixedSession {
    uid: Option<u32>,
    fd: OwnedFd,
}

impl FixedSession {
    pub fn new(uid: Option<u32>) -> io::Result<FixedSession> {
        // An fd that never becomes readable: an inotify instance with no watches.
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
        Ok(FixedSession { uid, fd })
    }
}

impl AsFd for FixedSession {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl SessionSource for FixedSession {
    fn active_uid(&mut self) -> Option<u32> {
        self.uid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seat_file() {
        let f = "# This is private data. Do not parse.\nIS_SEAT0=1\nACTIVE=c2\nACTIVE_UID=1000\nSESSIONS=c2\nUIDS=1000\n";
        assert_eq!(parse_active_uid(f), Some(1000));
        assert_eq!(parse_active_uid("IS_SEAT0=1\nACTIVE=\n"), None);
    }

    #[test]
    fn watches_session_switches() {
        // Under target/ (not the quota'd /tmp). CARGO_TARGET_TMPDIR only exists for integration tests.
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/sliverd-seats");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = SeatWatch::new(&dir, "seat0").unwrap();
        assert_eq!(w.active_uid(), None);
        // sd-login writes a temp file and renames it into place.
        let write = |uid: u32| {
            let tmp = dir.join(".#seat0abc");
            std::fs::write(&tmp, format!("IS_SEAT0=1\nACTIVE_UID={uid}\n")).unwrap();
            std::fs::rename(&tmp, dir.join("seat0")).unwrap();
        };
        write(1000);
        let mut fds = [rustix::event::PollFd::new(&w, rustix::event::PollFlags::IN)];
        let zero = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(rustix::event::poll(&mut fds, Some(&zero)).unwrap(), 1);
        assert_eq!(w.active_uid(), Some(1000));
        let mut fds = [rustix::event::PollFd::new(&w, rustix::event::PollFlags::IN)];
        assert_eq!(
            rustix::event::poll(&mut fds, Some(&zero)).unwrap(),
            0,
            "drained"
        );
        write(2000);
        assert_eq!(w.active_uid(), Some(2000));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
