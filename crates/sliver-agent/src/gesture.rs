//! Gesture recognition for touches on agent regions (customization.md §3). Sans-IO: feed it
//! touch phases with sliverd's timestamps (µs, monotonic) and call [`Recognizer::tick`] at
//! [`Recognizer::next_deadline`].
//!
//! Only the gestures a module actually binds are recognized, so an unbound gesture never delays
//! another: no hold binding → no hold timer; no double-tap binding → taps fire at once.

use std::collections::HashMap;

use sliver_ipc::proto::TouchPhase;

pub const HOLD_US: u64 = 500_000;
pub const DOUBLE_TAP_US: u64 = 300_000;
/// Movement below this is still a tap or a hold.
pub const SLOP_PX: f32 = 12.0;
pub const SWIPE_PX: f32 = 40.0;

/// Which gestures a region's module binds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bindings {
    pub tap: bool,
    pub double_tap: bool,
    pub hold: bool,
    pub release: bool,
    pub swipe: bool,
    /// Sliders and `on-drag`: every position while the finger is down.
    pub drag: bool,
    /// Fire `tap` on touch-down (keys, layer holds).
    pub on_press: bool,
    /// Press-and-slide (collapsed sliders): once the finger moves past the slop, report
    /// [`Gesture::Slide`] positions until it lifts.
    pub slide: bool,
    pub swipe_threshold: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// Visual feedback: the region is (no longer) being touched.
    Pressed(bool),
    Tap,
    DoubleTap,
    Hold,
    /// The finger lifted (for `on-release`, and to end `hold`-mode layers).
    Release,
    SwipeLeft,
    SwipeRight,
    /// Finger position along the bar, in bar pixels.
    Drag(f32),
    /// Like `Drag`, but for a press-and-slide: the finger started elsewhere (on a collapsed
    /// slider's icon), so the position counts relative to where sliding began.
    Slide(f32),
}

#[derive(Debug, Clone)]
struct Touch {
    region: u32,
    b: Bindings,
    start_x: f32,
    hold_at: Option<u64>,
    moved: bool,
    /// Something already fired for this touch (press-tap, hold, swipe), so lifting isn't a tap.
    consumed: bool,
}

#[derive(Debug, Default)]
pub struct Recognizer {
    touches: HashMap<u8, Touch>,
    /// A tap waiting to see if a second one makes it a double tap: (region, fire-at).
    pending_tap: Option<(u32, u64)>,
}

impl Recognizer {
    pub fn new() -> Recognizer {
        Recognizer::default()
    }

    /// One touch phase on `region`. Returns recognized gestures, in order, for that region (and
    /// possibly a pending tap on another region that this touch resolved).
    pub fn touch(
        &mut self,
        region: u32,
        b: Bindings,
        slot: u8,
        phase: TouchPhase,
        x: f32,
        t: u64,
    ) -> Vec<(u32, Gesture)> {
        let mut out = Vec::new();
        match phase {
            TouchPhase::Down => {
                // A new touch elsewhere settles a waiting tap.
                if let Some((r, _)) = self.pending_tap
                    && r != region
                {
                    self.pending_tap = None;
                    out.push((r, Gesture::Tap));
                }
                out.push((region, Gesture::Pressed(true)));
                let mut touch = Touch {
                    region,
                    b,
                    start_x: x,
                    hold_at: b.hold.then_some(t + HOLD_US),
                    moved: false,
                    consumed: false,
                };
                if b.drag {
                    out.push((region, Gesture::Drag(x)));
                    touch.consumed = true;
                }
                if b.on_press && b.tap {
                    out.push((region, Gesture::Tap));
                    touch.consumed = true;
                }
                self.touches.insert(slot, touch);
            }
            TouchPhase::Move => {
                let Some(tc) = self.touches.get_mut(&slot) else {
                    return out;
                };
                let dx = x - tc.start_x;
                if tc.b.drag {
                    out.push((tc.region, Gesture::Drag(x)));
                } else if tc.b.slide && (tc.moved || dx.abs() > SLOP_PX) {
                    tc.moved = true;
                    tc.hold_at = None;
                    out.push((tc.region, Gesture::Slide(x)));
                } else if dx.abs() > SLOP_PX && !tc.moved {
                    tc.moved = true;
                    tc.hold_at = None;
                }
                let threshold = tc.b.swipe_threshold.unwrap_or(SWIPE_PX).max(SLOP_PX);
                if tc.b.swipe && !tc.consumed && dx.abs() >= threshold {
                    tc.consumed = true;
                    let g = if dx < 0.0 {
                        Gesture::SwipeLeft
                    } else {
                        Gesture::SwipeRight
                    };
                    out.push((tc.region, g));
                }
            }
            TouchPhase::Up | TouchPhase::Cancel => {
                let Some(tc) = self.touches.remove(&slot) else {
                    return out;
                };
                if phase == TouchPhase::Up && tc.b.drag {
                    // Where the finger lifted is the final value.
                    out.push((tc.region, Gesture::Drag(x)));
                }
                if phase == TouchPhase::Up && tc.b.slide && tc.moved {
                    out.push((tc.region, Gesture::Slide(x)));
                }
                out.push((tc.region, Gesture::Pressed(false)));
                if phase == TouchPhase::Cancel {
                    return out;
                }
                out.push((tc.region, Gesture::Release));
                if tc.consumed || tc.moved || !tc.b.tap && !tc.b.double_tap {
                    return out;
                }
                if tc.b.double_tap {
                    match self.pending_tap.take() {
                        Some((r, _)) if r == tc.region => out.push((r, Gesture::DoubleTap)),
                        Some((r, _)) => {
                            out.push((r, Gesture::Tap));
                            self.pending_tap = Some((tc.region, t + DOUBLE_TAP_US));
                        }
                        None => self.pending_tap = Some((tc.region, t + DOUBLE_TAP_US)),
                    }
                } else {
                    out.push((tc.region, Gesture::Tap));
                }
            }
        }
        out
    }

    /// Fires holds and waiting taps that are due at `now`.
    pub fn tick(&mut self, now: u64) -> Vec<(u32, Gesture)> {
        let mut out = Vec::new();
        for tc in self.touches.values_mut() {
            if tc.hold_at.is_some_and(|at| at <= now) {
                tc.hold_at = None;
                tc.consumed = true;
                out.push((tc.region, Gesture::Hold));
            }
        }
        if let Some((r, at)) = self.pending_tap
            && at <= now
        {
            self.pending_tap = None;
            out.push((r, Gesture::Tap));
        }
        out
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.touches
            .values()
            .filter_map(|t| t.hold_at)
            .chain(self.pending_tap.map(|p| p.1))
            .min()
    }

    /// Forget every touch (layer switch, display off, hatch). Returns un-press feedback.
    pub fn cancel_all(&mut self) -> Vec<(u32, Gesture)> {
        self.pending_tap = None;
        self.touches
            .drain()
            .map(|(_, t)| (t.region, Gesture::Pressed(false)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Gesture::*;
    use TouchPhase::*;

    const R: u32 = 7;

    fn b() -> Bindings {
        Bindings {
            tap: true,
            ..Bindings::default()
        }
    }

    fn only(v: Vec<(u32, Gesture)>) -> Vec<Gesture> {
        v.into_iter()
            .filter(|(_, g)| !matches!(g, Pressed(_) | Release))
            .map(|(_, g)| g)
            .collect()
    }

    #[test]
    fn tap_fires_on_lift_without_delay() {
        let mut r = Recognizer::new();
        assert_eq!(r.touch(R, b(), 0, Down, 100.0, 0), [(R, Pressed(true))]);
        assert_eq!(r.next_deadline(), None, "no hold binding, no timer");
        assert_eq!(
            r.touch(R, b(), 0, Up, 103.0, 80_000),
            [(R, Pressed(false)), (R, Release), (R, Tap)]
        );
    }

    #[test]
    fn hold_replaces_tap() {
        let mut r = Recognizer::new();
        let hb = Bindings { hold: true, ..b() };
        r.touch(R, hb, 0, Down, 100.0, 0);
        assert_eq!(r.next_deadline(), Some(HOLD_US));
        assert!(r.tick(HOLD_US - 1).is_empty());
        assert_eq!(r.tick(HOLD_US), [(R, Hold)]);
        assert_eq!(only(r.touch(R, hb, 0, Up, 100.0, HOLD_US + 10)), []);
        // A quick lift is still a tap.
        r.touch(R, hb, 0, Down, 100.0, 1_000_000);
        assert_eq!(only(r.touch(R, hb, 0, Up, 100.0, 1_100_000)), [Tap]);
    }

    #[test]
    fn swipes_and_slop() {
        let mut r = Recognizer::new();
        let sb = Bindings {
            swipe: true,
            hold: true,
            ..b()
        };
        r.touch(R, sb, 0, Down, 200.0, 0);
        assert_eq!(only(r.touch(R, sb, 0, Move, 190.0, 10)), [], "within slop");
        assert_eq!(only(r.touch(R, sb, 0, Move, 150.0, 20)), [SwipeLeft]);
        assert_eq!(r.next_deadline(), None, "moving cancels hold");
        assert_eq!(
            only(r.touch(R, sb, 0, Move, 100.0, 30)),
            [],
            "one swipe per touch"
        );
        assert_eq!(only(r.touch(R, sb, 0, Up, 100.0, 40)), [], "not a tap");
    }

    #[test]
    fn double_tap_waits_only_when_bound() {
        let mut r = Recognizer::new();
        let db = Bindings {
            double_tap: true,
            ..b()
        };
        r.touch(R, db, 0, Down, 0.0, 0);
        assert_eq!(only(r.touch(R, db, 0, Up, 0.0, 50_000)), []);
        r.touch(R, db, 0, Down, 0.0, 150_000);
        assert_eq!(only(r.touch(R, db, 0, Up, 0.0, 200_000)), [DoubleTap]);
        r.touch(R, db, 0, Down, 0.0, 1_000_000);
        r.touch(R, db, 0, Up, 0.0, 1_050_000);
        assert_eq!(r.tick(1_050_000 + DOUBLE_TAP_US), [(R, Tap)]);
    }

    #[test]
    fn press_and_slide() {
        let s = Bindings {
            tap: true,
            on_press: true,
            slide: true,
            ..Bindings::default()
        };
        let mut r = Recognizer::new();
        // Opens on press; small jitter isn't a slide.
        assert_eq!(only(r.touch(R, s, 1, Down, 10.0, 0)), [Tap]);
        assert_eq!(only(r.touch(R, s, 1, Move, 13.0, 5)), []);
        assert_eq!(only(r.touch(R, s, 1, Move, 30.0, 9)), [Slide(30.0)]);
        assert_eq!(only(r.touch(R, s, 1, Move, 25.0, 12)), [Slide(25.0)]);
        assert_eq!(only(r.touch(R, s, 1, Up, 26.0, 15)), [Slide(26.0)]);
        // A plain tap stays a tap (opens) with no slide.
        assert_eq!(only(r.touch(R, s, 1, Down, 10.0, 100)), [Tap]);
        assert_eq!(only(r.touch(R, s, 1, Up, 11.0, 110)), []);
    }

    #[test]
    fn drag_and_press_taps() {
        let mut r = Recognizer::new();
        let d = Bindings {
            drag: true,
            ..Bindings::default()
        };
        assert_eq!(only(r.touch(R, d, 1, Down, 10.0, 0)), [Drag(10.0)]);
        assert_eq!(only(r.touch(R, d, 1, Move, 60.0, 5)), [Drag(60.0)]);
        assert_eq!(only(r.touch(R, d, 1, Up, 65.0, 9)), [Drag(65.0)]);
        let p = Bindings {
            on_press: true,
            ..b()
        };
        assert_eq!(only(r.touch(R, p, 0, Down, 0.0, 0)), [Tap]);
        assert_eq!(only(r.touch(R, p, 0, Up, 0.0, 9)), []);
        assert_eq!(r.touch(R, p, 0, Cancel, 0.0, 9), [], "already lifted");
    }
}
