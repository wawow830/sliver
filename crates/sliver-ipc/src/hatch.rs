//! The safe hatch (invariant 1): **double-press Fn and hold** and sliverd shows its built-in
//! layer ([`crate::keys::safe_keys_for_panel`]) for as long as Fn stays down, whatever the agent,
//! the config or any app is doing. Not configurable, by design (H4 revised, H14).
//!
//! Pure state, shared by sliverd and the fake so both behave the same.

/// Two Fn presses whose key-downs are at most this far apart count as a double press.
pub const DOUBLE_PRESS_MS: u64 = 400;

#[derive(Debug, Default, Clone)]
pub struct FnHatch {
    last_down_ms: Option<u64>,
    active: bool,
}

impl FnHatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// Feed a physical Fn key event (`time_ms` from a monotonic clock). Returns whether the hatch
    /// is active afterwards.
    pub fn fn_key(&mut self, down: bool, time_ms: u64) -> bool {
        if down {
            let double = self
                .last_down_ms
                .is_some_and(|t| time_ms.saturating_sub(t) <= DOUBLE_PRESS_MS);
            self.active = double;
            // A triple press doesn't chain off the press that opened the hatch.
            self.last_down_ms = if double { None } else { Some(time_ms) };
        } else {
            self.active = false;
        }
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_press_and_hold() {
        let mut h = FnHatch::new();
        assert!(!h.fn_key(true, 1000));
        assert!(!h.fn_key(false, 1100));
        assert!(h.fn_key(true, 1300));
        assert!(h.active());
        assert!(!h.fn_key(false, 5000));
    }

    #[test]
    fn slow_presses_and_triples() {
        let mut h = FnHatch::new();
        h.fn_key(true, 0);
        h.fn_key(false, 100);
        assert!(!h.fn_key(true, 501)); // too slow
        h.fn_key(false, 600);
        assert!(h.fn_key(true, 700)); // double with the previous press
        h.fn_key(false, 800);
        assert!(!h.fn_key(true, 900)); // the third press starts over
        h.fn_key(false, 950);
        assert!(h.fn_key(true, 1000));
    }
}
