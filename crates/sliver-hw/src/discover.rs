//! Find the Touch Bar's devices from sysfs. Read-only, safe to run anywhere.
//! Name lists mirror tiny-dfr 0.3.7 so we match the same machines (docs/hardware.md).

use std::fs;
use std::path::{Path, PathBuf};

/// DRM drivers that drive a Touch Bar panel: Apple Silicon display pipe, T2 USB display.
pub const BAR_DRM_DRIVERS: &[&str] = &["adp", "appletbdrm"];
/// Substrings of the bar backlight's sysfs name (AS panel-summit variants, T2 hid-appletb-bl).
pub const BAR_BACKLIGHT_NAMES: &[&str] = &["display-pipe", "228600000.dsi.0", "appletb_backlight"];
/// Main panel backlights (for adaptive brightness).
pub const DISPLAY_BACKLIGHT_NAMES: &[&str] = &[
    "apple-panel-bl",
    "gmux_backlight",
    "intel_backlight",
    "acpi_video0",
];
/// Touch digitizer names: "Mac14,7 Touch Bar", "MacBookPro17,1 Touch Bar",
/// "Apple Inc. Touch Bar Display Touchpad" (T2). tiny-dfr matches " Touch Bar".
pub const TOUCH_NAME_NEEDLE: &str = " Touch Bar";
/// The digitizers' exact names (packaging/udev/99-z-sliverd.rules and tiny-dfr's seat rules).
pub const TOUCH_NAMES: &[&str] = &[
    "Apple Inc. Touch Bar Display Touchpad",
    "MacBookPro17,1 Touch Bar",
    "Mac14,7 Touch Bar",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlight {
    pub sysfs: PathBuf,
    pub max: u32,
    pub current: Option<u32>,
}

impl Backlight {
    pub fn brightness_path(&self) -> PathBuf {
        self.sysfs.join("brightness")
    }
}

fn read_u32(p: &Path) -> Option<u32> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// `/dev/dri/cardN` of the bar, by DRM driver. `sys_drm` is normally `/sys/class/drm`.
pub fn bar_card(sys_drm: &Path) -> Option<PathBuf> {
    sorted_entries(sys_drm).into_iter().find_map(|e| {
        let name = e.file_name()?.to_str()?.to_owned();
        if !name.starts_with("card") || name.contains('-') {
            return None;
        }
        let drv = fs::read_link(e.join("device/driver")).ok()?;
        let drv = drv.file_name()?.to_str()?;
        BAR_DRM_DRIVERS
            .contains(&drv)
            .then(|| PathBuf::from("/dev/dri").join(name))
    })
}

fn backlight_matching(sys_bl: &Path, names: &[&str]) -> Option<Backlight> {
    sorted_entries(sys_bl).into_iter().find_map(|e| {
        let name = e.file_name()?.to_str()?;
        if !names.iter().any(|n| name.contains(n)) {
            return None;
        }
        Some(Backlight {
            max: read_u32(&e.join("max_brightness"))?,
            current: read_u32(&e.join("brightness")),
            sysfs: e,
        })
    })
}

/// The bar's backlight. `sys_bl` is normally `/sys/class/backlight`.
/// Note T2's `appletb_backlight` has max 2 (off/dim/full).
pub fn bar_backlight(sys_bl: &Path) -> Option<Backlight> {
    backlight_matching(sys_bl, BAR_BACKLIGHT_NAMES)
}

/// The main panel's backlight, if any.
pub fn display_backlight(sys_bl: &Path) -> Option<Backlight> {
    backlight_matching(sys_bl, DISPLAY_BACKLIGHT_NAMES)
}

/// `(/dev/input/eventN, name)` of the bar digitizer. `sys_input` is normally `/sys/class/input`.
pub fn touch_device(sys_input: &Path) -> Option<(PathBuf, String)> {
    let all: Vec<(PathBuf, String)> = sorted_entries(sys_input)
        .into_iter()
        .filter_map(|e| {
            let ev = e.file_name()?.to_str()?.to_owned();
            if !ev.starts_with("event") {
                return None;
            }
            let name = fs::read_to_string(e.join("device/name")).ok()?;
            Some((
                PathBuf::from("/dev/input").join(&ev),
                name.trim().to_owned(),
            ))
        })
        .collect();
    // Exact known names first (the ones udev puts on seat-touchbar), so a sibling interface whose
    // name merely contains "Touch Bar" can't win; then tiny-dfr's substring, for unknown models.
    let exact = all.iter().find(|(_, n)| TOUCH_NAMES.contains(&n.as_str()));
    exact
        .or_else(|| all.iter().find(|(_, n)| n.contains(TOUCH_NAME_NEEDLE)))
        .cloned()
}

/// Connector modes as listed in sysfs, e.g. `["60x2008"]`, without opening the card.
pub fn connector_modes(sys_drm: &Path, card: &str) -> Vec<(String, String)> {
    sorted_entries(sys_drm)
        .into_iter()
        .filter_map(|e| {
            let n = e.file_name()?.to_str()?.to_owned();
            let conn = n.strip_prefix(card)?.strip_prefix('-')?.to_owned();
            let modes = fs::read_to_string(e.join("modes")).unwrap_or_default();
            Some((conn, modes.split_whitespace().collect::<Vec<_>>().join(",")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn tmp(tag: &str) -> PathBuf {
        // Under target/, not /tmp: /tmp is a quota'd tmpfs on the dev machine.
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("sliver-hw-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_card_by_driver() {
        let d = tmp("card");
        for (card, drv) in [("card2", "asahi"), ("card1", "adp"), ("card3", "apple-drm")] {
            fs::create_dir_all(d.join("drivers").join(drv)).unwrap();
            fs::create_dir_all(d.join("class").join(card).join("device")).unwrap();
            symlink(
                d.join("drivers").join(drv),
                d.join("class").join(card).join("device/driver"),
            )
            .unwrap();
        }
        fs::create_dir_all(d.join("class/card1-DSI-1")).unwrap();
        fs::write(d.join("class/card1-DSI-1/modes"), "60x2008\n").unwrap();
        assert_eq!(
            bar_card(&d.join("class")),
            Some(PathBuf::from("/dev/dri/card1"))
        );
        assert_eq!(
            connector_modes(&d.join("class"), "card1"),
            vec![("DSI-1".to_owned(), "60x2008".to_owned())]
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn finds_backlights() {
        let d = tmp("bl");
        for (n, max, cur) in [
            ("apple-panel-bl", "509", "117"),
            ("228600000.dsi.0", "255", "62"),
        ] {
            fs::create_dir_all(d.join(n)).unwrap();
            fs::write(d.join(n).join("max_brightness"), max).unwrap();
            fs::write(d.join(n).join("brightness"), cur).unwrap();
        }
        let bar = bar_backlight(&d).unwrap();
        assert_eq!((bar.max, bar.current), (255, Some(62)));
        assert_eq!(display_backlight(&d).unwrap().max, 509);
        fs::remove_dir_all(d).unwrap();

        let d = tmp("bl-t2");
        fs::create_dir_all(d.join("appletb_backlight")).unwrap();
        fs::write(d.join("appletb_backlight/max_brightness"), "2\n").unwrap();
        assert_eq!(bar_backlight(&d).unwrap().max, 2);
        assert!(display_backlight(&d).is_none());
        fs::remove_dir_all(d).unwrap();
    }

    /// T96: an Intel T2 MacBook Pro has the main GPU (i915, sometimes amdgpu too) plus the bar's
    /// USB display (appletbdrm). Only the latter is the bar.
    #[test]
    fn finds_t2_card_and_digitizer() {
        let d = tmp("t2");
        for (card, drv) in [
            ("card0", "i915"),
            ("card1", "amdgpu"),
            ("card2", "appletbdrm"),
        ] {
            fs::create_dir_all(d.join("drivers").join(drv)).unwrap();
            fs::create_dir_all(d.join("class").join(card).join("device")).unwrap();
            symlink(
                d.join("drivers").join(drv),
                d.join("class").join(card).join("device/driver"),
            )
            .unwrap();
        }
        assert_eq!(
            bar_card(&d.join("class")),
            Some(PathBuf::from("/dev/dri/card2"))
        );
        let inp = d.join("input");
        for (ev, name) in [
            ("event3", "Apple Inc. Apple Internal Keyboard / Trackpad"),
            // A sibling interface that sorts first and also contains "Touch Bar".
            ("event6", "Apple Inc. Touch Bar Display Touchpad Keyboard"),
            ("event7", "Apple Inc. Touch Bar Display Touchpad"),
        ] {
            fs::create_dir_all(inp.join(ev).join("device")).unwrap();
            fs::write(inp.join(ev).join("device/name"), format!("{name}\n")).unwrap();
        }
        assert_eq!(
            touch_device(&inp),
            Some((
                PathBuf::from("/dev/input/event7"),
                "Apple Inc. Touch Bar Display Touchpad".to_owned()
            )),
            "the digitizer, not its keyboard sibling"
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn finds_touch() {
        let d = tmp("touch");
        for (ev, name) in [
            ("event1", "Apple MTP keyboard"),
            ("event2", "Mac14,7 Touch Bar"),
        ] {
            fs::create_dir_all(d.join(ev).join("device")).unwrap();
            fs::write(d.join(ev).join("device/name"), format!("{name}\n")).unwrap();
        }
        assert_eq!(
            touch_device(&d),
            Some((
                PathBuf::from("/dev/input/event2"),
                "Mac14,7 Touch Bar".to_owned()
            ))
        );
        fs::remove_dir_all(d).unwrap();
    }
}
