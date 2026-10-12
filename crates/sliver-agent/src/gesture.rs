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
/// Bar gestures: both fingers of a two-finger swipe must travel this far the same way.
pub const TWO_FINGER_PX: f32 = 60.0;
/// An edge swipe starts this close to a bar end…
pub const EDGE_PX: f32 = 48.0;
/// …and travels this far inward.
pub const EDGE_SWIPE_PX: f32 = 80.0;

/// Bar-wide gestures (`[gestures]` in the config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarGesture {
    TwoFingerSwipeLeft,
    TwoFingerSwipeRight,
    EdgeSwipeFromLeft,
    EdgeSwipeFromRight,
}

impl BarGesture {
    /// The config key (`[gestures]`).
    pub fn name(self) -> &'static str {
        match self {
            BarGesture::TwoFingerSwipeLeft => "two-finger-swipe-left",
            BarGesture::TwoFingerSwipeRight => "two-finger-swipe-right",
            BarGesture::EdgeSwipeFromLeft => "edge-swipe-from-left",
            BarGesture::EdgeSwipeFromRight => "edge-swipe-from-right",
        }
    }

    pub const ALL: [BarGesture; 4] = [
        BarGesture::TwoFingerSwipeLeft,
        BarGesture::TwoFingerSwipeRight,
        BarGesture::EdgeSwipeFromLeft,
        BarGesture::EdgeSwipeFromRight,
    ];
}

/// Which bar gestures are bound, and the bar's width (for the edges). Unbound ones cost
/// nothing: single-finger gestures behave exactly as without them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BarBindings {
    pub two_finger: bool,
    pub edge_left: bool,
    pub edge_right: bool,
    pub width: f32,
}

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
    /// A bar-wide gesture (reported with region 0: it belongs to no module).
    Bar(BarGesture),
    /// Like `Drag`, but for a press-and-slide: the finger started elsewhere (on a collapsed
    /// slider's icon), so the position counts relative to where sliding began.
    Slide(f32),
}

#[derive(Debug, Clone)]
struct Touch {
    region: u32,
    b: Bindings,
    start_x: f32,
    /// Where the finger is now (for two-finger swipes, which look at both).
    last_x: f32,
    hold_at: Option<u64>,
    moved: bool,
    /// Something already fired for this touch (press-tap, hold, swipe), so lifting isn't a tap.
    consumed: bool,
    /// Part of a two-finger gesture: no single-finger gestures (taps, drags…) any more.
    grouped: bool,
    /// Started at a bar end with an edge swipe bound for it: the inward direction (±1).
    edge: Option<f32>,
}

#[derive(Debug, Default)]
pub struct Recognizer {
    touches: HashMap<u8, Touch>,
    bar: BarBindings,
    /// The current two-finger contact already fired its gesture.
    two_fired: bool,
    /// A tap waiting to see if a second one makes it a double tap: (region, fire-at).
    pending_tap: Option<(u32, u64)>,
}

impl Recognizer {
    pub fn new() -> Recognizer {
        Recognizer::default()
    }

    /// Which bar gestures to recognize (from the config, on every render).
    pub fn set_bar(&mut self, bar: BarBindings) {
        self.bar = bar;
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
                // A second finger: both touches become a possible two-finger swipe and stop
                // doing single-finger things.
                let grouped = self.bar.two_finger && !self.touches.is_empty();
                if grouped {
                    for tc in self.touches.values_mut() {
                        tc.grouped = true;
                        tc.consumed = true;
                        tc.hold_at = None;
                    }
                } else if self.touches.is_empty() {
                    self.two_fired = false;
                }
                // Sliders own horizontal motion, so they never start an edge swipe.
                let edge = if b.drag || b.slide {
                    None
                } else if self.bar.edge_left && x <= EDGE_PX {
                    Some(1.0)
                } else if self.bar.edge_right && x >= self.bar.width - EDGE_PX {
                    Some(-1.0)
                } else {
                    None
                };
                let mut touch = Touch {
                    region,
                    b,
                    start_x: x,
                    last_x: x,
                    hold_at: (b.hold && !grouped).then_some(t + HOLD_US),
                    moved: false,
                    consumed: grouped,
                    grouped,
                    edge,
                };
                if grouped {
                    self.touches.insert(slot, touch);
                    return out;
                }
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
                tc.last_x = x;
                if tc.grouped {
                    tc.moved = tc.moved || dx.abs() > SLOP_PX;
                    self.two_finger(&mut out);
                    return out;
                }
                if let Some(dir) = tc.edge {
                    if !tc.consumed && dx * dir >= EDGE_SWIPE_PX {
                        tc.consumed = true;
                        tc.moved = true;
                        tc.hold_at = None;
                        out.push((
                            0,
                            Gesture::Bar(if dir > 0.0 {
                                BarGesture::EdgeSwipeFromLeft
                            } else {
                                BarGesture::EdgeSwipeFromRight
                            }),
                        ));
                    } else if dx.abs() > SLOP_PX && !tc.moved {
                        tc.moved = true;
                        tc.hold_at = None;
                    }
                    // The module's own swipes yield to the bar's edge swipe.
                    return out;
                }
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
                if phase == TouchPhase::Up && tc.b.drag && !tc.grouped {
                    // Where the finger lifted is the final value.
                    out.push((tc.region, Gesture::Drag(x)));
                }
                if phase == TouchPhase::Up && tc.b.slide && tc.moved && !tc.grouped {
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

    /// A grouped finger moved to `x`: fire the two-finger swipe once both fingers have
    /// travelled far enough the same way.
    fn two_finger(&mut self, out: &mut Vec<(u32, Gesture)>) {
        if self.two_fired {
            return;
        }
        let current: Vec<f32> = self
            .touches
            .values()
            .filter(|t| t.grouped)
            .map(|t| t.last_x - t.start_x)
            .collect();
        if current.len() < 2 {
            return;
        }
        let all = |f: &dyn Fn(f32) -> bool| current.iter().all(|d| f(*d));
        let g = if all(&|d| d <= -TWO_FINGER_PX) {
            BarGesture::TwoFingerSwipeLeft
        } else if all(&|d| d >= TWO_FINGER_PX) {
            BarGesture::TwoFingerSwipeRight
        } else {
            return;
        };
        self.two_fired = true;
        out.push((0, Gesture::Bar(g)));
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

    fn bar(two: bool, left: bool, right: bool) -> BarBindings {
        BarBindings {
            two_finger: two,
            edge_left: left,
            edge_right: right,
            width: 2008.0,
        }
    }

    /// Everything, bar gestures included (`only` drops feedback).
    fn bars(v: Vec<(u32, Gesture)>) -> Vec<BarGesture> {
        v.into_iter()
            .filter_map(|(r, g)| match g {
                Bar(b) => {
                    assert_eq!(r, 0, "bar gestures belong to no region");
                    Some(b)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn two_finger_swipes() {
        let mut r = Recognizer::new();
        r.set_bar(bar(true, false, false));
        let sb = Bindings { swipe: true, ..b() };
        assert_eq!(only(r.touch(R, sb, 0, Down, 100.0, 0)), []);
        // The second finger groups both: no taps, swipes or drags for either from now on.
        assert_eq!(only(r.touch(8, sb, 1, Down, 300.0, 10)), []);
        assert!(
            bars(r.touch(R, sb, 0, Move, 170.0, 20)).is_empty(),
            "one finger isn't enough"
        );
        assert_eq!(
            bars(r.touch(8, sb, 1, Move, 365.0, 30)),
            [BarGesture::TwoFingerSwipeRight]
        );
        assert!(
            only(r.touch(R, sb, 0, Move, 260.0, 40)).is_empty(),
            "fires once"
        );
        assert_eq!(only(r.touch(R, sb, 0, Up, 260.0, 50)), []);
        assert_eq!(only(r.touch(8, sb, 1, Up, 400.0, 60)), []);
        // Opposite directions: a pinch, not a swipe.
        r.touch(R, b(), 0, Down, 100.0, 1000);
        r.touch(8, b(), 1, Down, 300.0, 1010);
        assert!(bars(r.touch(R, b(), 0, Move, 20.0, 1020)).is_empty());
        assert!(bars(r.touch(8, b(), 1, Move, 400.0, 1030)).is_empty());
        r.touch(R, b(), 0, Up, 20.0, 1040);
        r.touch(8, b(), 1, Up, 400.0, 1050);
        // Next contact can fire again; leftwards.
        r.touch(R, b(), 0, Down, 500.0, 2000);
        r.touch(8, b(), 1, Down, 700.0, 2010);
        r.touch(R, b(), 0, Move, 420.0, 2020);
        assert_eq!(
            bars(r.touch(8, b(), 1, Move, 610.0, 2030)),
            [BarGesture::TwoFingerSwipeLeft]
        );
    }

    #[test]
    fn without_bar_gestures_two_fingers_are_two_taps() {
        let mut r = Recognizer::new();
        r.touch(R, b(), 0, Down, 100.0, 0);
        r.touch(8, b(), 1, Down, 300.0, 10);
        assert_eq!(only(r.touch(R, b(), 0, Up, 100.0, 20)), [Tap]);
        assert_eq!(only(r.touch(8, b(), 1, Up, 300.0, 30)), [Tap]);
    }

    #[test]
    fn edge_swipes() {
        let mut r = Recognizer::new();
        r.set_bar(bar(false, true, true));
        let sb = Bindings { swipe: true, ..b() };
        // From the left end inward; the module's own swipe yields.
        r.touch(R, sb, 0, Down, 20.0, 0);
        assert!(
            only(r.touch(R, sb, 0, Move, 70.0, 10)).is_empty(),
            "not far enough"
        );
        assert_eq!(
            bars(r.touch(R, sb, 0, Move, 110.0, 20)),
            [BarGesture::EdgeSwipeFromLeft]
        );
        assert_eq!(only(r.touch(R, sb, 0, Up, 110.0, 30)), [], "and no tap");
        // From the right end.
        r.touch(R, b(), 0, Down, 1995.0, 100);
        assert_eq!(
            bars(r.touch(R, b(), 0, Move, 1900.0, 110)),
            [BarGesture::EdgeSwipeFromRight]
        );
        r.touch(R, b(), 0, Up, 1900.0, 120);
        // Outward or mid-bar: nothing; a still touch at the edge is still a tap.
        r.touch(R, sb, 0, Down, 600.0, 200);
        assert!(bars(r.touch(R, sb, 0, Move, 720.0, 210)).is_empty());
        r.touch(R, sb, 0, Up, 720.0, 220);
        r.touch(R, b(), 0, Down, 10.0, 300);
        assert_eq!(only(r.touch(R, b(), 0, Up, 12.0, 310)), [Tap]);
        // Sliders keep their drag.
        let d = Bindings {
            drag: true,
            ..Bindings::default()
        };
        r.touch(R, d, 0, Down, 10.0, 400);
        assert_eq!(only(r.touch(R, d, 0, Move, 150.0, 410)), [Drag(150.0)]);
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
