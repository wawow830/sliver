//! `sliver-reload`: hot reload for the config and the stylesheet (customization.md §1).
//!
//! * [`Watcher`]: inotify on the watched files' directories (survives atomic-rename saves, sees a
//!   config being created), debounced; one fd to poll, one deadline for the poll timeout.
//! * [`Reloader`]: loads the config and stylesheet, remembers the last good [`Applied`] and, when
//!   a change turns out invalid, keeps it and reports why: **an invalid config is never
//!   applied**. Valid changes come back as [`Outcome::Applied`]; identical content is
//!   [`Outcome::Unchanged`], so a `touch` does not redraw the bar.
//!
//! The agent's loop: poll [`Reloader::as_fd`]; on readable call [`Reloader::on_readable`]; use
//! [`Reloader::deadline`] as the poll timeout and call [`Reloader::poll_due`] when it passes;
//! `sliverctl reload` and SIGUSR2 call [`Reloader::reload_now`].

mod reload;
mod watch;

pub use reload::{Applied, Outcome, Reloader};
pub use watch::{DEBOUNCE, Watcher};
