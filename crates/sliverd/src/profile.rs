//! Per-model quirks in one table (docs/hardware.md "Touch Bar models"). Everything else is
//! discovered at runtime: geometry from the DRM mode, `max_brightness` from sysfs, the model from
//! DMI. Only the Mac14,7 row has been observed directly; the rest is "believed".

use sliver_ipc::keys;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    T1,
    T2,
    AppleSilicon,
    Unknown,
}

/// What sliverd needs to know about the machine beyond what it can measure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceProfile {
    pub family: Family,
    /// Whether the machine has a physical Esc key. Without one, Esc leads the built-in layers.
    pub physical_esc: bool,
    /// Backlight while active, in device units.
    pub active_brightness: u32,
    /// Backlight while dimmed, in device units.
    pub dim_brightness: u32,
    pub max_brightness: u32,
}

/// Model prefix (DMI product name) → family.
const MODELS: &[(&str, Family)] = &[
    ("Mac14,7", Family::AppleSilicon),
    ("MacBookPro17,1", Family::AppleSilicon),
    ("MacBookPro15,", Family::T2),
    ("MacBookPro16,", Family::T2),
    ("MacBookPro13,", Family::T1),
    ("MacBookPro14,", Family::T1),
];

impl DeviceProfile {
    /// `width` is the logical bar length, `max_brightness` the bar backlight's. `physical_esc`
    /// overrides tiny-dfr's length heuristic (`/etc/sliver/sliverd.toml`).
    pub fn detect(
        model: &str,
        width: u32,
        max_brightness: u32,
        physical_esc: Option<bool>,
    ) -> DeviceProfile {
        let family = MODELS
            .iter()
            .find(|(prefix, _)| model.starts_with(prefix))
            .map_or(Family::Unknown, |(_, f)| *f);
        let physical_esc = physical_esc.unwrap_or(width < keys::NO_PHYSICAL_ESC_MIN_WIDTH);
        // tiny-dfr: ActiveBrightness 128 of 255, dim 1. A 3-step T2 backlight (off/dim/full) is
        // simply full when active.
        let (active_brightness, dim_brightness) = match max_brightness {
            0 => (0, 0),
            1..=2 => (max_brightness, 1),
            m => ((m * 128).div_ceil(255), 1),
        };
        DeviceProfile {
            family,
            physical_esc,
            active_brightness,
            dim_brightness,
            max_brightness,
        }
    }

    /// Keys of the built-in safe layer (invariant 1): F1–F12, Esc first without a physical Esc.
    pub fn safe_keys(&self) -> &'static [u16] {
        if self.physical_esc {
            &keys::F_KEYS
        } else {
            &keys::SAFE_KEYS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models() {
        let m2 = DeviceProfile::detect("Mac14,7", 2008, 255, None);
        assert_eq!(m2.family, Family::AppleSilicon);
        assert!(m2.physical_esc);
        assert_eq!((m2.active_brightness, m2.dim_brightness), (128, 1));
        assert_eq!(m2.safe_keys(), &keys::F_KEYS);

        let t2 = DeviceProfile::detect("MacBookPro15,1", 2170, 2, None);
        assert_eq!(t2.family, Family::T2);
        assert!(!t2.physical_esc);
        assert_eq!((t2.active_brightness, t2.dim_brightness), (2, 1));
        assert_eq!(t2.safe_keys(), &keys::SAFE_KEYS);

        let over = DeviceProfile::detect("Mac14,7", 2008, 255, Some(false));
        assert_eq!(over.safe_keys(), &keys::SAFE_KEYS);
        assert_eq!(
            DeviceProfile::detect("Weird1,1", 1000, 100, None).family,
            Family::Unknown
        );
    }
}
