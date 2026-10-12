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

/// Lays out a row of items across a bar `width` x `height` pixels.
///
/// Each item takes `stretch` slots (0 is treated as 1). Slots are separated by `spacing` pixels,
/// and every item is inset by `margin_y` from the top and bottom. The math matches tiny-dfr, so
/// imported layouts land on the same pixels.
pub fn layout_row(
    width: u32,
    height: u32,
    stretches: &[u32],
    spacing: f32,
    margin_y: f32,
) -> Vec<Rect> {
    let slots: u32 = stretches.iter().map(|s| (*s).max(1)).sum();
    if slots == 0 {
        return Vec::new();
    }
    let slot_w = (width as f32 - spacing * (slots - 1) as f32) / slots as f32;
    let pitch = slot_w + spacing;
    let h = (height as f32 - 2.0 * margin_y).max(0.0);
    let mut start = 0;
    stretches
        .iter()
        .map(|s| {
            let s = (*s).max(1);
            let x = (start as f32 * pitch).floor();
            let w = slot_w + ((s - 1) as f32 * pitch).floor();
            start += s;
            Rect::new(x, margin_y, w.ceil(), h)
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

    #[test]
    fn tiny_dfr_primary_layer_geometry() {
        // Esc + F1..F12 on the 2008 px Apple Silicon bar, tiny-dfr's 16 px spacing.
        let rects = layout_row(2008, 60, &[1; 13], 16.0, 1.0);
        assert_eq!(rects.len(), 13);
        let slot_w = (2008.0 - 16.0 * 12.0) / 13.0;
        assert_eq!(rects[0].x, 0.0);
        assert_eq!(rects[1].x, (slot_w + 16.0_f32).floor());
        let last = rects[12];
        assert!((last.right() - 2008.0).abs() <= 1.0, "{last:?}");
        assert_eq!((rects[0].y, rects[0].h), (1.0, 58.0));
    }

    #[test]
    fn stretch_spans_slots_and_gaps() {
        let rects = layout_row(1000, 60, &[2, 1, 1], 10.0, 0.0);
        let slot_w = (1000.0 - 30.0) / 4.0;
        assert_eq!(rects[0].w, (slot_w * 2.0 + 10.0_f32).ceil());
        assert_eq!(rects[1].x, ((slot_w + 10.0) * 2.0_f32).floor());
    }

    #[test]
    fn zero_stretch_is_one() {
        assert_eq!(
            layout_row(100, 10, &[0, 1], 0.0, 0.0),
            layout_row(100, 10, &[1, 1], 0.0, 0.0)
        );
    }

    #[test]
    fn hit_test_splits_gaps_and_covers_edges() {
        let rects = layout_row(1000, 60, &[1, 1], 20.0, 0.0);
        assert_eq!(rects[0].right(), 490.0);
        assert_eq!(hit_test(&rects, -5.0), Some(0));
        assert_eq!(hit_test(&rects, 499.9), Some(0));
        assert_eq!(hit_test(&rects, 500.0), Some(1));
        assert_eq!(hit_test(&rects, 5000.0), Some(1));
        assert_eq!(hit_test(&[], 1.0), None);
    }
}
