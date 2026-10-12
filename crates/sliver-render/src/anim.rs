//! Transitions: when a module's computed style changes, ease its paint properties (`color`,
//! `background-color`, `border-color`, `opacity`) from where they were to where they are going,
//! as its `transition` asks.
//!
//! The renderer stays a function of (scene, sheet, time): [`Animator`] only remembers, per item,
//! what each property currently shows. Frames are needed only while something is moving:
//! [`Animator::deadline`] says when, and is `None` once everything has settled, so an idle bar
//! costs nothing (invariant 3). Frames come at most every [`FRAME`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Color;
use crate::geometry::Rect;
use crate::style::{Style, Timing, TransitionProp};

/// Minimum time between animation frames (~30 fps): plenty for fades on a 60 px bar, and cheap.
pub const FRAME: Duration = Duration::from_millis(33);

trait Lerp: Copy + PartialEq {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

impl Lerp for Color {
    /// Straight-alpha sRGB interpolation. A fade from transparent keeps the target's hue
    /// instead of passing through black.
    fn lerp(a: Color, b: Color, t: f32) -> Color {
        let (a, b) = match (a.a, b.a) {
            (0, _) => (Color { a: 0, ..b }, b),
            (_, 0) => (a, Color { a: 0, ..a }),
            _ => (a, b),
        };
        let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
        Color {
            r: mix(a.r, b.r),
            g: mix(a.g, b.g),
            b: mix(a.b, b.b),
            a: mix(a.a, b.a),
        }
    }
}

#[derive(Clone, Copy)]
struct Track<T: Lerp> {
    from: T,
    to: T,
    start: Instant,
    duration: Duration,
    delay: Duration,
    timing: Timing,
}

impl<T: Lerp> Track<T> {
    fn settled(v: T, now: Instant) -> Track<T> {
        Track {
            from: v,
            to: v,
            start: now,
            duration: Duration::ZERO,
            delay: Duration::ZERO,
            timing: Timing::Linear,
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let begin = self.start + self.delay;
        if now <= begin {
            return 0.0;
        }
        ((now - begin).as_secs_f32() / self.duration.as_secs_f32()).min(1.0)
    }

    fn value(&self, now: Instant) -> T {
        let p = self.progress(now);
        if p >= 1.0 {
            self.to
        } else {
            T::lerp(self.from, self.to, self.timing.at(p))
        }
    }

    fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0 && now >= self.start + self.delay || self.duration.is_zero()
    }

    /// The element's style now asks for `to`. Start easing from whatever is showing, using
    /// the transition spec for this property (`None`: no transition, jump).
    fn retarget(&mut self, to: T, spec: Option<(Duration, Duration, Timing)>, now: Instant) {
        if to == self.to {
            return;
        }
        let shown = self.value(now);
        match spec {
            Some((duration, delay, timing)) if !(duration + delay).is_zero() => {
                *self = Track {
                    from: shown,
                    to,
                    start: now,
                    duration,
                    delay,
                    timing,
                };
            }
            _ => *self = Track::settled(to, now),
        }
    }
}

struct ItemAnim {
    color: Track<Color>,
    background: Track<Color>,
    border: Track<Color>,
    opacity: Track<f32>,
    seen: bool,
}

struct GeoAnim {
    x: Track<f32>,
    w: Track<f32>,
    seen: bool,
}

/// Per-item transition state for one [`crate::Renderer`].
#[derive(Default)]
pub(crate) struct Animator {
    items: HashMap<String, ItemAnim>,
    /// Boxes (zones and items) with a `width` transition.
    geo: HashMap<String, GeoAnim>,
    /// The desktop asked for reduced motion: every change jumps.
    pub reduce_motion: bool,
    /// Next frame due, per scene (the agent renders several layers with one renderer).
    next: HashMap<String, Instant>,
}

/// Item keys are `<scene>|<zone>/<item>`.
fn in_scene(key: &str, scene: &str) -> bool {
    key.strip_prefix(scene).is_some_and(|r| r.starts_with('|'))
}

/// The last matching entry wins, as in CSS; `all` matches every paint property.
fn spec(style: &Style, prop: TransitionProp) -> Option<(Duration, Duration, Timing)> {
    style
        .transitions
        .iter()
        .rev()
        .find(|t| {
            t.prop == prop || t.prop == TransitionProp::All && prop != TransitionProp::Geometry
        })
        .map(|t| (t.duration, t.delay, t.timing))
}

impl Animator {
    /// Start drawing `scene` (an id such as its classes): its items are "unseen" until applied.
    pub fn begin_frame(&mut self, scene: &str) {
        for (k, a) in self.items.iter_mut() {
            if in_scene(k, scene) {
                a.seen = false;
            }
        }
        for (k, a) in self.geo.iter_mut() {
            if in_scene(k, scene) {
                a.seen = false;
            }
        }
        self.next.remove(scene);
    }

    /// The style to draw for `key` right now, given its computed (target) `style`.
    pub fn apply(
        &mut self,
        scene: &str,
        key: &str,
        style: &Arc<Style>,
        now: Instant,
    ) -> Arc<Style> {
        let target = (
            style.color,
            style.background,
            style.border_color(),
            style.opacity,
        );
        let a = self
            .items
            .entry(key.to_string())
            .or_insert_with(|| ItemAnim {
                color: Track::settled(target.0, now),
                background: Track::settled(target.1, now),
                border: Track::settled(target.2, now),
                opacity: Track::settled(target.3, now),
                seen: true,
            });
        a.seen = true;
        let reduce = self.reduce_motion;
        let spec = |p| spec(style, p).filter(|_| !reduce);
        a.color.retarget(target.0, spec(TransitionProp::Color), now);
        a.background
            .retarget(target.1, spec(TransitionProp::Background), now);
        a.border
            .retarget(target.2, spec(TransitionProp::BorderColor), now);
        a.opacity
            .retarget(target.3, spec(TransitionProp::Opacity), now);
        let moving = !(a.color.done(now)
            && a.background.done(now)
            && a.border.done(now)
            && a.opacity.done(now));
        if !moving {
            return style.clone();
        }
        let due = self.next.entry(scene.to_string()).or_insert(now + FRAME);
        *due = (*due).min(now + FRAME);
        let mut s = (**style).clone();
        s.color = a.color.value(now);
        s.background = a.background.value(now);
        s.border_color = Some(a.border.value(now));
        s.opacity = a.opacity.value(now);
        Arc::new(s)
    }

    /// Where to draw the box `key` (laid out at `target`) right now, given its `style`'s
    /// `width` transition. Only x and width move: the bar is one row.
    pub fn geometry(
        &mut self,
        scene: &str,
        key: &str,
        target: Rect,
        style: &Style,
        now: Instant,
    ) -> Rect {
        let spec = spec(style, TransitionProp::Geometry).filter(|_| !self.reduce_motion);
        if spec.is_none() && !self.geo.contains_key(key) {
            return target; // the common case: nothing to remember
        }
        let a = self.geo.entry(key.to_string()).or_insert_with(|| GeoAnim {
            x: Track::settled(target.x, now),
            w: Track::settled(target.w, now),
            seen: true,
        });
        a.seen = true;
        a.x.retarget(target.x, spec, now);
        a.w.retarget(target.w, spec, now);
        if a.x.done(now) && a.w.done(now) {
            return target;
        }
        let due = self.next.entry(scene.to_string()).or_insert(now + FRAME);
        *due = (*due).min(now + FRAME);
        Rect {
            x: a.x.value(now),
            w: a.w.value(now),
            ..target
        }
    }

    /// Forget the items of `scene` that were not drawn this frame.
    pub fn end_frame(&mut self, scene: &str) {
        self.items.retain(|k, a| a.seen || !in_scene(k, scene));
        self.geo.retain(|k, a| a.seen || !in_scene(k, scene));
    }

    /// Drop all state for scenes other than `keep` (scene ids): they're no longer drawn, so a
    /// fade still pending on one must not keep asking for frames.
    pub fn retain_scenes(&mut self, keep: &[String]) {
        let kept = |k: &str| keep.iter().any(|s| in_scene(k, s));
        self.items.retain(|k, _| kept(k));
        self.geo.retain(|k, _| kept(k));
        self.next.retain(|s, _| keep.contains(s));
    }

    /// When the next animation frame is due; `None` when everything is at rest.
    pub fn deadline(&self) -> Option<Instant> {
        self.next.values().min().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Transition;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn style(bg: Color, trans: &[Transition]) -> Arc<Style> {
        let mut s = Style::initial();
        s.background = bg;
        s.transitions = trans.to_vec();
        Arc::new(s)
    }

    fn fade(prop: TransitionProp, d: u64) -> Transition {
        Transition {
            prop,
            duration: ms(d),
            delay: Duration::ZERO,
            timing: Timing::Linear,
        }
    }

    const RED: Color = Color::rgb(255, 0, 0);
    const BLUE: Color = Color::rgb(0, 0, 255);

    #[test]
    fn first_frame_does_not_animate_and_change_eases() {
        let t0 = Instant::now();
        let mut an = Animator::default();
        let tr = [fade(TransitionProp::Background, 200)];
        an.begin_frame("s");
        assert_eq!(an.apply("s", "s|a", &style(RED, &tr), t0).background, RED);
        assert!(an.deadline().is_none(), "nothing moving");
        an.end_frame("s");

        an.begin_frame("s");
        let mid = an.apply("s", "s|a", &style(BLUE, &tr), t0 + ms(1000));
        assert_eq!(mid.background, RED, "starts where it was");
        assert!(an.deadline().is_some());
        an.end_frame("s");
        an.begin_frame("s");
        let mid = an.apply("s", "s|a", &style(BLUE, &tr), t0 + ms(1100));
        assert_eq!(
            mid.background,
            Color::rgb(128, 0, 128),
            "halfway after 100 of 200 ms"
        );
        an.end_frame("s");
        an.begin_frame("s");
        let end = an.apply("s", "s|a", &style(BLUE, &tr), t0 + ms(1300));
        assert_eq!(end.background, BLUE);
        assert!(an.deadline().is_none(), "settled: no more frames needed");
    }

    #[test]
    fn no_transition_jumps_and_retarget_midflight_starts_from_current() {
        let t0 = Instant::now();
        let mut an = Animator::default();
        an.begin_frame("s");
        an.apply("s", "s|a", &style(RED, &[]), t0);
        an.end_frame("s");
        an.begin_frame("s");
        assert_eq!(
            an.apply("s", "s|a", &style(BLUE, &[]), t0 + ms(10))
                .background,
            BLUE,
            "no transition: instant"
        );
        an.end_frame("s");

        let tr = [fade(TransitionProp::All, 100)];
        an.begin_frame("s");
        an.apply("s", "s|b", &style(RED, &tr), t0);
        an.end_frame("s");
        an.begin_frame("s");
        an.apply("s", "s|b", &style(BLUE, &tr), t0 + ms(100));
        an.end_frame("s");
        an.begin_frame("s");
        let half = an
            .apply("s", "s|b", &style(BLUE, &tr), t0 + ms(150))
            .background;
        assert_eq!(half, Color::rgb(128, 0, 128));
        an.end_frame("s");
        // reverse at the halfway point: it eases back from purple, not from blue
        an.begin_frame("s");
        let turn = an
            .apply("s", "s|b", &style(RED, &tr), t0 + ms(150))
            .background;
        assert_eq!(turn, Color::rgb(128, 0, 128));
        an.end_frame("s");
    }

    #[test]
    fn unseen_items_are_forgotten_and_opacity_animates() {
        let t0 = Instant::now();
        let mut an = Animator::default();
        let tr = [fade(TransitionProp::Opacity, 100)];
        let mut s = Style::initial();
        s.transitions = tr.to_vec();
        let on = Arc::new(s.clone());
        s.opacity = 0.0;
        let off = Arc::new(s);
        an.begin_frame("s");
        an.apply("s", "s|x", &on, t0);
        an.end_frame("s");
        an.begin_frame("s");
        an.apply("s", "s|x", &off, t0 + ms(10));
        an.end_frame("s");
        an.begin_frame("s");
        let o = an.apply("s", "s|x", &off, t0 + ms(60)).opacity;
        assert!((o - 0.5).abs() < 0.02, "{o}");
        an.end_frame("s");
        an.begin_frame("s"); // x not drawn this frame
        an.end_frame("s");
        an.begin_frame("s");
        assert_eq!(
            an.apply("s", "s|x", &off, t0 + ms(70)).opacity,
            0.0,
            "came back as a new item: no animation"
        );
    }

    #[test]
    fn delay_holds_the_start() {
        let t0 = Instant::now();
        let mut an = Animator::default();
        let tr = [Transition {
            prop: TransitionProp::Background,
            duration: ms(100),
            delay: ms(100),
            timing: Timing::Linear,
        }];
        an.begin_frame("s");
        an.apply("s", "s|a", &style(RED, &tr), t0);
        an.end_frame("s");
        an.begin_frame("s");
        an.apply("s", "s|a", &style(BLUE, &tr), t0 + ms(10));
        an.end_frame("s");
        an.begin_frame("s");
        assert_eq!(
            an.apply("s", "s|a", &style(BLUE, &tr), t0 + ms(80))
                .background,
            RED,
            "still waiting"
        );
        assert!(an.deadline().is_some());
    }

    #[test]
    fn fading_in_from_transparent_keeps_the_hue() {
        let mid = Color::lerp(Color::TRANSPARENT, BLUE, 0.5);
        assert_eq!((mid.r, mid.g, mid.b), (0, 0, 255));
        assert_eq!(mid.a, 128);
    }
}
