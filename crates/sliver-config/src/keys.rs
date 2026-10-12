//! Key names: `input_linux::Key` variant names (what tiny-dfr accepts), case-insensitive.

use std::fmt;
use std::str::FromStr;

use crate::keys_table::KEYS;

/// A Linux key, as written in config (`F1`, `BrightnessDown`, `LeftCtrl`…).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    code: u16,
    name: &'static str,
}

impl Key {
    /// The Linux input keycode (`KEY_*` from `linux/input-event-codes.h`).
    pub fn code(self) -> u16 {
        self.code
    }

    /// The canonical spelling (`input_linux::Key` variant name).
    pub fn name(self) -> &'static str {
        self.name
    }

    /// Looks a key up by name, ignoring case and `_`/`-`. Also accepts kernel names such as
    /// `KEY_ESC` or `kbdillumup`.
    pub fn from_name(s: &str) -> Option<Key> {
        let norm = normalize(s.strip_prefix("KEY_").unwrap_or(s));
        let hit = KEYS
            .iter()
            .find(|(n, _)| normalize(n) == norm)
            .or_else(|| kernel_alias(&norm).and_then(|c| KEYS.iter().find(|(_, code)| *code == c)));
        hit.map(|&(name, code)| Key { code, name })
    }

    pub fn from_code(code: u16) -> Option<Key> {
        KEYS.iter()
            .find(|(_, c)| *c == code)
            .map(|&(name, code)| Key { code, name })
    }

    /// `F1`..=`F24` → Some(n).
    pub fn function_number(self) -> Option<u8> {
        let n: u8 = self.name.strip_prefix('F')?.parse().ok()?;
        (1..=24).contains(&n).then_some(n)
    }

    /// F1..=F24 for `n` in 1..=24.
    pub fn function(n: u8) -> Option<Key> {
        Key::from_name(&format!("F{n}"))
    }

    pub fn esc() -> Key {
        Key::from_name("Esc").expect("Esc is in the key table")
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Kernel names that `input_linux` renamed (e.g. `KEY_KBDILLUMUP` is `IllumUp`).
fn kernel_alias(norm: &str) -> Option<u16> {
    Some(match norm {
        "kbdillumtoggle" => 228,
        "kbdillumdown" => 229,
        "kbdillumup" => 230,
        "escape" => 1,
        _ => return None,
    })
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

impl FromStr for Key {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Key::from_name(s).ok_or_else(|| {
            format!(
                "unknown key name {s:?} (use input_linux::Key names like F1, Esc, BrightnessDown)"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_dfr_names_resolve() {
        for (name, code) in [
            ("F1", 59),
            ("F12", 88),
            ("Esc", 1),
            ("BrightnessDown", 224),
            ("BrightnessUp", 225),
            ("MicMute", 248),
            ("Search", 217),
            ("IllumDown", 229),
            ("IllumUp", 230),
            ("PreviousSong", 165),
            ("PlayPause", 164),
            ("NextSong", 163),
            ("Mute", 113),
            ("VolumeDown", 114),
            ("VolumeUp", 115),
            ("LeftCtrl", 29),
            ("LeftAlt", 56),
        ] {
            assert_eq!(Key::from_name(name).map(Key::code), Some(code), "{name}");
        }
    }

    #[test]
    fn case_and_kernel_spellings() {
        assert_eq!(Key::from_name("esc"), Key::from_name("Esc"));
        assert_eq!(Key::from_name("KEY_ESC"), Key::from_name("Esc"));
        assert_eq!(
            Key::from_name("kbd-illum-up").map(Key::name),
            Some("IllumUp")
        );
        assert_eq!(Key::from_name("left_ctrl").map(Key::name), Some("LeftCtrl"));
        assert!(Key::from_name("NotAKey").is_none());
    }

    #[test]
    fn function_keys() {
        assert_eq!(Key::function(7).and_then(Key::function_number), Some(7));
        assert_eq!(Key::function(24).map(Key::code), Some(194));
        assert_eq!(Key::esc().function_number(), None);
    }
}
