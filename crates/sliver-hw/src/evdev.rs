//! Minimal evdev ioctls (no grab, read-only). Only for use under the harness.

use std::os::fd::RawFd;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const BTN_TOUCH: u16 = 0x14a;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_TOUCH_MAJOR: u16 = 0x30;
pub const ABS_MT_TOUCH_MINOR: u16 = 0x31;
pub const ABS_MT_WIDTH_MAJOR: u16 = 0x32;
pub const ABS_MT_ORIENTATION: u16 = 0x34;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;

/// The axes worth reporting for the bar digitizer.
pub const PROBE_AXES: &[(u16, &str)] = &[
    (ABS_X, "ABS_X"),
    (ABS_Y, "ABS_Y"),
    (ABS_MT_SLOT, "MT_SLOT"),
    (ABS_MT_TOUCH_MAJOR, "MT_TOUCH_MAJOR"),
    (ABS_MT_TOUCH_MINOR, "MT_TOUCH_MINOR"),
    (ABS_MT_WIDTH_MAJOR, "MT_WIDTH_MAJOR"),
    (ABS_MT_ORIENTATION, "MT_ORIENTATION"),
    (ABS_MT_POSITION_X, "MT_POSITION_X"),
    (ABS_MT_POSITION_Y, "MT_POSITION_Y"),
    (ABS_MT_TRACKING_ID, "MT_TRACKING_ID"),
];

/// `struct input_absinfo`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

const fn ioc(dir: u64, ty: u8, nr: u8, size: usize) -> u64 {
    (dir << 30) | ((size as u64) << 16) | ((ty as u64) << 8) | nr as u64
}

/// `EVIOCGABS(abs)` = `_IOR('E', 0x40 + abs, struct input_absinfo)`.
pub const fn eviocgabs(abs: u16) -> u64 {
    ioc(2, b'E', 0x40 + abs as u8, std::mem::size_of::<AbsInfo>())
}

/// `EVIOCGNAME(len)` = `_IOC(_IOC_READ, 'E', 0x06, len)`.
pub const fn eviocgname(len: usize) -> u64 {
    ioc(2, b'E', 0x06, len)
}

pub fn abs_info(fd: RawFd, abs: u16) -> std::io::Result<AbsInfo> {
    let mut info = AbsInfo::default();
    // SAFETY: EVIOCGABS writes exactly one input_absinfo into the pointer we pass.
    let r = unsafe { libc::ioctl(fd, eviocgabs(abs) as _, &mut info as *mut AbsInfo) };
    if r < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

/// `EVIOCGMTSLOTS(len)` = `_IOC(_IOC_READ, 'E', 0x0a, len)`.
pub const fn eviocgmtslots(len: usize) -> u64 {
    ioc(2, b'E', 0x0a, len)
}

/// Current per-slot values of one MT axis (`code`), for `n` slots.
pub fn mt_slots(fd: RawFd, code: u16, n: usize) -> std::io::Result<Vec<i32>> {
    // struct input_mt_request_layout { __u32 code; __s32 values[n]; }
    let mut buf = vec![0i32; n + 1];
    buf[0] = i32::from(code);
    let len = buf.len() * 4;
    // SAFETY: the kernel writes at most `len` bytes into `buf`.
    let r = unsafe { libc::ioctl(fd, eviocgmtslots(len) as _, buf.as_mut_ptr()) };
    if r < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(buf[1..].to_vec())
    }
}

/// Decode one `struct input_event` (64-bit timeval layout: 24 bytes).
pub fn decode_event(b: &[u8; 24]) -> (i64, i64, u16, u16, i32) {
    let sec = i64::from_ne_bytes(b[0..8].try_into().unwrap());
    let usec = i64::from_ne_bytes(b[8..16].try_into().unwrap());
    let ty = u16::from_ne_bytes(b[16..18].try_into().unwrap());
    let code = u16::from_ne_bytes(b[18..20].try_into().unwrap());
    let value = i32::from_ne_bytes(b[20..24].try_into().unwrap());
    (sec, usec, ty, code, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_linux_headers() {
        assert_eq!(eviocgabs(ABS_X), 0x8018_4540);
        assert_eq!(eviocgabs(ABS_MT_POSITION_X), 0x8018_4575);
        assert_eq!(eviocgname(256), 0x8100_4506);
        assert_eq!(eviocgmtslots(4 * 17), 0x8044_450a);
    }

    #[test]
    fn decodes_event() {
        let mut b = [0u8; 24];
        b[0..8].copy_from_slice(&5i64.to_ne_bytes());
        b[8..16].copy_from_slice(&7i64.to_ne_bytes());
        b[16..18].copy_from_slice(&EV_ABS.to_ne_bytes());
        b[18..20].copy_from_slice(&ABS_MT_POSITION_X.to_ne_bytes());
        b[20..24].copy_from_slice(&(-3i32).to_ne_bytes());
        assert_eq!(decode_event(&b), (5, 7, EV_ABS, ABS_MT_POSITION_X, -3));
    }
}
