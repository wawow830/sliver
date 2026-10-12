//! Linux evdev keycodes (`linux/input-event-codes.h`) that the protocol treats specially.

pub const KEY_ESC: u16 = 1;
pub const KEY_F1: u16 = 59;
pub const KEY_F2: u16 = 60;
pub const KEY_F3: u16 = 61;
pub const KEY_F4: u16 = 62;
pub const KEY_F5: u16 = 63;
pub const KEY_F6: u16 = 64;
pub const KEY_F7: u16 = 65;
pub const KEY_F8: u16 = 66;
pub const KEY_F9: u16 = 67;
pub const KEY_F10: u16 = 68;
pub const KEY_F11: u16 = 87;
pub const KEY_F12: u16 = 88;
/// Never emitted by sliverd: a same-uid process must not get kernel magic SysRq.
pub const KEY_SYSRQ: u16 = 99;
pub const KEY_FN: u16 = 464;
/// Highest valid keycode (`KEY_MAX`).
pub const KEY_MAX: u16 = 0x2ff;

pub const F_KEYS: [u16; 12] = [
    KEY_F1, KEY_F2, KEY_F3, KEY_F4, KEY_F5, KEY_F6, KEY_F7, KEY_F8, KEY_F9, KEY_F10, KEY_F11,
    KEY_F12,
];

/// The built-in safe layer's keys on a bar *without* a physical Esc: Esc + F1–F12.
pub const SAFE_KEYS: [u16; 13] = [
    KEY_ESC, KEY_F1, KEY_F2, KEY_F3, KEY_F4, KEY_F5, KEY_F6, KEY_F7, KEY_F8, KEY_F9, KEY_F10,
    KEY_F11, KEY_F12,
];

/// Bars at least this long (2016–19 MacBook Pros) have no physical Esc (tiny-dfr's heuristic).
pub const NO_PHYSICAL_ESC_MIN_WIDTH: u32 = 2170;

/// Keys of the built-in safe layer (shown by the hatch and when no agent drives the bar) for a
/// panel of this logical width: F1–F12, with Esc first when the machine has no physical Esc.
/// Config validators use the same list to *warn* when a config drops them.
pub fn safe_keys_for_panel(width: u32) -> &'static [u16] {
    if width >= NO_PHYSICAL_ESC_MIN_WIDTH {
        &SAFE_KEYS
    } else {
        &F_KEYS
    }
}

/// Keys sliverd refuses to emit, in regions and in `emit_keys`.
pub const FORBIDDEN: [u16; 1] = [KEY_SYSRQ];

/// Whether `code` is a key sliverd may emit (in range, not 0/`KEY_RESERVED`, not forbidden).
pub fn emittable(code: u16) -> bool {
    code != 0 && code <= KEY_MAX && !FORBIDDEN.contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_keys_depend_on_physical_esc() {
        assert_eq!(safe_keys_for_panel(2008), &F_KEYS); // Mac14,7: physical Esc (H2)
        assert_eq!(safe_keys_for_panel(2170), &SAFE_KEYS);
        assert!(!emittable(KEY_SYSRQ) && !emittable(0) && emittable(KEY_ESC));
    }
}
