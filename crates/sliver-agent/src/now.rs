//! The core never reads time itself: the loop reads [`Now`] once per wakeup and every deadline,
//! transition, gesture and timer is measured from it.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local};

/// One reading of the clocks: monotonic µs (what sliverd stamps touches with, and what gestures,
/// macros and timers count in), the same moment as an [`Instant`] (the renderer's transitions),
/// and wall time (what clocks show).
#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub mono_us: u64,
    pub instant: Instant,
    pub wall: DateTime<Local>,
}

impl Now {
    pub fn read() -> Now {
        let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        Now {
            mono_us: t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1000,
            instant: Instant::now(),
            wall: Local::now(),
        }
    }

    /// Showing `wall` on the clocks, now (previews, tests).
    pub fn at(wall: DateTime<Local>) -> Now {
        Now {
            wall,
            ..Now::read()
        }
    }

    /// The same clocks `d` later.
    pub fn after(self, d: Duration) -> Now {
        Now {
            mono_us: self.mono_us + d.as_micros() as u64,
            instant: self.instant + d,
            wall: self.wall + chrono::Duration::from_std(d).unwrap_or_default(),
        }
    }
}
