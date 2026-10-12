/// An axis-aligned rectangle in landscape bar coordinates (x along the bar, y across it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Shrinks by `[top, right, bottom, left]`, never below zero size.
    pub fn inset(&self, s: [f32; 4]) -> Rect {
        Rect::new(
            self.x + s[3],
            self.y + s[0],
            (self.w - s[1] - s[3]).max(0.0),
            (self.h - s[0] - s[2]).max(0.0),
        )
    }
}

/// How wide an item or zone is (H6: Auto, Fixed px, or Fill).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Width {
    /// Fits its content (and the style's `min-width`).
    Auto,
    /// Exactly this many pixels, unless the bar overflows.
    Fixed(f32),
    /// Shares the leftover space by weight. `Fill(n)` spans `n` slots *including the gaps between
    /// them*, so a row of `Fill` items reproduces tiny-dfr's `Stretch` geometry exactly.
    Fill(u32),
    /// Fits its content, but never narrower than the first or wider than the second value (a
    /// published item's `width = {min, max}`).
    Between(f32, f32),
}

/// Where children sit when they don't fill their container.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// One child's sizing input to [`solve`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizing {
    pub width: Width,
    /// Preferred width for `Auto` (content, padding, border, `min-width`).
    pub natural: f32,
    /// The narrowest it may be squeezed to on overflow.
    pub min: f32,
}

/// Lays out children along a span of `width` px with `gap` between them. Returns `(x, w)` per
/// child, relative to the span's start, in floating point (round at the end).
///
/// Leftover space goes to `Fill` children, otherwise it's distributed by `align`. On overflow
/// (ux.md §4), `Fill` children compress to their minimum first, then the widest children are capped
/// at a common width like a tab strip (never below their minimum), and as a last resort everything
/// scales to fit. Children never leave the span.
pub fn solve(children: &[Sizing], width: f32, gap: f32, align: Align) -> Vec<(f32, f32)> {
    if children.is_empty() {
        return Vec::new();
    }
    let avail = (width - gap * (children.len() - 1) as f32).max(0.0);
    let weight = |s: &Sizing| match s.width {
        Width::Fill(w) => w.max(1) as f32,
        _ => 0.0,
    };
    let is_fill = |s: &Sizing| matches!(s.width, Width::Fill(_));
    let fill_weight: f32 = children.iter().map(weight).sum();
    // Gaps "inside" multi-slot Fill items, which tiny-dfr counts as part of the item.
    let fill_inner: f32 = children
        .iter()
        .map(|s| (weight(s) - 1.0).max(0.0) * gap)
        .sum();

    let mut widths: Vec<f32> = children
        .iter()
        .map(|s| match s.width {
            Width::Auto | Width::Between(..) => s.natural.max(0.0),
            Width::Fixed(px) => px.max(0.0),
            Width::Fill(_) => 0.0,
        })
        .collect();
    let base: f32 = widths.iter().sum();
    let fill_min: f32 = children.iter().filter(|s| is_fill(s)).map(|s| s.min).sum();
    let mut leftover = 0.0;

    if base + fill_min <= avail {
        leftover = avail - base;
        if fill_weight > 0.0 {
            let slot = (leftover - fill_inner) / fill_weight;
            for (w, s) in widths.iter_mut().zip(children) {
                if is_fill(s) {
                    *w = (weight(s) * slot + (weight(s) - 1.0) * gap).max(0.0);
                }
            }
            leftover = 0.0;
        }
    } else {
        for (w, s) in widths.iter_mut().zip(children) {
            if is_fill(s) {
                *w = s.min;
            }
        }
        // Cap the widest non-Fill children at a common width, respecting each one's minimum.
        let budget = avail - fill_min;
        let capped = |cap: f32| -> f32 {
            widths
                .iter()
                .zip(children)
                .filter(|(_, s)| !is_fill(s))
                .map(|(w, s)| w.min(cap.max(s.min)))
                .sum()
        };
        let (mut lo, mut hi) = (0.0, widths.iter().copied().fold(0.0, f32::max));
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if capped(mid) <= budget {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        for (w, s) in widths.iter_mut().zip(children) {
            if !is_fill(s) {
                *w = w.min(lo.max(s.min));
            }
        }
        let total: f32 = widths.iter().sum();
        if total > avail && total > 0.0 {
            let k = avail / total;
            widths.iter_mut().for_each(|w| *w *= k);
        }
    }

    let mut x = match align {
        Align::Start => 0.0,
        Align::Center => leftover / 2.0,
        Align::End => leftover,
    };
    widths
        .into_iter()
        .map(|w| {
            let r = (x, w);
            x += w + gap;
            r
        })
        .collect()
}

/// Returns the index of the item under bar coordinate `x`.
///
/// The whole bar height is touchable, and each gap between items is split down the middle, so
/// there are no dead zones between buttons. Callers decide whether the hit item is interactive.
pub fn hit_test(rects: &[Rect], x: f32) -> Option<usize> {
    let n = rects.len();
    (0..n).find(|&i| {
        let left = if i == 0 {
            f32::NEG_INFINITY
        } else {
            (rects[i - 1].right() + rects[i].x) / 2.0
        };
        let right = if i + 1 == n {
            f32::INFINITY
        } else {
            (rects[i].right() + rects[i + 1].x) / 2.0
        };
        x >= left && x < right
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(n: u32) -> Sizing {
        Sizing {
            width: Width::Fill(n),
            natural: 0.0,
            min: 60.0,
        }
    }

    fn auto(natural: f32) -> Sizing {
        Sizing {
            width: Width::Auto,
            natural,
            min: natural.min(60.0),
        }
    }

    fn fixed(px: f32) -> Sizing {
        Sizing {
            width: Width::Fixed(px),
            natural: 0.0,
            min: px.min(60.0),
        }
    }

    fn one(items: &[Sizing], width: f32) -> Vec<(f32, f32)> {
        solve(items, width, 16.0, Align::Start)
    }

    /// No overlaps, gaps kept, everything inside the span.
    fn assert_sane(r: &[(f32, f32)], width: f32) {
        for &(x, w) in r {
            assert!(
                x >= -0.01 && x + w <= width + 0.01,
                "{x},{w} outside {width}"
            );
        }
        for p in r.windows(2) {
            assert!(p[1].0 - (p[0].0 + p[0].1) >= 15.99, "{p:?}");
        }
    }

    #[test]
    fn fill_matches_tiny_dfr_slots() {
        // Esc + F1..F12 on the 2008 px Apple Silicon bar.
        let r = one(&[fill(1); 13], 2008.0);
        let slot = (2008.0 - 16.0 * 12.0) / 13.0;
        for (i, &(x, w)) in r.iter().enumerate() {
            assert!((x - i as f32 * (slot + 16.0)).abs() < 0.01);
            assert!((w - slot).abs() < 0.01);
        }
        // Stretch 2 spans two slots and the gap between them.
        let r = one(&[fill(2), fill(1), fill(1)], 1000.0);
        let slot = (1000.0 - 48.0) / 4.0;
        assert!((r[0].1 - (2.0 * slot + 16.0)).abs() < 0.01);
        assert!((r[1].0 - 2.0 * (slot + 16.0)).abs() < 0.01);
        assert_sane(&r, 1000.0);
    }

    #[test]
    fn auto_fixed_and_fill_share_space() {
        let r = one(&[auto(80.0), auto(130.0), fixed(200.0), fill(1)], 1000.0);
        assert_eq!((r[0].1, r[1].1, r[2].1), (80.0, 130.0, 200.0));
        assert_eq!(r[3].0 + r[3].1, 1000.0);
        assert_sane(&r, 1000.0);
    }

    #[test]
    fn align_distributes_leftover() {
        let items = [fixed(100.0), fixed(100.0)];
        assert_eq!(solve(&items, 1000.0, 0.0, Align::Start)[0].0, 0.0);
        assert_eq!(solve(&items, 1000.0, 0.0, Align::Center)[0].0, 400.0);
        assert_eq!(solve(&items, 1000.0, 0.0, Align::End)[1], (900.0, 100.0));
    }

    #[test]
    fn overflow_compresses_fill_then_caps_widest() {
        let r = one(&[fixed(400.0), fixed(400.0), fill(1)], 880.0);
        assert_eq!(r[2].1, 60.0);
        assert!((r[2].0 + r[2].1 - 880.0).abs() < 0.01);
        assert_sane(&r, 880.0);
        // Short items keep their size, wide ones are capped at a common width.
        let r = one(&[fixed(100.0), fixed(600.0), fixed(900.0)], 1032.0);
        assert_eq!(r[0].1, 100.0);
        assert!((r[1].1 - 450.0).abs() < 0.5 && (r[2].1 - 450.0).abs() < 0.5);
        // Way too many items: everything scales, stays inside, no overlap.
        let r = one(&[auto(300.0); 40], 2008.0);
        assert_sane(&r, 2008.0);
        assert!((r[39].0 + r[39].1 - 2008.0).abs() < 0.01);
    }

    #[test]
    fn empty_is_empty() {
        assert!(solve(&[], 500.0, 16.0, Align::Start).is_empty());
    }

    #[test]
    fn hit_test_splits_gaps_and_covers_edges() {
        let rects = [
            Rect::new(0.0, 0.0, 500.0, 60.0),
            Rect::new(516.0, 0.0, 500.0, 60.0),
        ];
        assert_eq!(hit_test(&rects, -5.0), Some(0));
        assert_eq!(hit_test(&rects, 507.9), Some(0));
        assert_eq!(hit_test(&rects, 508.0), Some(1));
        assert_eq!(hit_test(&rects, 5000.0), Some(1));
        assert_eq!(hit_test(&[], 1.0), None);
    }
}
