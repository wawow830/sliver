//! What sliverd checks before trusting anything from the agent (`docs/api.md` §6).
//!
//! sliverd treats the agent as untrusted. A frame that fails [`validate_present`] is rejected
//! (the previous frame stays up). A frame that fails only [`escape_floor`] is shown with the
//! built-in fn layer swapped in.

use crate::keys;
use crate::proto::{MAX_BUFFERS, PanelInfo, PixelFormat, Present, Region, RegionKind};
use std::collections::HashSet;
use std::fmt;

pub const MAX_REGIONS: usize = 128;
/// Keys per region or per `emit_keys` (combos like Ctrl+Alt+Shift+T).
pub const MAX_KEYS_PER_COMBO: usize = 8;
/// A key region counts toward the escape floor only if it's at least this wide.
pub const MIN_FLOOR_KEY_WIDTH: u32 = 32;
pub const MAX_DAMAGE_SPANS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    BufferSize { width: u32, height: u32 },
    BufferStride { stride: u32, min: u32 },
    BufferFormat(PixelFormat),
    TooManyBuffers,
    TooManyRegions(usize),
    DuplicateRegionId(u32),
    EmptyRegion(u32),
    RegionOutOfBounds(u32),
    RegionsOverlap(u32, u32),
    NoKeys(u32),
    TooManyKeys(usize),
    BadKey(u16),
    ForbiddenKey(u16),
    BadDamage,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Invalid::*;
        match self {
            BufferSize { width, height } => {
                write!(f, "buffer {width}x{height} doesn't match the panel")
            }
            BufferStride { stride, min } => write!(f, "stride {stride} < {min}"),
            BufferFormat(p) => write!(f, "unsupported format {p:?}"),
            TooManyBuffers => write!(f, "more than {MAX_BUFFERS} buffers attached"),
            TooManyRegions(n) => write!(f, "{n} regions > {MAX_REGIONS}"),
            DuplicateRegionId(id) => write!(f, "duplicate region id {id}"),
            EmptyRegion(id) => write!(f, "region {id} is empty"),
            RegionOutOfBounds(id) => write!(f, "region {id} is outside the panel"),
            RegionsOverlap(a, b) => write!(f, "regions {a} and {b} overlap"),
            NoKeys(id) => write!(f, "key region {id} has no keys"),
            TooManyKeys(n) => write!(f, "{n} keys > {MAX_KEYS_PER_COMBO}"),
            BadKey(k) => write!(f, "invalid keycode {k}"),
            ForbiddenKey(k) => write!(f, "keycode {k} is forbidden"),
            BadDamage => write!(f, "damage spans out of bounds or too many"),
        }
    }
}

impl std::error::Error for Invalid {}

impl Invalid {
    pub fn code(&self) -> crate::proto::ErrorCode {
        use crate::proto::ErrorCode;
        match self {
            Invalid::BufferSize { .. }
            | Invalid::BufferStride { .. }
            | Invalid::BufferFormat(_)
            | Invalid::TooManyBuffers => ErrorCode::InvalidBuffer,
            Invalid::ForbiddenKey(_) => ErrorCode::ForbiddenKey,
            _ => ErrorCode::InvalidFrame,
        }
    }
}

/// Checks an `attach_buffer` header. The fd's seals and size (≥ [`required_len`]) are checked by
/// sliverd itself.
pub fn validate_buffer(
    panel: &PanelInfo,
    attached: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: PixelFormat,
) -> Result<(), Invalid> {
    if attached >= MAX_BUFFERS {
        return Err(Invalid::TooManyBuffers);
    }
    if format != panel.format {
        return Err(Invalid::BufferFormat(format));
    }
    if width != panel.width || height != panel.height {
        return Err(Invalid::BufferSize { width, height });
    }
    let min = width.saturating_mul(format.bytes_per_pixel());
    if stride < min || stride % 4 != 0 {
        return Err(Invalid::BufferStride { stride, min });
    }
    Ok(())
}

/// Bytes the memfd must hold for this buffer.
pub fn required_len(stride: u32, height: u32) -> u64 {
    u64::from(stride) * u64::from(height)
}

/// Checks keys for a region or `emit_keys`.
pub fn validate_keys(keys: &[u16]) -> Result<(), Invalid> {
    if keys.len() > MAX_KEYS_PER_COMBO {
        return Err(Invalid::TooManyKeys(keys.len()));
    }
    for &k in keys {
        if keys::FORBIDDEN.contains(&k) {
            return Err(Invalid::ForbiddenKey(k));
        }
        if !keys::emittable(k) {
            return Err(Invalid::BadKey(k));
        }
    }
    Ok(())
}

/// Structural checks on a `present`: regions in bounds, unique, non-overlapping, sane keys.
pub fn validate_present(panel: &PanelInfo, p: &Present) -> Result<(), Invalid> {
    validate_regions(panel, &p.regions)?;
    if let Some(damage) = &p.damage {
        let ok = damage.len() <= MAX_DAMAGE_SPANS
            && damage
                .iter()
                .all(|[x, w]| u64::from(*x) + u64::from(*w) <= u64::from(panel.width));
        if !ok {
            return Err(Invalid::BadDamage);
        }
    }
    Ok(())
}

pub fn validate_regions(panel: &PanelInfo, regions: &[Region]) -> Result<(), Invalid> {
    if regions.len() > MAX_REGIONS {
        return Err(Invalid::TooManyRegions(regions.len()));
    }
    let mut ids = HashSet::with_capacity(regions.len());
    for r in regions {
        if !ids.insert(r.id) {
            return Err(Invalid::DuplicateRegionId(r.id));
        }
        if r.rect.w == 0 || r.rect.h == 0 {
            return Err(Invalid::EmptyRegion(r.id));
        }
        if r.rect.right() > u64::from(panel.width) || r.rect.bottom() > u64::from(panel.height) {
            return Err(Invalid::RegionOutOfBounds(r.id));
        }
        if let RegionKind::Key { keys } = &r.kind {
            if keys.is_empty() {
                return Err(Invalid::NoKeys(r.id));
            }
            validate_keys(keys)?;
        }
    }
    // ≤ 128 regions, so O(n²) is at most ~8k cheap comparisons.
    for (i, a) in regions.iter().enumerate() {
        for b in &regions[i + 1..] {
            if a.rect.intersects(&b.rect) {
                return Err(Invalid::RegionsOverlap(a.id, b.id));
            }
        }
    }
    Ok(())
}

/// Keys from `required` that aren't reachable as wide-enough single-key regions in either
/// layer. Empty = the floor holds. Pass `fn_layer = None` when the built-in fn layer is in use
/// (it always satisfies the floor).
pub fn escape_floor(base: &[Region], fn_layer: Option<&[Region]>, required: &[u16]) -> Vec<u16> {
    let Some(fn_layer) = fn_layer else {
        return Vec::new();
    };
    let reachable: HashSet<u16> = base
        .iter()
        .chain(fn_layer)
        .filter(|r| r.rect.w >= MIN_FLOOR_KEY_WIDTH)
        .filter_map(|r| match &r.kind {
            RegionKind::Key { keys } if keys.len() == 1 => Some(keys[0]),
            _ => None,
        })
        .collect();
    required
        .iter()
        .copied()
        .filter(|k| !reachable.contains(k))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::*;
    use crate::proto::{Layer, Rect};

    fn panel() -> PanelInfo {
        PanelInfo {
            width: 2008,
            height: 60,
            format: PixelFormat::Xrgb8888,
            max_brightness: 255,
            model: "Mac14,7".into(),
        }
    }

    fn key(id: u32, x: u32, w: u32, keys: &[u16]) -> Region {
        Region {
            id,
            rect: Rect::new(x, 0, w, 60),
            kind: RegionKind::Key {
                keys: keys.to_vec(),
            },
        }
    }

    fn agent(id: u32, x: u32, w: u32) -> Region {
        Region {
            id,
            rect: Rect::new(x, 0, w, 60),
            kind: RegionKind::Agent,
        }
    }

    /// Esc + F1..F12 laid out like tiny-dfr's function layer.
    fn fkeys_layer() -> Vec<Region> {
        let mut v = vec![key(0, 0, 150, &[KEY_ESC])];
        for (i, k) in F_KEYS.iter().enumerate() {
            v.push(key(i as u32 + 1, 150 + i as u32 * 150, 150, &[*k]));
        }
        v
    }

    fn present(regions: Vec<Region>) -> Present {
        Present {
            serial: 1,
            layer: Layer::Base,
            image: 0,
            pressed: None,
            regions,
            damage: None,
        }
    }

    #[test]
    fn fkeys_layer_is_valid_and_satisfies_floor() {
        let l = fkeys_layer();
        validate_regions(&panel(), &l).unwrap();
        assert!(escape_floor(&[], Some(&l), &ESCAPE_FLOOR).is_empty());
        assert!(escape_floor(&l, Some(&[]), &ESCAPE_FLOOR).is_empty());
    }

    #[test]
    fn floor_reports_missing_keys() {
        let base = vec![key(0, 0, 100, &[KEY_ESC]), agent(1, 100, 1908)];
        let missing = escape_floor(&base, Some(&[]), &ESCAPE_FLOOR);
        assert_eq!(missing, F_KEYS.to_vec());
        // The built-in fn layer always satisfies it.
        assert!(escape_floor(&base, None, &ESCAPE_FLOOR).is_empty());
    }

    #[test]
    fn floor_ignores_combos_and_slivers() {
        let mut l = fkeys_layer();
        l[0] = key(0, 0, 150, &[29, KEY_ESC]); // Ctrl+Esc isn't Esc
        assert_eq!(escape_floor(&l, Some(&[]), &ESCAPE_FLOOR), vec![KEY_ESC]);
        l[0] = key(0, 0, MIN_FLOOR_KEY_WIDTH - 1, &[KEY_ESC]); // too thin to hit
        assert_eq!(escape_floor(&l, Some(&[]), &ESCAPE_FLOOR), vec![KEY_ESC]);
    }

    #[test]
    fn rejects_bad_regions() {
        let p = panel();
        let e = |r: Vec<Region>| validate_present(&p, &present(r)).unwrap_err();
        assert_eq!(
            e(vec![key(1, 2000, 9, &[KEY_ESC])]),
            Invalid::RegionOutOfBounds(1)
        );
        assert_eq!(
            e(vec![key(1, 0, 10, &[KEY_ESC]), agent(1, 10, 10)]),
            Invalid::DuplicateRegionId(1)
        );
        assert_eq!(
            e(vec![key(1, 0, 10, &[KEY_ESC]), agent(2, 9, 10)]),
            Invalid::RegionsOverlap(1, 2)
        );
        assert_eq!(e(vec![key(1, 0, 0, &[KEY_ESC])]), Invalid::EmptyRegion(1));
        assert_eq!(e(vec![key(1, 0, 10, &[])]), Invalid::NoKeys(1));
        assert_eq!(e(vec![key(1, 0, 10, &[0])]), Invalid::BadKey(0));
        assert_eq!(
            e(vec![key(1, 0, 10, &[KEY_MAX + 1])]),
            Invalid::BadKey(KEY_MAX + 1)
        );
        assert_eq!(
            e(vec![key(1, 0, 10, &[56, KEY_SYSRQ])]),
            Invalid::ForbiddenKey(KEY_SYSRQ)
        );
        assert_eq!(e(vec![key(1, 0, 10, &[30; 9])]), Invalid::TooManyKeys(9));
        let many: Vec<Region> = (0..=MAX_REGIONS as u32)
            .map(|i| agent(i, i * 10, 10))
            .collect();
        assert_eq!(e(many), Invalid::TooManyRegions(MAX_REGIONS + 1));
        // Huge rects must not overflow.
        let r = Region {
            id: 1,
            rect: Rect::new(u32::MAX, 0, u32::MAX, 60),
            kind: RegionKind::Agent,
        };
        assert_eq!(e(vec![r]), Invalid::RegionOutOfBounds(1));
    }

    #[test]
    fn damage_bounds() {
        let p = panel();
        let mut f = present(vec![]);
        f.damage = Some(vec![[0, 2008]]);
        validate_present(&p, &f).unwrap();
        f.damage = Some(vec![[2000, 9]]);
        assert_eq!(validate_present(&p, &f), Err(Invalid::BadDamage));
        f.damage = Some(vec![[u32::MAX, u32::MAX]]);
        assert_eq!(validate_present(&p, &f), Err(Invalid::BadDamage));
    }

    #[test]
    fn buffers() {
        let p = panel();
        let x = PixelFormat::Xrgb8888;
        validate_buffer(&p, 0, 2008, 60, 8032, x).unwrap();
        validate_buffer(&p, 0, 2008, 60, 8192, x).unwrap();
        assert!(validate_buffer(&p, 0, 60, 2008, 240, x).is_err());
        assert!(validate_buffer(&p, 0, 2008, 60, 8000, x).is_err());
        assert_eq!(
            validate_buffer(&p, MAX_BUFFERS, 2008, 60, 8032, x),
            Err(Invalid::TooManyBuffers)
        );
        assert_eq!(required_len(8032, 60), 481_920);
    }

    #[test]
    fn emit_keys_refuses_sysrq() {
        assert_eq!(
            validate_keys(&[KEY_SYSRQ]),
            Err(Invalid::ForbiddenKey(KEY_SYSRQ))
        );
        validate_keys(&[29, 46]).unwrap();
    }
}
