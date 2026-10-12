//! Touch → key/agent routing for sliverd (`docs/api.md` §6, "Key-region touch semantics").
//!
//! Lives here, not in sliverd, so the fake daemon behaves exactly like the real one.
//! It's pure state: feed it touches plus the regions currently on screen, and act on the output.
//!
//! - A touch is **captured** by the region it starts in. Later layer swaps don't change it.
//! - Key region: down presses its keys, sliding off releases them, sliding back presses again,
//!   and up releases. Combos are released in reverse order.
//! - Keys are reference-counted across touches. Two fingers on the same key emit one down and one up.
//! - Agent region: every phase is forwarded with the captured region id.

use crate::proto::{Rect, Region, RegionKind, TouchPhase};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Routed {
    /// Emit `EV_KEY code value=1/0` (followed by `SYN_REPORT` once per batch).
    Key { code: u16, down: bool },
    /// Forward as [`crate::DaemonMsg::Touch`].
    Agent {
        region: u32,
        slot: u8,
        phase: TouchPhase,
        x: f32,
        y: f32,
    },
}

#[derive(Debug, Clone)]
enum Capture {
    Key {
        region: u32,
        rect: Rect,
        keys: Vec<u16>,
        pressed: bool,
    },
    Agent {
        region: u32,
    },
}

#[derive(Debug, Default)]
pub struct TouchRouter {
    slots: HashMap<u8, Capture>,
    held: HashMap<u16, u32>,
}

impl TouchRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Route one touch event against the regions currently on screen.
    pub fn touch(
        &mut self,
        regions: &[Region],
        slot: u8,
        phase: TouchPhase,
        x: f32,
        y: f32,
    ) -> Vec<Routed> {
        let mut out = Vec::new();
        match phase {
            TouchPhase::Down => {
                // A stale capture on a reused slot (lost up event) is released first.
                self.end(slot, TouchPhase::Cancel, x, y, &mut out);
                let Some(r) = regions.iter().find(|r| r.rect.contains(x, y)) else {
                    return out;
                };
                let cap = match &r.kind {
                    RegionKind::Key { keys } => {
                        self.press(keys, &mut out);
                        Capture::Key {
                            region: r.id,
                            rect: r.rect,
                            keys: keys.clone(),
                            pressed: true,
                        }
                    }
                    RegionKind::Agent => {
                        out.push(Routed::Agent {
                            region: r.id,
                            slot,
                            phase,
                            x,
                            y,
                        });
                        Capture::Agent { region: r.id }
                    }
                };
                self.slots.insert(slot, cap);
            }
            TouchPhase::Move => match self.slots.get(&slot).cloned() {
                Some(Capture::Key {
                    region,
                    rect,
                    keys,
                    pressed,
                }) => {
                    let inside = rect.contains(x, y);
                    if inside != pressed {
                        if inside {
                            self.press(&keys, &mut out);
                        } else {
                            self.release(&keys, &mut out);
                        }
                        self.slots.insert(
                            slot,
                            Capture::Key {
                                region,
                                rect,
                                keys,
                                pressed: inside,
                            },
                        );
                    }
                }
                Some(Capture::Agent { region }) => out.push(Routed::Agent {
                    region,
                    slot,
                    phase,
                    x,
                    y,
                }),
                None => {}
            },
            TouchPhase::Up | TouchPhase::Cancel => self.end(slot, phase, x, y, &mut out),
        }
        out
    }

    /// Release everything (safe layer engaged, agent gone, display off, …).
    pub fn cancel_all(&mut self) -> Vec<Routed> {
        let mut out = Vec::new();
        let mut slots: Vec<u8> = self.slots.keys().copied().collect();
        slots.sort_unstable();
        for s in slots {
            self.end(s, TouchPhase::Cancel, 0.0, 0.0, &mut out);
        }
        out
    }

    /// Key regions currently drawn pressed (to blit from the `pressed` image).
    pub fn pressed_regions(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .slots
            .values()
            .filter_map(|c| match c {
                Capture::Key {
                    region,
                    pressed: true,
                    ..
                } => Some(*region),
                _ => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn end(&mut self, slot: u8, phase: TouchPhase, x: f32, y: f32, out: &mut Vec<Routed>) {
        match self.slots.remove(&slot) {
            Some(Capture::Key {
                keys,
                pressed: true,
                ..
            }) => self.release(&keys, out),
            Some(Capture::Agent { region }) => out.push(Routed::Agent {
                region,
                slot,
                phase,
                x,
                y,
            }),
            _ => {}
        }
    }

    fn press(&mut self, keys: &[u16], out: &mut Vec<Routed>) {
        for &k in keys {
            let n = self.held.entry(k).or_insert(0);
            *n += 1;
            if *n == 1 {
                out.push(Routed::Key {
                    code: k,
                    down: true,
                });
            }
        }
    }

    fn release(&mut self, keys: &[u16], out: &mut Vec<Routed>) {
        for &k in keys.iter().rev() {
            if let Some(n) = self.held.get_mut(&k) {
                *n -= 1;
                if *n == 0 {
                    self.held.remove(&k);
                    out.push(Routed::Key {
                        code: k,
                        down: false,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TouchPhase::*;

    fn regions() -> Vec<Region> {
        vec![
            Region {
                id: 1,
                rect: Rect::new(0, 0, 100, 60),
                kind: RegionKind::Key { keys: vec![1] },
            },
            Region {
                id: 2,
                rect: Rect::new(100, 0, 100, 60),
                kind: RegionKind::Key { keys: vec![29, 46] },
            },
            Region {
                id: 3,
                rect: Rect::new(200, 0, 300, 60),
                kind: RegionKind::Agent,
            },
        ]
    }

    fn k(code: u16, down: bool) -> Routed {
        Routed::Key { code, down }
    }

    #[test]
    fn tap_key() {
        let mut r = TouchRouter::new();
        let rs = regions();
        assert_eq!(r.touch(&rs, 0, Down, 50.0, 30.0), vec![k(1, true)]);
        assert_eq!(r.pressed_regions(), vec![1]);
        assert_eq!(r.touch(&rs, 0, Up, 50.0, 30.0), vec![k(1, false)]);
        assert!(r.pressed_regions().is_empty());
    }

    #[test]
    fn combo_releases_in_reverse() {
        let mut r = TouchRouter::new();
        let rs = regions();
        assert_eq!(
            r.touch(&rs, 0, Down, 150.0, 30.0),
            vec![k(29, true), k(46, true)]
        );
        assert_eq!(
            r.touch(&rs, 0, Up, 150.0, 30.0),
            vec![k(46, false), k(29, false)]
        );
    }

    #[test]
    fn slide_off_and_back_without_pressing_neighbours() {
        let mut r = TouchRouter::new();
        let rs = regions();
        r.touch(&rs, 0, Down, 50.0, 30.0);
        assert_eq!(r.touch(&rs, 0, Move, 150.0, 30.0), vec![k(1, false)]); // onto region 2: nothing pressed
        assert_eq!(r.touch(&rs, 0, Move, 160.0, 30.0), vec![]);
        assert_eq!(r.touch(&rs, 0, Move, 90.0, 30.0), vec![k(1, true)]);
        assert_eq!(r.touch(&rs, 0, Up, 90.0, 30.0), vec![k(1, false)]);
    }

    #[test]
    fn agent_capture_follows_the_finger() {
        let mut r = TouchRouter::new();
        let rs = regions();
        let a = |phase, x| Routed::Agent {
            region: 3,
            slot: 1,
            phase,
            x,
            y: 30.0,
        };
        assert_eq!(r.touch(&rs, 1, Down, 250.0, 30.0), vec![a(Down, 250.0)]);
        assert_eq!(r.touch(&rs, 1, Move, 50.0, 30.0), vec![a(Move, 50.0)]); // over a key: still agent's
        assert_eq!(r.touch(&rs, 1, Up, 50.0, 30.0), vec![a(Up, 50.0)]);
        assert_eq!(r.touch(&rs, 1, Move, 250.0, 30.0), vec![]);
    }

    #[test]
    fn multitouch_refcounts_keys() {
        let mut r = TouchRouter::new();
        let rs = regions();
        assert_eq!(r.touch(&rs, 0, Down, 10.0, 30.0), vec![k(1, true)]);
        assert_eq!(r.touch(&rs, 1, Down, 90.0, 30.0), vec![]);
        assert_eq!(r.touch(&rs, 0, Up, 10.0, 30.0), vec![]);
        assert_eq!(r.touch(&rs, 1, Up, 90.0, 30.0), vec![k(1, false)]);
    }

    #[test]
    fn capture_survives_layer_swap_and_cancel_all_releases() {
        let mut r = TouchRouter::new();
        let rs = regions();
        r.touch(&rs, 0, Down, 150.0, 30.0);
        r.touch(&rs, 1, Down, 300.0, 30.0);
        // Fn pressed: a different layer is on screen now. The captured key still releases correctly.
        let fn_layer = vec![Region {
            id: 9,
            rect: Rect::new(0, 0, 2008, 60),
            kind: RegionKind::Agent,
        }];
        assert_eq!(r.touch(&fn_layer, 0, Move, 151.0, 30.0), vec![]);
        let out = r.cancel_all();
        assert_eq!(
            out,
            vec![
                k(46, false),
                k(29, false),
                Routed::Agent {
                    region: 3,
                    slot: 1,
                    phase: Cancel,
                    x: 0.0,
                    y: 0.0
                }
            ]
        );
        assert!(r.cancel_all().is_empty());
    }

    #[test]
    fn touches_outside_regions_are_ignored() {
        let mut r = TouchRouter::new();
        assert!(r.touch(&regions(), 0, Down, 1000.0, 30.0).is_empty());
        assert!(r.touch(&regions(), 0, Up, 1000.0, 30.0).is_empty());
    }
}
