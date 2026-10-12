//! Display power: on → dimmed → off after inactivity, off while the lid is closed.
//!
//! Event-driven (invariant 3): the daemon asks for [`Idle::deadline`] and arms one timer for it.
//! While off with the lid closed or no timeouts set, there's no deadline at all, so the daemon
//! sleeps until input arrives. (tiny-dfr polls every 10 s instead.)

use sliver_ipc::curve;
use sliver_ipc::proto::{DisplayPolicy, DisplayState};

/// tiny-dfr's timeouts.
pub const DEFAULT_DIM_AFTER_MS: u64 = 30_000;
pub const DEFAULT_OFF_AFTER_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Idle {
    max: u32,
    active: u32,
    dim: u32,
    /// 0 = never.
    dim_after_ms: u64,
    /// 0 = never.
    off_after_ms: u64,
    follow_screen: bool,
    /// Normalised adaptive curve ([`sliver_ipc::curve`]). Empty = tiny-dfr's sqrt.
    curve: Vec<[u16; 2]>,
    state: DisplayState,
    last_activity_ms: u64,
    lid_closed: bool,
}

impl Idle {
    pub fn new(max: u32, active: u32, dim: u32, now: u64) -> Idle {
        Idle {
            max,
            active: active.min(max),
            dim: dim.min(max),
            dim_after_ms: DEFAULT_DIM_AFTER_MS,
            off_after_ms: DEFAULT_OFF_AFTER_MS,
            follow_screen: false,
            curve: Vec::new(),
            state: DisplayState::On,
            last_activity_ms: now,
            lid_closed: false,
        }
    }

    pub fn state(&self) -> DisplayState {
        self.state
    }

    pub fn follow_screen(&self) -> bool {
        self.follow_screen
    }

    /// Applies an agent's policy. Missing fields keep their values, brightness is clamped to max.
    pub fn set_policy(&mut self, p: &DisplayPolicy, now: u64) -> DisplayState {
        if let Some(b) = p.brightness {
            self.active = b.min(self.max);
        }
        if let Some(b) = p.dim_brightness {
            self.dim = b.min(self.max);
        }
        if let Some(ms) = p.dim_after_ms {
            self.dim_after_ms = ms;
        }
        if let Some(ms) = p.off_after_ms {
            self.off_after_ms = ms;
        }
        if let Some(f) = p.follow_screen {
            self.follow_screen = f;
        }
        if let Some(c) = &p.curve {
            self.curve = curve::normalise(c);
        }
        self.tick(now)
    }

    /// The user touched something. Returns the new state.
    pub fn activity(&mut self, now: u64) -> DisplayState {
        self.last_activity_ms = now;
        self.tick(now)
    }

    pub fn set_lid(&mut self, closed: bool, now: u64) -> DisplayState {
        self.lid_closed = closed;
        if closed {
            self.tick(now)
        } else {
            self.activity(now) // opening the lid counts as activity
        }
    }

    /// Re-evaluates the state at `now`. Returns it.
    pub fn tick(&mut self, now: u64) -> DisplayState {
        let idle = now.saturating_sub(self.last_activity_ms);
        self.state = if self.lid_closed || (self.off_after_ms > 0 && idle >= self.off_after_ms) {
            DisplayState::Off
        } else if self.dim_after_ms > 0 && idle >= self.dim_after_ms {
            DisplayState::Dimmed
        } else {
            DisplayState::On
        };
        self.state
    }

    /// When the state will next change without input, if ever.
    pub fn deadline(&self) -> Option<u64> {
        if self.lid_closed {
            return None;
        }
        let at = |ms: u64| (ms > 0).then(|| self.last_activity_ms.saturating_add(ms));
        let dim = match self.state {
            DisplayState::On => at(self.dim_after_ms),
            _ => None,
        };
        let off = match self.state {
            DisplayState::Off => None,
            _ => at(self.off_after_ms),
        };
        [dim, off].into_iter().flatten().min()
    }

    /// Backlight value for the current state. `screen` is the main display's brightness fraction,
    /// used when `follow_screen` is on: through the agent's curve, or tiny-dfr's
    /// `sqrt(screen) * active + 1` without one. Never below 1 while on.
    pub fn brightness(&self, screen: Option<f32>) -> u32 {
        match self.state {
            DisplayState::Off => 0,
            DisplayState::Dimmed => self.dim,
            DisplayState::On => match (self.follow_screen, screen) {
                (true, Some(f)) if self.max > 0 => {
                    curve::level(&self.curve, f, self.active, self.max)
                }
                _ => self.active,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adaptive(curve: Vec<[u16; 2]>) -> Idle {
        let mut i = Idle::new(255, 128, 1, 0);
        i.set_policy(
            &DisplayPolicy {
                brightness: Some(200),
                follow_screen: Some(true),
                curve: Some(curve),
                ..Default::default()
            },
            0,
        );
        i
    }

    #[test]
    fn curve_from_policy() {
        // The math is tested in sliver_ipc::curve; here: Idle stores, resets and keeps it.
        let i = adaptive(vec![[1000, 9999], [0, 0], [0, 1000]]);
        assert_eq!(i.curve, vec![[0, 0], [1000, 1000]], "normalised");
        assert_eq!(i.brightness(Some(0.5)), 100);
        assert_eq!(i.brightness(None), 200, "no screen reading: active level");
        let mut i = adaptive(vec![[0, 0], [1000, 1000]]);
        i.set_policy(&DisplayPolicy::default(), 0);
        assert_eq!(i.curve.len(), 2, "a policy without curve keeps it");
        let reset = DisplayPolicy {
            curve: Some(vec![]),
            ..Default::default()
        };
        i.set_policy(&reset, 0);
        assert!(i.curve.is_empty(), "[] resets to sqrt");
        assert_eq!(i.brightness(Some(0.25)), 101);
    }

    #[test]
    fn dims_then_turns_off_then_wakes() {
        let mut i = Idle::new(255, 128, 1, 0);
        assert_eq!(i.deadline(), Some(30_000));
        assert_eq!(i.tick(29_999), DisplayState::On);
        assert_eq!(i.tick(30_000), DisplayState::Dimmed);
        assert_eq!(i.brightness(None), 1);
        assert_eq!(i.deadline(), Some(60_000));
        assert_eq!(i.tick(60_000), DisplayState::Off);
        assert_eq!(i.brightness(None), 0);
        assert_eq!(i.deadline(), None, "no timers while off");
        assert_eq!(i.activity(70_000), DisplayState::On);
        assert_eq!(i.brightness(None), 128);
        assert_eq!(i.deadline(), Some(100_000));
    }

    #[test]
    fn lid_and_policy() {
        let mut i = Idle::new(255, 128, 1, 0);
        assert_eq!(i.set_lid(true, 5), DisplayState::Off);
        assert_eq!(i.deadline(), None);
        assert_eq!(i.set_lid(false, 10), DisplayState::On);
        let p = DisplayPolicy {
            brightness: Some(999),
            dim_after_ms: Some(0),
            off_after_ms: Some(0),
            follow_screen: Some(true),
            ..Default::default()
        };
        i.set_policy(&p, 10);
        assert_eq!(i.deadline(), None, "never dim, never off");
        assert_eq!(i.brightness(None), 255);
        assert_eq!(i.brightness(Some(0.25)), 129); // sqrt(.25) * 255 + 1 = 128.5
        assert_eq!(i.brightness(Some(0.0)), 1);
    }
}
