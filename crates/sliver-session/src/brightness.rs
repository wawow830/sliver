//! Screen and keyboard backlight: read levels from sysfs, write through logind.
//!
//! sysfs `brightness` is world-readable but root-writable, so writes go through
//! `org.freedesktop.login1.Session.SetBrightness`, which works for the active local session
//! without a prompt. The Touch Bar's own backlight is never reported as the screen (it is
//! sliverd's), see [`is_touch_bar`].

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use zbus::blocking::Connection;

/// Levels in percent of the device's own range (not perceptual).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Levels {
    pub screen_pct: Option<u8>,
    pub keyboard_pct: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// logind subsystem: `backlight` or `leds`.
    pub subsystem: &'static str,
    pub name: String,
    pub max: u32,
}

/// Names of the Touch Bar panel's backlight, as in tiny-dfr's udev rules (docs/hardware.md).
pub fn is_touch_bar(name: &str, driver: Option<&str>) -> bool {
    matches!(
        name,
        "appletb_backlight" | "228200000.display-pipe.0" | "228600000.dsi.0"
    ) || driver == Some("panel-summit")
}

/// Preference order for the main screen, as tiny-dfr uses for its display backlight.
const SCREEN_PREFERENCE: [&str; 4] = [
    "apple-panel-bl",
    "gmux_backlight",
    "intel_backlight",
    "acpi_video0",
];

pub fn to_percent(value: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }
    ((u64::from(value) * 100 + u64::from(max) / 2) / u64::from(max)).min(100) as u8
}

pub fn from_percent(pct: u8, max: u32) -> u32 {
    (u64::from(pct.min(100)) * u64::from(max) / 100) as u32
}

/// A view of `/sys/class` (injectable root for tests).
#[derive(Debug, Clone)]
pub struct Sysfs {
    root: PathBuf,
}

impl Default for Sysfs {
    fn default() -> Self {
        Self::new("/sys/class")
    }
}

impl Sysfs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn read_u32(path: &Path) -> Option<u32> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    fn driver(&self, dev: &Path) -> Option<String> {
        let link = std::fs::read_link(dev.join("device/driver")).ok()?;
        Some(link.file_name()?.to_string_lossy().into_owned())
    }

    /// The main screen's backlight, never the Touch Bar's.
    pub fn screen(&self) -> Option<Device> {
        let dir = self.root.join("backlight");
        let mut cands: Vec<Device> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if is_touch_bar(&name, self.driver(&e.path()).as_deref()) {
                    return None;
                }
                let max = Self::read_u32(&e.path().join("max_brightness"))?;
                Some(Device {
                    subsystem: "backlight",
                    name,
                    max,
                })
            })
            .collect();
        cands.sort_by_key(|d| {
            (
                SCREEN_PREFERENCE
                    .iter()
                    .position(|p| *p == d.name)
                    .unwrap_or(usize::MAX),
                d.name.clone(),
            )
        });
        cands.into_iter().next()
    }

    /// The keyboard backlight LED (`*kbd_backlight`).
    pub fn keyboard(&self) -> Option<Device> {
        let dir = self.root.join("leds");
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with("kbd_backlight"))
            .collect();
        names.sort();
        let name = names.into_iter().next()?;
        let max = Self::read_u32(&dir.join(&name).join("max_brightness"))?;
        Some(Device {
            subsystem: "leds",
            name,
            max,
        })
    }

    pub fn read(&self, d: &Device) -> Option<u8> {
        let p = self.root.join(d.subsystem).join(&d.name).join("brightness");
        Self::read_u32(&p).map(|v| to_percent(v, d.max))
    }

    pub fn levels(&self) -> Levels {
        Levels {
            screen_pct: self.screen().and_then(|d| self.read(&d)),
            keyboard_pct: self.keyboard().and_then(|d| self.read(&d)),
        }
    }
}

/// Set a device to `pct` through logind. 0% on a screen is allowed by the caller's policy;
/// widgets should clamp to a visible minimum themselves.
pub fn set(conn: &Connection, d: &Device, pct: u8) -> zbus::Result<()> {
    conn.call_method(
        Some("org.freedesktop.login1"),
        "/org/freedesktop/login1/session/auto",
        Some("org.freedesktop.login1.Session"),
        "SetBrightness",
        &(d.subsystem, d.name.as_str(), from_percent(pct, d.max)),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn percent_math() {
        assert_eq!(to_percent(509, 509), 100);
        assert_eq!(to_percent(117, 509), 23);
        assert_eq!(to_percent(0, 0), 0);
        assert_eq!(to_percent(999, 255), 100);
        assert_eq!(from_percent(50, 255), 127);
        assert_eq!(from_percent(200, 255), 255);
    }

    #[test]
    fn touch_bar_is_recognised() {
        assert!(is_touch_bar("228600000.dsi.0", None));
        assert!(is_touch_bar("something", Some("panel-summit")));
        assert!(!is_touch_bar("apple-panel-bl", Some("apple-dcp")));
    }

    fn dev(root: &Path, sub: &str, name: &str, max: u32, cur: u32) {
        let d = root.join(sub).join(name);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("max_brightness"), format!("{max}\n")).unwrap();
        fs::write(d.join("brightness"), format!("{cur}\n")).unwrap();
    }

    #[test]
    fn finds_screen_not_touch_bar_and_keyboard() {
        let root = std::env::temp_dir().join(format!("sliver-sysfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        dev(&root, "backlight", "228600000.dsi.0", 255, 62);
        dev(&root, "backlight", "apple-panel-bl", 509, 255);
        dev(&root, "leds", "input1::capslock", 1, 0);
        dev(&root, "leds", "kbd_backlight", 255, 51);
        let s = Sysfs::new(&root);
        assert_eq!(s.screen().unwrap().name, "apple-panel-bl");
        assert_eq!(s.keyboard().unwrap().name, "kbd_backlight");
        assert_eq!(
            s.levels(),
            Levels {
                screen_pct: Some(50),
                keyboard_pct: Some(20)
            }
        );
        // Only the Touch Bar present: there is no "screen".
        fs::remove_dir_all(root.join("backlight/apple-panel-bl")).unwrap();
        assert_eq!(s.screen(), None);
        assert_eq!(Sysfs::new(root.join("nope")).levels(), Levels::default());
        fs::remove_dir_all(&root).unwrap();
    }
}
