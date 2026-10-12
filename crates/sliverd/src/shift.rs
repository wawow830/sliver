//! Pixel shift against OLED burn-in (T79, tiny-dfr `EnablePixelShift`).
//!
//! Same path as tiny-dfr's `pixel_shift.rs`: x walks back and forth across ±11 px, y follows a
//! triangle wave of ±2 px tied to x (with a random phase), and it rests longer at each end. tiny-dfr
//! slides each pixel over 4 s with a redraw every 200 ms. We take whole-pixel steps instead (one
//! flush each), which costs almost nothing (invariant 3) and is still too slow to notice.
//!
//! Pure: the daemon asks [`PixelShift::deadline`] for its timer and calls [`PixelShift::tick`].
//! Nothing moves while the display is off, so a dark bar sleeps.

/// Half the x range: the picture moves up to this far left or right. Content is laid out this far
/// in from both ends while shifting (agent and built-in layers).
pub const RANGE_X: i32 = 11;
/// Half the y range. More than 2 px makes icons visibly off-centre (tiny-dfr's comment).
pub const RANGE_Y: i32 = 2;
/// Time between 1 px steps.
pub const STEP_MS: u64 = 15_000;
/// Rest at each end of the x range.
pub const DWELL_MS: u64 = 60_000;

const SPAN_X: u32 = (RANGE_X * 2) as u32;
const SPAN_Y: u32 = (RANGE_Y * 2) as u32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelShift {
    enabled: bool,
    /// 0..=SPAN_X, x offset + RANGE_X.
    progress: u32,
    direction: i32,
    /// Phase of y against x, 0..2*SPAN_Y, so the 2-D path differs between runs.
    y_phase: u32,
    next_at: Option<u64>,
}

impl PixelShift {
    /// Off. `seed` picks the starting point and the y phase (any number, e.g. from the clock).
    pub fn new(seed: u64) -> PixelShift {
        PixelShift {
            enabled: false,
            progress: (seed % u64::from(SPAN_X + 1)) as u32,
            direction: 1,
            y_phase: ((seed / u64::from(SPAN_X + 1)) % u64::from(SPAN_Y * 2)) as u32,
            next_at: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turns it on or off. Returns whether that changed anything.
    pub fn set_enabled(&mut self, on: bool, now: u64) -> bool {
        if on == self.enabled {
            return false;
        }
        self.enabled = on;
        self.next_at = on.then_some(now + STEP_MS);
        true
    }

    /// `(dx, dy)` to move the picture by. `(0, 0)` when off.
    pub fn offset(&self) -> (i32, i32) {
        if !self.enabled {
            return (0, 0);
        }
        let x = self.progress as i32 - RANGE_X;
        let span = SPAN_Y as i32;
        let mut y = (self.progress as i32 + self.y_phase as i32) % (span * 2);
        if y > span {
            y = span * 2 - y;
        }
        (x, y - RANGE_Y)
    }

    /// When the next step is due, if it can happen: only while on and the display shows something.
    pub fn deadline(&self, display_on: bool) -> Option<u64> {
        if self.enabled && display_on {
            self.next_at
        } else {
            None
        }
    }

    /// Takes the next step if it's due. Returns whether the offset changed. An overdue step (the
    /// display was off) happens once, then the timing starts over from `now`.
    pub fn tick(&mut self, now: u64, display_on: bool) -> bool {
        match self.deadline(display_on) {
            Some(at) if now >= at => {}
            _ => return false,
        }
        let next = self.progress as i32 + self.direction;
        self.progress = next.clamp(0, SPAN_X as i32) as u32;
        let at_end = self.progress == 0 || self.progress == SPAN_X;
        if at_end {
            self.direction = -self.direction;
        }
        self.next_at = Some(now + if at_end { DWELL_MS } else { STEP_MS });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_and_never_schedules() {
        let mut s = PixelShift::new(7);
        assert_eq!(s.offset(), (0, 0));
        assert_eq!(s.deadline(true), None);
        assert!(!s.tick(u64::MAX, true));
    }

    #[test]
    fn walks_the_range_and_rests_at_the_ends() {
        let mut s = PixelShift::new(0);
        assert!(s.set_enabled(true, 0));
        assert!(!s.set_enabled(true, 0), "no change");
        assert_eq!(s.offset().0, -RANGE_X);
        let (mut now, mut xs, mut gaps) = (0, vec![], vec![]);
        for _ in 0..(SPAN_X * 2 + 2) {
            let at = s.deadline(true).unwrap();
            gaps.push(at - now);
            now = at;
            assert!(!s.tick(now - 1, true), "not early");
            assert!(s.tick(now, true));
            xs.push(s.offset().0);
        }
        // -10 .. 11, rest, 10 .. -11, rest, back up.
        assert_eq!(xs[..SPAN_X as usize], (-10..=11).collect::<Vec<_>>()[..]);
        assert_eq!(xs[SPAN_X as usize], 10);
        assert_eq!(*xs.iter().min().unwrap(), -RANGE_X);
        assert_eq!(*xs.iter().max().unwrap(), RANGE_X);
        assert!(
            xs.windows(2).all(|w| (w[0] - w[1]).abs() == 1),
            "1 px steps"
        );
        assert_eq!(gaps[0], STEP_MS);
        assert_eq!(
            gaps[SPAN_X as usize], DWELL_MS,
            "rests after reaching the end"
        );
    }

    #[test]
    fn y_stays_in_range_and_moves_with_x() {
        for seed in 0..200 {
            let mut s = PixelShift::new(seed);
            s.set_enabled(true, 0);
            let mut ys = std::collections::BTreeSet::new();
            for _ in 0..60 {
                let (x, y) = s.offset();
                assert!(
                    x.abs() <= RANGE_X && y.abs() <= RANGE_Y,
                    "{seed}: ({x}, {y})"
                );
                ys.insert(y);
                let at = s.deadline(true).unwrap();
                s.tick(at, true);
            }
            assert_eq!(ys.len(), SPAN_Y as usize + 1, "{seed}: covers every y");
        }
    }

    #[test]
    fn sleeps_while_the_display_is_off() {
        let mut s = PixelShift::new(3);
        s.set_enabled(true, 0);
        let before = s.offset();
        assert_eq!(s.deadline(false), None);
        assert!(!s.tick(10 * STEP_MS, false));
        assert_eq!(s.offset(), before);
        // Back on, long overdue: one step, then the timing restarts.
        assert!(s.tick(10 * STEP_MS, true));
        assert!(s.deadline(true).unwrap() >= 10 * STEP_MS + STEP_MS);
    }

    #[test]
    fn turning_off_recentres() {
        let mut s = PixelShift::new(5);
        s.set_enabled(true, 0);
        assert_ne!(s.offset(), (0, 0));
        assert!(s.set_enabled(false, 1));
        assert_eq!(s.offset(), (0, 0));
        assert_eq!(s.deadline(true), None);
    }
}
