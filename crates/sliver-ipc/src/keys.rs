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

/// Keys that must always be reachable as single-key regions in `base ∪ fn` (invariant 1).
pub const ESCAPE_FLOOR: [u16; 13] = [
    KEY_ESC, KEY_F1, KEY_F2, KEY_F3, KEY_F4, KEY_F5, KEY_F6, KEY_F7, KEY_F8, KEY_F9, KEY_F10,
    KEY_F11, KEY_F12,
];

/// Keys sliverd refuses to emit, in regions and in `emit_keys`.
pub const FORBIDDEN: [u16; 1] = [KEY_SYSRQ];

/// Whether `code` is a key sliverd may emit (in range, not 0/`KEY_RESERVED`, not forbidden).
pub fn emittable(code: u16) -> bool {
    code != 0 && code <= KEY_MAX && !FORBIDDEN.contains(&code)
}
