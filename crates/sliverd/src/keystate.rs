//! Reference-counted key output. Two fingers on the same key, or two combos sharing a modifier,
//! must not release the key while one of them still holds it.

use std::collections::HashMap;

use crate::backend::Keys;

#[derive(Default, Debug)]
pub struct KeyState {
    held: HashMap<u16, u32>,
}

impl KeyState {
    /// Presses `keys` in order, then one `SYN_REPORT`.
    pub fn press(&mut self, keys: &[u16], out: &mut dyn Keys) {
        for &k in keys {
            let n = self.held.entry(k).or_insert(0);
            *n += 1;
            if *n == 1 {
                out.key(k, true);
            }
        }
        out.sync();
    }

    /// Releases `keys` in reverse order (modifiers last), then one `SYN_REPORT`.
    pub fn release(&mut self, keys: &[u16], out: &mut dyn Keys) {
        for &k in keys.iter().rev() {
            if let Some(n) = self.held.get_mut(&k) {
                *n -= 1;
                if *n == 0 {
                    self.held.remove(&k);
                    out.key(k, false);
                }
            }
        }
        out.sync();
    }

    pub fn is_down(&self, key: u16) -> bool {
        self.held.contains_key(&key)
    }

    pub fn any_down(&self) -> bool {
        !self.held.is_empty()
    }
}
