//! Layout-aware key combos (T104).
//!
//! Config key names are Linux keycodes, i.e. *positions* on a US keyboard (tiny-dfr's semantics).
//! For F-keys, media keys, modifiers and navigation that's exactly right. For character keys it's
//! a trap: on a French AZERTY keymap, `{ keys = ["LeftCtrl", "Z"] }` sends the key in the US "z"
//! position, which types "w", so "undo" becomes Ctrl+W and closes the tab.
//!
//! [`KeyMapper`] fixes that for character keys: it takes the symbol the key has on a US keymap
//! ("z") and finds the key that gives the same symbol, unshifted, in the keymap the compositor
//! applies to sliverd's virtual keyboard (`DesktopState::bar_keymap`). A key whose symbol isn't
//! on the target's base level keeps its position: AZERTY digits, or letters on a Greek layout,
//! where apps match shortcuts by position anyway. Unknown keymap = plain positions.

use std::collections::HashMap;

use sliver_desktop::XkbNames;
use xkbcommon::xkb;

/// evdev keycodes are xkb keycodes minus 8.
const EVDEV_OFFSET: u32 = 8;

/// The keys whose meaning depends on the layout: digits, letters and punctuation of the main
/// block (KEY_1..KEY_EQUAL, KEY_Q..KEY_RIGHTBRACE, KEY_A..KEY_GRAVE, KEY_BACKSLASH..KEY_SLASH,
/// KEY_102ND). Everything else is the same key on every layout.
fn is_character_key(code: u16) -> bool {
    matches!(code, 2..=13 | 16..=27 | 30..=41 | 43..=53 | 86)
}

/// Maps configured (US-position) keycodes to the keycodes that type the same characters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyMapper {
    /// Only the keys that move. Absent = unchanged.
    moved: HashMap<u16, u16>,
}

impl KeyMapper {
    /// Plain positions (unknown keymap, or a US one).
    pub fn positional() -> KeyMapper {
        KeyMapper::default()
    }

    /// For the keymap `names`. Falls back to [`KeyMapper::positional`] if xkb can't compile it.
    pub fn new(names: &XkbNames) -> KeyMapper {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let compile = |rules: &str, model: &str, layout: &str, variant: &str, options: &str| {
            xkb::Keymap::new_from_names(
                &ctx,
                rules,
                model,
                layout,
                variant,
                (!options.is_empty()).then(|| options.to_string()),
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        let (Some(us), Some(target)) = (
            compile("", "", "us", "", ""),
            compile(
                &names.rules,
                &names.model,
                &names.layout,
                &names.variant,
                &names.options,
            ),
        ) else {
            return KeyMapper::positional();
        };
        let group = names.index;
        let base = |map: &xkb::Keymap, code: u32, group: u32| -> Option<xkb::Keysym> {
            map.key_get_syms_by_level(xkb::Keycode::new(code), group, 0)
                .first()
                .copied()
        };
        let (lo, hi) = (target.min_keycode().raw(), target.max_keycode().raw());
        let mut moved = HashMap::new();
        for code in (0u16..256).filter(|c| is_character_key(*c)) {
            let xk = u32::from(code) + EVDEV_OFFSET;
            let Some(want) = base(&us, xk, 0) else {
                continue;
            };
            if base(&target, xk, group) == Some(want) {
                continue; // same key types it: nothing to do
            }
            let found = (lo..=hi)
                .filter(|&k| k >= EVDEV_OFFSET)
                .find(|&k| base(&target, k, group) == Some(want));
            if let Some(k) = found {
                moved.insert(code, (k - EVDEV_OFFSET) as u16);
            }
        }
        KeyMapper { moved }
    }

    /// The keycode to send for configured `code`.
    pub fn map(&self, code: u16) -> u16 {
        self.moved.get(&code).copied().unwrap_or(code)
    }

    pub fn map_all(&self, codes: &[u16]) -> Vec<u16> {
        codes.iter().map(|&c| self.map(c)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_1: u16 = 2;
    const KEY_Q: u16 = 16;
    const KEY_W: u16 = 17;
    const KEY_Y: u16 = 21;
    const KEY_A: u16 = 30;
    const KEY_Z: u16 = 44;
    const KEY_C: u16 = 46;
    const KEY_LEFTCTRL: u16 = 29;
    const KEY_F5: u16 = 63;
    const KEY_VOLUMEUP: u16 = 115;

    fn names(layout: &str, variant: &str, index: u32) -> XkbNames {
        XkbNames {
            layout: layout.into(),
            variant: variant.into(),
            index,
            ..Default::default()
        }
    }

    #[test]
    fn us_moves_nothing() {
        assert_eq!(KeyMapper::new(&names("us", "", 0)), KeyMapper::positional());
    }

    #[test]
    fn azerty_undo_stays_undo() {
        let m = KeyMapper::new(&names("fr", "", 0));
        // "z" is on the US "w" key on AZERTY, "a" and "q" swap with each other too.
        assert_eq!(m.map(KEY_Z), KEY_W, "Ctrl+Z must not become Ctrl+W");
        assert_eq!(m.map(KEY_W), KEY_Z);
        assert_eq!(m.map(KEY_A), KEY_Q);
        assert_eq!(m.map(KEY_Q), KEY_A);
        assert_eq!(m.map(KEY_C), KEY_C, "same place");
        assert_eq!(m.map_all(&[KEY_LEFTCTRL, KEY_Z]), vec![KEY_LEFTCTRL, KEY_W]);
        // Digits need Shift on AZERTY: not on the base level, so they keep their position.
        assert_eq!(m.map(KEY_1), KEY_1);
        // Never touched: modifiers, F-keys, media keys.
        for k in [KEY_LEFTCTRL, KEY_F5, KEY_VOLUMEUP] {
            assert_eq!(m.map(k), k);
        }
    }

    #[test]
    fn qwertz_and_the_active_group() {
        // German swaps y and z; only when "de" is the active group.
        let de = KeyMapper::new(&names("us,de", ",", 1));
        assert_eq!(de.map(KEY_Z), KEY_Y);
        assert_eq!(de.map(KEY_Y), KEY_Z);
        let us = KeyMapper::new(&names("us,de", ",", 0));
        assert_eq!(us.map(KEY_Z), KEY_Z);
    }

    #[test]
    fn non_latin_keeps_positions() {
        // Greek has no "z" on the base level: apps match shortcuts by position there.
        let m = KeyMapper::new(&names("gr", "", 0));
        assert_eq!(m.map(KEY_Z), KEY_Z);
        assert_eq!(m.map(KEY_C), KEY_C);
    }

    #[test]
    fn bad_names_fall_back_to_positions() {
        let m = KeyMapper::new(&names("no-such-layout", "", 0));
        assert_eq!(m.map(KEY_Z), KEY_Z);
    }
}
