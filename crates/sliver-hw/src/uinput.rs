//! Key output through a uinput virtual keyboard. Creating it injects into the live session, so
//! it refuses outside the harness ([`crate::require_harness`]).
//!
//! Every emittable key is registered up front (KEY_ESC..KEY_MAX minus the BTN_* ranges, so udev
//! doesn't classify us as a mouse/joystick), so config changes never need a new device (tiny-dfr
//! 0.3.7 had that bug).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

use crate::evdev::{EV_KEY, EV_SYN};
use crate::input::UINPUT_NAME;

const KEY_MAX: u16 = 0x2ff;
const BUS_VIRTUAL: u16 = 0x06;

const fn io(nr: u64) -> u64 {
    ((b'U' as u64) << 8) | nr
}
const fn iow(nr: u64, size: usize) -> u64 {
    (1 << 30) | ((size as u64) << 16) | ((b'U' as u64) << 8) | nr
}
const UI_DEV_CREATE: u64 = io(1);
const UI_DEV_DESTROY: u64 = io(2);
const UI_DEV_SETUP: u64 = iow(3, std::mem::size_of::<UinputSetup>());
const UI_SET_EVBIT: u64 = iow(100, 4);
const UI_SET_KEYBIT: u64 = iow(101, 4);

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

/// Key codes we register: everything that's a key, nothing in the BTN_* ranges
/// (0x100..=0x15f misc/mouse/joystick/gamepad/digi, 0x2c0..=0x2ff trigger-happy).
pub fn registered_keys() -> impl Iterator<Item = u16> {
    (1..=KEY_MAX).filter(|&k| !(0x100..=0x15f).contains(&k) && !(0x2c0..=0x2ff).contains(&k))
}

/// One `struct input_event` (64-bit layout), zero timestamp (the kernel fills it in).
pub fn encode_event(ty: u16, code: u16, value: i32) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[16..18].copy_from_slice(&ty.to_ne_bytes());
    b[18..20].copy_from_slice(&code.to_ne_bytes());
    b[20..24].copy_from_slice(&value.to_ne_bytes());
    b
}

pub struct UinputKeys {
    file: File,
    batch: Vec<u8>,
}

fn ioctl(f: &File, req: u64, arg: libc::c_ulong) -> io::Result<()> {
    // SAFETY: uinput ioctls with an integer argument (or none).
    let r = unsafe { libc::ioctl(f.as_raw_fd(), req as _, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl UinputKeys {
    pub fn create() -> io::Result<UinputKeys> {
        crate::require_harness().map_err(io::Error::other)?;
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open("/dev/uinput")?;
        ioctl(&file, UI_SET_EVBIT, EV_KEY.into())?;
        for k in registered_keys() {
            ioctl(&file, UI_SET_KEYBIT, k.into())?;
        }
        let mut setup = UinputSetup {
            bustype: BUS_VIRTUAL,
            vendor: 0,
            product: 0,
            version: 1,
            name: [0; 80],
            ff_effects_max: 0,
        };
        setup.name[..UINPUT_NAME.len()].copy_from_slice(UINPUT_NAME.as_bytes());
        // SAFETY: UI_DEV_SETUP reads one uinput_setup.
        let r = unsafe {
            libc::ioctl(
                file.as_raw_fd(),
                UI_DEV_SETUP as _,
                &setup as *const UinputSetup,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        ioctl(&file, UI_DEV_CREATE, 0)?;
        Ok(UinputKeys {
            file,
            batch: Vec::new(),
        })
    }

    /// Queues a key event. Nothing is visible until [`sync`](Self::sync).
    pub fn key(&mut self, code: u16, down: bool) {
        self.batch
            .extend_from_slice(&encode_event(EV_KEY, code, i32::from(down)));
    }

    /// Writes the queued events plus `SYN_REPORT` in one write.
    pub fn sync(&mut self) -> io::Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        self.batch.extend_from_slice(&encode_event(EV_SYN, 0, 0));
        let r = self.file.write_all(&self.batch);
        self.batch.clear();
        r
    }
}

impl Drop for UinputKeys {
    fn drop(&mut self) {
        let _ = ioctl(&self.file, UI_DEV_DESTROY, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_linux_headers() {
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(UI_DEV_CREATE, 0x5501);
        assert_eq!(UI_DEV_DESTROY, 0x5502);
        assert_eq!(UI_DEV_SETUP, 0x405c_5503);
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
    }

    #[test]
    fn key_set_excludes_buttons() {
        let keys: Vec<u16> = registered_keys().collect();
        assert!(keys.contains(&1)); // KEY_ESC
        assert!(keys.contains(&59)); // KEY_F1
        assert!(keys.contains(&0x1d0)); // KEY_FN
        assert!(!keys.contains(&0x110)); // BTN_LEFT
        assert!(!keys.contains(&0x14a)); // BTN_TOUCH
        assert!(!keys.contains(&0x2c0));
    }

    #[test]
    fn event_encoding() {
        let b = encode_event(EV_KEY, 59, 1);
        assert_eq!(crate::evdev::decode_event(&b), (0, 0, EV_KEY, 59, 1));
    }
}
