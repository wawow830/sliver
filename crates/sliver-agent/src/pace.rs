//! Frame pacing for content that changes on its own (invariant 3, T99): published items, script
//! output and other feeds may change hundreds of times a second (an app calling `Update` in a
//! loop, a script printing as fast as it can). Each change only marks the bar stale; it's
//! redrawn at most [`CONTENT_FPS`] times a second, and the last change always gets drawn.
//!
//! Touches, layer switches and animations don't go through this: they draw at once.

use std::time::{Duration, Instant};

/// Redraws per second, at most, for content changes.
pub const CONTENT_FPS: u32 = 30;

#[derive(Debug, Clone)]
pub struct Pacer {
    min: Duration,
    last: Option<Instant>,
    pending: bool,
}

impl Default for Pacer {
    fn default() -> Pacer {
        Pacer::new(CONTENT_FPS)
    }
}

impl Pacer {
    pub fn new(fps: u32) -> Pacer {
        Pacer {
            min: Duration::from_secs(1) / fps.max(1),
            last: None,
            pending: false,
        }
    }

    /// Content changed; draw it when the pace allows.
    pub fn changed(&mut self) {
        self.pending = true;
    }

    /// Whether a change is waiting to be drawn.
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// A pending change may be drawn now.
    pub fn due(&self, now: Instant) -> bool {
        self.pending && self.last.is_none_or(|t| now.duration_since(t) >= self.min)
    }

    /// A frame was drawn (for any reason): it includes every change so far.
    pub fn drawn(&mut self, now: Instant) {
        self.last = Some(now);
        self.pending = false;
    }

    /// How long until a pending change is due (`None` when nothing is pending): the main loop's
    /// poll timeout, so a held-back change is never lost and nothing wakes otherwise.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        if !self.pending {
            return None;
        }
        let since = self.last.map_or(self.min, |t| now.duration_since(t));
        Some(self.min.saturating_sub(since))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The main loop's shape: every wake drains changes, draws if due, else sleeps until due.
    fn storm(
        pacer: &mut Pacer,
        start: Instant,
        every: Duration,
        total: Duration,
    ) -> (u32, Instant) {
        let mut frames = 0;
        let mut now = start;
        let mut next_change = start;
        let end = start + total;
        let mut last_drawn = start;
        while now < end || pacer.pending() {
            if now >= next_change && now < end {
                pacer.changed();
                next_change += every;
            }
            if pacer.due(now) {
                pacer.drawn(now);
                frames += 1;
                last_drawn = now;
            }
            // Wake on the next change or when the held-back one is due, whichever is first.
            let wake_change = (next_change < end).then_some(next_change);
            let wake_due = pacer.wait(now).map(|d| now + d);
            now = match (wake_change, wake_due) {
                (Some(a), Some(b)) => a.min(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => break,
            };
        }
        (frames, last_drawn)
    }

    #[test]
    fn a_1khz_storm_draws_at_most_30_fps_and_the_last_change() {
        let mut p = Pacer::default();
        let t0 = Instant::now();
        let (frames, last) = storm(&mut p, t0, Duration::from_millis(1), Duration::from_secs(2));
        assert!((55..=61).contains(&frames), "{frames} frames in 2 s");
        // The final change (at ~2 s) was drawn, within one frame interval.
        assert!(
            last >= t0 + Duration::from_millis(1999 - 34),
            "{:?}",
            last - t0
        );
        assert!(!p.pending());
    }

    #[test]
    fn slow_changes_draw_at_once() {
        let mut p = Pacer::default();
        let t0 = Instant::now();
        let (frames, _) = storm(
            &mut p,
            t0,
            Duration::from_millis(500),
            Duration::from_secs(2),
        );
        assert_eq!(frames, 4);
        // Idle: nothing pending, no timeout.
        assert_eq!(p.wait(t0 + Duration::from_secs(9)), None);
    }

    #[test]
    fn other_frames_count() {
        let mut p = Pacer::default();
        let t0 = Instant::now();
        p.drawn(t0); // a touch redraw
        p.changed();
        assert!(!p.due(t0 + Duration::from_millis(10)));
        assert_eq!(
            p.wait(t0 + Duration::from_millis(10)),
            Some(Duration::from_secs(1) / 30 - Duration::from_millis(10))
        );
        assert!(p.due(t0 + Duration::from_millis(34)));
    }
}
