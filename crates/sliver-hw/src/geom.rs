//! Landscape (what users see, e.g. 2008x60) <-> portrait scanout (60x2008) mapping.
//!
//! Same convention as tiny-dfr (`translate(fb_w, 0); rotate(90deg)`) and
//! `sliver-safeswap snap`: landscape `(lx, ly)` lives at fb `(x = fb_w - 1 - ly, y = lx)`.

/// fb pixel coordinates of landscape pixel `(lx, ly)` for a scanout `vis_w` pixels wide.
pub const fn landscape_to_fb(lx: u32, ly: u32, vis_w: u32) -> (u32, u32) {
    (vis_w - 1 - ly, lx)
}

/// Fill an XRGB8888 portrait buffer from a landscape pixel function `f(lx, ly) -> 0xRRGGBB`.
/// `vis_w` x `len` is the visible portrait size, `pitch` the buffer stride in bytes.
pub fn fill_portrait(
    buf: &mut [u8],
    pitch: usize,
    vis_w: u32,
    len: u32,
    f: impl Fn(u32, u32) -> u32,
) {
    for lx in 0..len {
        for ly in 0..vis_w {
            let (x, y) = landscape_to_fb(lx, ly, vis_w);
            let o = y as usize * pitch + x as usize * 4;
            buf[o..o + 4].copy_from_slice(&(f(lx, ly) & 0x00ff_ffff).to_le_bytes());
        }
    }
}

/// Orientation test pattern (landscape `len` x `h`): red->blue gradient left to right,
/// a white block at the top-left, a green block at the bottom-right. That makes
/// mirroring/rotation mistakes obvious in a snap.
pub fn test_pattern(lx: u32, ly: u32, len: u32, h: u32) -> u32 {
    let block = h;
    if lx < block && ly < h / 2 {
        return 0xffffff;
    }
    if lx >= len.saturating_sub(block) && ly >= h / 2 {
        return 0x00ff00;
    }
    let t = lx * 255 / len.max(1);
    ((255 - t) << 16) | t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_corners() {
        // top-left of the landscape bar is the top-right column of the portrait fb
        assert_eq!(landscape_to_fb(0, 0, 60), (59, 0));
        assert_eq!(landscape_to_fb(2007, 59, 60), (0, 2007));
    }

    #[test]
    fn fill_matches_mapping() {
        let (w, len, pitch) = (4u32, 6u32, 5 * 4);
        let mut buf = vec![0u8; pitch * len as usize];
        fill_portrait(&mut buf, pitch, w, len, |lx, ly| (lx << 8) | ly);
        for lx in 0..len {
            for ly in 0..w {
                let (x, y) = landscape_to_fb(lx, ly, w);
                let o = y as usize * pitch + x as usize * 4;
                assert_eq!(
                    u32::from_le_bytes(buf[o..o + 4].try_into().unwrap()),
                    (lx << 8) | ly
                );
            }
        }
    }

    #[test]
    fn pattern_markers() {
        assert_eq!(test_pattern(0, 0, 2008, 60), 0xffffff);
        assert_eq!(test_pattern(2007, 59, 2008, 60), 0x00ff00);
        assert_eq!(
            test_pattern(1000, 50, 2008, 60) >> 16,
            255 - 1000 * 255 / 2008
        );
    }
}
