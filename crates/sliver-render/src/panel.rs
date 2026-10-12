//! Getting frames onto a real panel: change detection and rotation into the framebuffer layout.
//!
//! The renderer always works in landscape bar coordinates (x along the bar). Panels report a
//! portrait mode (Apple Silicon: 60x2008), so frames are rotated on the way out, exactly like
//! tiny-dfr's `translate(height, 0); rotate(90°)`.

use tiny_skia::Pixmap;

/// How the panel's framebuffer is laid out relative to the landscape bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// Framebuffer is as wide as the bar.
    Landscape,
    /// Framebuffer is `bar_height` wide and `bar_width` tall. Bar x runs down the framebuffer rows,
    /// bar y runs right-to-left across columns (tiny-dfr's transform).
    Rotate90,
}

impl Orientation {
    /// Framebuffer size for a landscape bar of `w` x `h`.
    pub fn panel_size(self, w: u32, h: u32) -> (u32, u32) {
        match self {
            Orientation::Landscape => (w, h),
            Orientation::Rotate90 => (h, w),
        }
    }

    /// Maps a span of bar columns `[x, x + w)` covering the full bar height to a framebuffer
    /// rectangle `(x, y, w, h)` (e.g. for DRM dirty clips).
    pub fn panel_rect(self, span: Span, bar_h: u32) -> (u32, u32, u32, u32) {
        match self {
            Orientation::Landscape => (span.x, 0, span.w, bar_h),
            Orientation::Rotate90 => (0, span.x, bar_h, span.w),
        }
    }
}

/// A run of changed bar columns, `[x, x + w)`, spanning the full bar height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub x: u32,
    pub w: u32,
}

/// Column spans that differ between two frames. Spans closer than `merge_gap` columns are merged
/// to keep the number of dirty clips small. A size change damages everything.
pub fn damage(prev: &Pixmap, next: &Pixmap, merge_gap: u32) -> Vec<Span> {
    let (w, h) = (next.width(), next.height());
    if (prev.width(), prev.height()) != (w, h) {
        return vec![Span { x: 0, w }];
    }
    let (a, b) = (prev.data(), next.data());
    let row = w as usize * 4;
    let column_changed = |x: u32| {
        let off = x as usize * 4;
        (0..h as usize).any(|y| a[y * row + off..][..4] != b[y * row + off..][..4])
    };
    let mut spans: Vec<Span> = Vec::new();
    for x in (0..w).filter(|&x| column_changed(x)) {
        match spans.last_mut() {
            Some(s) if x <= s.x + s.w + merge_gap => s.w = x + 1 - s.x,
            _ => spans.push(Span { x, w: 1 }),
        }
    }
    spans
}

/// Writes bar columns `span` of `src` into an XRGB8888 (little-endian: B, G, R, X) framebuffer
/// with `stride` bytes per row, laid out per `orientation`. Pass `None` to write the whole frame.
pub fn blit_xrgb8888(
    src: &Pixmap,
    orientation: Orientation,
    span: Option<Span>,
    dst: &mut [u8],
    stride: usize,
) {
    let (w, h) = (src.width(), src.height());
    let span = span.unwrap_or(Span { x: 0, w });
    let x_end = (span.x + span.w).min(w);
    let data = src.data();
    // Pixmaps are premultiplied, so writing the color channels composites over black.
    let put = |dst: &mut [u8], off: usize, s: &[u8]| {
        dst[off..off + 4].copy_from_slice(&[s[2], s[1], s[0], 0]);
    };
    for y in 0..h {
        for x in span.x..x_end {
            let s = &data[(y * w + x) as usize * 4..][..4];
            let off = match orientation {
                Orientation::Landscape => y as usize * stride + x as usize * 4,
                Orientation::Rotate90 => x as usize * stride + (h - 1 - y) as usize * 4,
            };
            put(dst, off, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::{Color, Paint, Rect, Transform};

    fn frame(w: u32, h: u32, dot: Option<(f32, f32)>) -> Pixmap {
        let mut p = Pixmap::new(w, h).unwrap();
        p.fill(Color::BLACK);
        if let Some((x, y)) = dot {
            let mut paint = Paint::default();
            paint.set_color_rgba8(255, 0, 0, 255);
            let r = Rect::from_xywh(x, y, 1.0, 1.0).unwrap();
            p.fill_rect(r, &paint, Transform::identity(), None);
        }
        p
    }

    #[test]
    fn damage_finds_and_merges_changed_columns() {
        let a = frame(100, 4, None);
        assert!(damage(&a, &a, 0).is_empty());
        let mut b = frame(100, 4, Some((10.0, 3.0)));
        let paint = {
            let mut p = Paint::default();
            p.set_color_rgba8(0, 255, 0, 255);
            p
        };
        b.fill_rect(
            Rect::from_xywh(13.0, 0.0, 2.0, 1.0).unwrap(),
            &paint,
            Transform::identity(),
            None,
        );
        assert_eq!(
            damage(&a, &b, 0),
            vec![Span { x: 10, w: 1 }, Span { x: 13, w: 2 }]
        );
        assert_eq!(damage(&a, &b, 2), vec![Span { x: 10, w: 5 }]);
        assert_eq!(
            damage(&a, &frame(50, 4, None), 0),
            vec![Span { x: 0, w: 50 }]
        );
    }

    #[test]
    fn rotate90_matches_tiny_dfr_transform() {
        // Bar pixel (x=7, y=1) on a 20x3 bar lands at framebuffer column h-1-y=1, row x=7.
        let src = frame(20, 3, Some((7.0, 1.0)));
        let (pw, ph) = Orientation::Rotate90.panel_size(20, 3);
        assert_eq!((pw, ph), (3, 20));
        let stride = pw as usize * 4;
        let mut fb = vec![0u8; stride * ph as usize];
        blit_xrgb8888(&src, Orientation::Rotate90, None, &mut fb, stride);
        let px = |c: usize, r: usize| &fb[r * stride + c * 4..][..4];
        assert_eq!(px(1, 7), &[0, 0, 255, 0]);
        assert_eq!(px(0, 7), &[0, 0, 0, 0]);
        assert_eq!(
            Orientation::Rotate90.panel_rect(Span { x: 7, w: 2 }, 3),
            (0, 7, 3, 2)
        );
    }

    #[test]
    fn landscape_blit_respects_span() {
        let src = frame(4, 1, Some((2.0, 0.0)));
        let mut fb = vec![9u8; 16];
        blit_xrgb8888(
            &src,
            Orientation::Landscape,
            Some(Span { x: 2, w: 1 }),
            &mut fb,
            16,
        );
        assert_eq!(&fb[8..12], &[0, 0, 255, 0]);
        assert_eq!(&fb[0..8], &[9; 8]);
    }
}
