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
}

/// How wide an item is (H6: each button picks Auto, Fixed px, or Fill).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Width {
    /// Fits its content, but never narrower than the theme's comfortable touch width.
    Auto,
    /// Exactly this many pixels, unless the bar overflows.
    Fixed(f32),
    /// Shares the leftover space by weight. `Fill(n)` spans `n` slots *including the gaps between
    /// them*, so a row of `Fill` items reproduces tiny-dfr's `Stretch` geometry exactly.
    Fill(u32),
}

/// One item's sizing input to [`solve`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizing {
    pub width: Width,
    /// Content width plus padding, for `Auto` items.
    pub natural: f32,
}

/// Spacing knobs for [`solve`], normally taken from the [`crate::Theme`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    /// Gap between items in a zone.
    pub item: f32,
    /// Gap between non-empty zones.
    pub zone: f32,
    pub margin_y: f32,
    /// `Auto` items are at least this wide.
    pub comfortable: f32,
    /// Overflowing items are squeezed down to this, and no further unless nothing else fits.
    pub min: f32,
}

/// Lays out zones of items (left to right) across a `width` x `height` bar. Returns one rect per
/// item, in order.
///
/// Leftover space goes to `Fill` items. With none, it goes to the end of the `grow` zones, so
/// trailing zones are pushed to the right edge. On overflow (ux.md §4), `Fill` items compress to
/// the minimum first, then the widest `Auto` and `Fixed` items are capped at a common width (never
/// below the minimum). As a last resort everything scales down to fit. Items never leave the bar.
pub fn solve(zones: &[(bool, Vec<Sizing>)], width: u32, height: u32, sp: Spacing) -> Vec<Rect> {
    let items: Vec<Sizing> = zones.iter().flat_map(|z| z.1.iter().copied()).collect();
    let nonempty = zones.iter().filter(|z| !z.1.is_empty()).count();
    if items.is_empty() {
        return Vec::new();
    }
    let gaps = sp.item * (items.len() - nonempty) as f32 + sp.zone * (nonempty - 1) as f32;
    let avail = (width as f32 - gaps).max(0.0);

    let weight = |s: &Sizing| match s.width {
        Width::Fill(w) => w.max(1) as f32,
        _ => 0.0,
    };
    let fill_weight: f32 = items.iter().map(weight).sum();
    // Gaps "inside" multi-slot Fill items, which tiny-dfr counts as part of the item.
    let fill_inner: f32 = items
        .iter()
        .map(|s| (weight(s) - 1.0).max(0.0) * sp.item)
        .sum();
    let fill_span = |w: f32, slot: f32| w * slot + (w - 1.0) * sp.item;

    let mut widths: Vec<f32> = items
        .iter()
        .map(|s| match s.width {
            Width::Auto => s.natural.max(sp.comfortable),
            Width::Fixed(px) => px.max(0.0),
            Width::Fill(_) => 0.0,
        })
        .collect();
    let base: f32 = widths.iter().sum();
    let fill_min = fill_weight * sp.min + fill_inner;
    let mut grow_extra = 0.0;

    if base + fill_min <= avail {
        let leftover = avail - base;
        if fill_weight > 0.0 {
            let slot = (leftover - fill_inner) / fill_weight;
            for (w, s) in widths.iter_mut().zip(&items) {
                if let Width::Fill(_) = s.width {
                    *w = fill_span(weight(s), slot);
                }
            }
        } else {
            grow_extra = leftover;
        }
    } else {
        for (w, s) in widths.iter_mut().zip(&items) {
            if let Width::Fill(_) = s.width {
                *w = fill_span(weight(s), sp.min);
            }
        }
        // Like a tab strip: cap the widest Auto/Fixed items at a common width `cap` (never below
        // the minimum), so short items keep their natural size.
        let budget = avail - fill_min;
        let capped = |cap: f32| -> f32 {
            widths
                .iter()
                .zip(&items)
                .map(|(w, s)| match s.width {
                    Width::Fill(_) => 0.0,
                    _ => w.min(cap),
                })
                .sum()
        };
        let (mut lo, mut hi) = (sp.min, widths.iter().copied().fold(sp.min, f32::max));
        if capped(lo) <= budget {
            for _ in 0..32 {
                let mid = (lo + hi) / 2.0;
                if capped(mid) <= budget {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
        }
        for (w, s) in widths.iter_mut().zip(&items) {
            if !matches!(s.width, Width::Fill(_)) {
                *w = w.min(lo);
            }
        }
        // Last resort: even at the minimum it doesn't fit, so scale everything.
        let total: f32 = widths.iter().sum();
        if total > avail && total > 0.0 {
            let k = avail / total;
            widths.iter_mut().for_each(|w| *w *= k);
        }
    }

    let grow_zones = zones.iter().filter(|z| z.0 && !z.1.is_empty()).count();
    let per_grow = if grow_zones > 0 {
        grow_extra / grow_zones as f32
    } else {
        0.0
    };
    let h = (height as f32 - 2.0 * sp.margin_y).max(0.0);
    let mut rects = Vec::with_capacity(items.len());
    let mut x = 0.0_f32;
    let mut widths = widths.into_iter();
    for (grow, zone) in zones.iter().filter(|z| !z.1.is_empty()) {
        if !rects.is_empty() {
            x += sp.zone;
        }
        for (i, _) in zone.iter().enumerate() {
            if i > 0 {
                x += sp.item;
            }
            let w = widths.next().unwrap_or(0.0);
            let (x0, x1) = (x.round(), (x + w).round());
            rects.push(Rect::new(x0, sp.margin_y, x1 - x0, h));
            x += w;
        }
        if *grow {
            x += per_grow;
        }
    }
    rects
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

    const SP: Spacing = Spacing {
        item: 16.0,
        zone: 16.0,
        margin_y: 1.0,
        comfortable: 80.0,
        min: 60.0,
    };

    fn fill(n: u32) -> Sizing {
        Sizing {
            width: Width::Fill(n),
            natural: 0.0,
        }
    }

    fn auto(natural: f32) -> Sizing {
        Sizing {
            width: Width::Auto,
            natural,
        }
    }

    fn fixed(px: f32) -> Sizing {
        Sizing {
            width: Width::Fixed(px),
            natural: 0.0,
        }
    }

    fn one(items: Vec<Sizing>, width: u32) -> Vec<Rect> {
        solve(&[(true, items)], width, 60, SP)
    }

    /// No overlaps, gaps at least `min_gap`, everything on the bar.
    fn assert_sane(rects: &[Rect], width: u32, min_gap: f32) {
        for r in rects {
            assert!(r.x >= 0.0 && r.right() <= width as f32, "{r:?} off-bar");
        }
        for p in rects.windows(2) {
            assert!(p[1].x - p[0].right() >= min_gap - 1.0, "{p:?}");
        }
    }

    #[test]
    fn fill_matches_tiny_dfr_slots() {
        // Esc + F1..F12 on the 2008 px Apple Silicon bar.
        let rects = one(vec![fill(1); 13], 2008);
        let slot = (2008.0 - 16.0 * 12.0) / 13.0;
        for (i, r) in rects.iter().enumerate() {
            assert_eq!(r.x, (i as f32 * (slot + 16.0)).round());
            assert!((r.w - slot).abs() <= 1.0);
        }
        assert_eq!(rects[12].right(), 2008.0);
        assert_eq!((rects[0].y, rects[0].h), (1.0, 58.0));
        // Stretch 2 spans two slots and the gap between them.
        let rects = one(vec![fill(2), fill(1), fill(1)], 1000);
        let slot = (1000.0 - 48.0) / 4.0;
        assert!((rects[0].w - (2.0 * slot + 16.0)).abs() <= 1.0);
        assert_eq!(rects[1].x, (2.0 * (slot + 16.0_f32)).round());
        assert_sane(&rects, 1000, 16.0);
    }

    #[test]
    fn auto_fixed_and_fill_share_space() {
        let rects = one(vec![auto(50.0), auto(130.0), fixed(200.0), fill(1)], 1000);
        assert_eq!(rects[0].w, 80.0, "auto is at least comfortable");
        assert_eq!(rects[1].w, 130.0);
        assert_eq!(rects[2].w, 200.0);
        assert_eq!(rects[3].right(), 1000.0);
        assert_sane(&rects, 1000, 16.0);
    }

    #[test]
    fn leftover_goes_to_grow_zones() {
        let zones = [
            (false, vec![auto(0.0)]),
            (true, vec![auto(0.0), auto(0.0)]),
            (false, vec![fixed(100.0)]),
        ];
        let rects = solve(&zones, 1000, 60, SP);
        assert_eq!(rects[0].x, 0.0);
        assert_eq!(rects[1].x, 96.0, "zone gap after esc");
        assert_eq!(rects[3].right(), 1000.0, "controls pushed right");
        assert_sane(&rects, 1000, 16.0);
    }

    #[test]
    fn overflow_compresses_fill_then_shrinks_others() {
        // Fill compresses first: fixed items keep their size.
        let rects = one(vec![fixed(400.0), fixed(400.0), fill(1)], 880);
        assert_eq!(rects[2].w, 60.0);
        assert_eq!(rects[2].right(), 880.0);
        assert_sane(&rects, 880, 16.0);
        assert!(rects[0].w < 400.0 && rects[0].w >= 60.0);
        // Short items keep their size, wide ones are capped at a common width.
        let rects = one(vec![fixed(100.0), fixed(600.0), fixed(900.0)], 1032);
        assert_eq!(rects[0].w, 100.0);
        assert!((rects[1].w - 450.0).abs() <= 1.0 && (rects[2].w - 450.0).abs() <= 1.0);
        assert_sane(&rects, 1032, 16.0);
        // Way too many items: everything scales, stays on the bar, no overlap.
        let rects = one(vec![auto(300.0); 40], 2008);
        assert_sane(&rects, 2008, 16.0);
        assert_eq!(rects.last().unwrap().right(), 2008.0);
    }

    #[test]
    fn empty_zones_add_no_gaps() {
        let rects = solve(&[(false, vec![]), (true, vec![fill(1)])], 500, 60, SP);
        assert_eq!((rects[0].x, rects[0].w), (0.0, 500.0));
        assert!(solve(&[(true, vec![])], 500, 60, SP).is_empty());
    }

    #[test]
    fn hit_test_splits_gaps_and_covers_edges() {
        let rects = one(vec![fill(1), fill(1)], 1016);
        assert_eq!(rects[0].right(), 500.0);
        assert_eq!(hit_test(&rects, -5.0), Some(0));
        assert_eq!(hit_test(&rects, 507.9), Some(0));
        assert_eq!(hit_test(&rects, 508.0), Some(1));
        assert_eq!(hit_test(&rects, 5000.0), Some(1));
        assert_eq!(hit_test(&[], 1.0), None);
    }
}
