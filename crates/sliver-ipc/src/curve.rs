//! Adaptive brightness curve: how the bar follows the main display (`display_policy.curve`).
//!
//! Shared so sliverd (which applies it) and the settings app (which plots it) use the same math.
//! On the wire and here, points are `[screen, bar]` in per-mille (0–1000): screen = the main
//! display's brightness, bar = share of the active brightness. Config uses percent (×10).

/// Most points a curve keeps.
pub const MAX_POINTS: usize = 16;

/// Untrusted points → a usable curve: values clamped to 1000, sorted by screen, duplicate screens
/// dropped (first wins), at most [`MAX_POINTS`]. Fewer than 2 points = empty (tiny-dfr's sqrt).
pub fn normalise(points: &[[u16; 2]]) -> Vec<[u16; 2]> {
    let mut c: Vec<[u16; 2]> = points
        .iter()
        .take(64) // bound the work on hostile input (the packet size limit bounds it anyway)
        .map(|&[s, b]| [s.min(1000), b.min(1000)])
        .collect();
    c.sort_by_key(|p| p[0]); // stable: the first of equal screens stays first
    c.dedup_by_key(|p| p[0]);
    c.truncate(MAX_POINTS);
    if c.len() < 2 {
        c.clear();
    }
    c
}

/// Bar share (0–1) for main-display brightness `screen` (0–1) on a [`normalise`]d curve:
/// piecewise-linear, flat beyond the ends. An empty curve is tiny-dfr's `sqrt(screen)`. NaN = 1.
pub fn bar_fraction(curve: &[[u16; 2]], screen: f32) -> f32 {
    let f = if screen.is_nan() {
        1.0
    } else {
        screen.clamp(0.0, 1.0)
    };
    let (Some(first), Some(last)) = (curve.first(), curve.last()) else {
        return f.sqrt();
    };
    let x = f * 1000.0;
    let y = if x <= first[0] as f32 {
        first[1] as f32
    } else if x >= last[0] as f32 {
        last[1] as f32
    } else {
        let i = curve.partition_point(|p| (p[0] as f32) <= x);
        let ([x0, y0], [x1, y1]) = (curve[i - 1], curve[i]);
        let t = (x - x0 as f32) / (x1 - x0) as f32;
        y0 as f32 + t * (y1 as f32 - y0 as f32)
    };
    y / 1000.0
}

/// The backlight value sliverd writes while on and following the screen: `active` scaled by
/// [`bar_fraction`] (plus tiny-dfr's `+ 1` without a curve), never below 1 and never above `max`.
pub fn level(curve: &[[u16; 2]], screen: f32, active: u32, max: u32) -> u32 {
    if max == 0 {
        return 0;
    }
    let offset = if curve.is_empty() { 1.0 } else { 0.0 };
    let v = bar_fraction(curve, screen) * active as f32 + offset;
    (v.round() as u32).clamp(1, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piecewise_linear_and_flat_beyond_the_ends() {
        let c = normalise(&[[200, 100], [600, 500], [1000, 1000]]);
        let at = |s| level(&c, s, 200, 255);
        assert_eq!(at(0.0), 20); // flat below the first point
        assert_eq!(at(0.2), 20);
        assert_eq!(at(0.4), 60); // halfway 100..500
        assert_eq!(at(0.6), 100);
        assert_eq!(at(0.8), 150);
        assert_eq!(at(1.0), 200);
        assert_eq!(at(f32::NAN), 200);
        assert!((bar_fraction(&c, 0.4) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn empty_is_tiny_dfr_sqrt() {
        assert_eq!(level(&[], 0.25, 200, 255), 101); // sqrt(0.25) * 200 + 1
        assert_eq!(level(&[], 1.0, 255, 255), 255, "clamped to max");
        assert_eq!(level(&[], 0.5, 0, 0), 0, "no backlight range");
    }

    #[test]
    fn never_dark_while_on() {
        let c = normalise(&[[0, 0], [1000, 0]]);
        assert_eq!(level(&c, 0.5, 200, 255), 1);
    }

    #[test]
    fn hostile_points_are_normalised() {
        assert_eq!(
            normalise(&[[1000, 9999], [0, 0], [0, 1000], [5000, 0]]),
            vec![[0, 0], [1000, 1000]]
        );
        assert!(normalise(&[[500, 500]]).is_empty());
        assert!(normalise(&[]).is_empty());
        let many: Vec<[u16; 2]> = (0..100).map(|n| [n * 10, 500]).collect();
        assert_eq!(normalise(&many).len(), MAX_POINTS);
    }
}
