//! Bar backlight (write) + main display backlight (read, for adaptive brightness), via sysfs.
//!
//! The bar's `brightness` file is root-only, so in production and under the harness systemd
//! opens it and passes the fd (`OpenFile=...:backlight`, see docs/safe-swap.md). Then sliverd
//! never needs root. `max_brightness` and the main display's values are world-readable.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::discover::{self, Backlight as Found};

fn read_u32(p: &Path) -> Option<u32> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

pub struct SysfsBacklight {
    file: File,
    max: u32,
    last: Option<u32>,
    display: Option<Found>,
}

impl SysfsBacklight {
    /// Bar backlight from the systemd-passed fd named `backlight` if present, else by opening
    /// the sysfs file (needs root). Discovery is the same as tiny-dfr's (docs/hardware.md).
    pub fn open() -> io::Result<SysfsBacklight> {
        let sys = Path::new("/sys/class/backlight");
        let bar = discover::bar_backlight(sys)
            .ok_or_else(|| io::Error::other("no Touch Bar backlight"))?;
        let file = match crate::listen_fd("backlight") {
            // SAFETY: systemd handed us this fd; we take sole ownership of it here.
            Some(fd) => unsafe { File::from_raw_fd(fd) },
            None => OpenOptions::new().write(true).open(bar.brightness_path())?,
        };
        Ok(Self::from_parts(
            file,
            bar.max,
            discover::display_backlight(sys),
        ))
    }

    pub fn from_parts(file: File, max: u32, display: Option<Found>) -> SysfsBacklight {
        SysfsBacklight {
            file,
            max,
            last: None,
            display,
        }
    }

    pub fn max(&self) -> u32 {
        self.max
    }

    /// Writes `value` (clamped to `max`), skipping redundant writes.
    pub fn set(&mut self, value: u32) -> io::Result<()> {
        let v = value.min(self.max);
        if self.last == Some(v) {
            return Ok(());
        }
        // Always at offset 0: sysfs attributes are written whole.
        self.file.write_at(format!("{v}\n").as_bytes(), 0)?;
        self.last = Some(v);
        Ok(())
    }

    /// Main display brightness / its own max (tiny-dfr wrongly hardcodes 509).
    pub fn screen_fraction(&self) -> Option<f32> {
        let d = self.display.as_ref()?;
        let cur = read_u32(&d.brightness_path())?;
        Some((cur as f32 / d.max.max(1) as f32).clamp(0.0, 1.0))
    }

    pub fn display_path(&self) -> Option<PathBuf> {
        self.display.as_ref().map(|d| d.sysfs.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_clamped_dedup_and_reads_fraction() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("bl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("panel")).unwrap();
        fs::write(d.join("panel/brightness"), "127\n").unwrap();
        let bar = d.join("bar_brightness");
        fs::write(&bar, "").unwrap();
        let file = OpenOptions::new().write(true).open(&bar).unwrap();
        let disp = Found {
            sysfs: d.join("panel"),
            max: 509,
            current: None,
        };
        let mut bl = SysfsBacklight::from_parts(file, 255, Some(disp));
        bl.set(300).unwrap();
        assert_eq!(fs::read_to_string(&bar).unwrap(), "255\n");
        bl.set(7).unwrap();
        assert_eq!(fs::read_to_string(&bar).unwrap(), "7\n5\n"); // offset 0 write over "255\n"
        let f = bl.screen_fraction().unwrap();
        assert!((f - 127.0 / 509.0).abs() < 1e-6);
        fs::remove_dir_all(d).unwrap();
    }
}
