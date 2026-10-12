//! Pure decoder: Linux multitouch protocol B (evdev `EV_ABS`/`EV_SYN` stream) into contact phases
//! in logical bar pixels. No I/O, so it's unit-tested on recorded sequences (docs/hardware.md has
//! the real device's ranges: X 0..23044 along the bar, Y 0..639, ~60 Hz SYN_REPORTs).

use crate::evdev::{
    ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_SLOT, ABS_MT_TRACKING_ID, EV_ABS, EV_SYN,
};

pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;
/// Slots we track. The Mac14,7 reliably reports 3 contacts. Slot numbers above this are ignored.
pub const MAX_SLOTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
    /// Contact lost without a clean release (SYN_DROPPED, device closed). Don't fire actions.
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub slot: u8,
    pub phase: Phase,
    /// Logical bar pixels: x along the bar (0..len), y across it (0..thickness).
    pub x: f32,
    pub y: f32,
    pub time_us: u64,
}

/// Device axis ranges -> logical bar size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mapping {
    pub x_min: i32,
    pub x_max: i32,
    pub y_min: i32,
    pub y_max: i32,
    /// Logical bar length and thickness in pixels (e.g. 2008 x 60).
    pub len: u32,
    pub thickness: u32,
}

impl Mapping {
    fn map(v: i32, min: i32, max: i32, size: u32) -> f32 {
        let span = (max - min).max(1) as f32;
        (((v - min) as f32 / span) * size as f32).clamp(0.0, size as f32 - 0.001)
    }
    pub fn x(&self, v: i32) -> f32 {
        Self::map(v, self.x_min, self.x_max, self.len)
    }
    pub fn y(&self, v: i32) -> f32 {
        Self::map(v, self.y_min, self.y_max, self.thickness)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    /// Contact is down (as of the last SYN_REPORT).
    live: bool,
    /// Down since before we started reading ([`Decoder::sync`]): tracked but never reported, so a
    /// finger resting on the bar at startup can't fire a key.
    foreign: bool,
    /// Pending state since the last SYN_REPORT.
    tracking: Option<bool>, // Some(true)=new contact, Some(false)=lifted
    x: i32,
    y: i32,
    moved: bool,
}

pub struct Decoder {
    map: Mapping,
    slots: [Slot; MAX_SLOTS],
    cur: usize,
    /// After SYN_DROPPED: ignore everything until the next SYN_REPORT.
    dropping: bool,
}

impl Decoder {
    pub fn new(map: Mapping) -> Self {
        Decoder {
            map,
            slots: [Slot::default(); MAX_SLOTS],
            cur: 0,
            dropping: false,
        }
    }

    pub fn mapping(&self) -> Mapping {
        self.map
    }

    /// Seeds the state from the kernel's current MT state at open (`EVIOCGABS(ABS_MT_SLOT)` and
    /// `EVIOCGMTSLOTS`). evdev doesn't replay contacts to a new reader, and doesn't resend
    /// `ABS_MT_SLOT` if the device's current slot is unchanged. Contacts already down become
    /// *foreign*: tracked until lifted, never reported.
    pub fn sync(&mut self, current_slot: i32, tracking_ids: &[i32], xs: &[i32], ys: &[i32]) {
        self.cur = current_slot.max(0) as usize;
        for (i, &tid) in tracking_ids.iter().enumerate().take(MAX_SLOTS) {
            let s = &mut self.slots[i];
            *s = Slot {
                live: tid >= 0,
                foreign: tid >= 0,
                ..Slot::default()
            };
            s.x = xs.get(i).copied().unwrap_or(0);
            s.y = ys.get(i).copied().unwrap_or(0);
        }
    }

    /// Feed one evdev event. Returns the contacts that changed, emitted on `SYN_REPORT`.
    pub fn feed(&mut self, ty: u16, code: u16, value: i32, time_us: u64, out: &mut Vec<Contact>) {
        if ty == EV_SYN {
            match code {
                SYN_DROPPED => {
                    self.cancel_all(time_us, out);
                    self.dropping = true;
                }
                SYN_REPORT if self.dropping => self.dropping = false,
                SYN_REPORT => self.report(time_us, out),
                _ => {}
            }
            return;
        }
        if ty != EV_ABS || self.dropping {
            return;
        }
        match code {
            ABS_MT_SLOT => self.cur = value.max(0) as usize,
            _ if self.cur >= MAX_SLOTS => {}
            ABS_MT_TRACKING_ID => self.slots[self.cur].tracking = Some(value >= 0),
            ABS_MT_POSITION_X => {
                let s = &mut self.slots[self.cur];
                s.moved |= s.x != value;
                s.x = value;
            }
            ABS_MT_POSITION_Y => {
                let s = &mut self.slots[self.cur];
                s.moved |= s.y != value;
                s.y = value;
            }
            _ => {}
        }
    }

    fn contact(&self, i: usize, phase: Phase, time_us: u64) -> Contact {
        let s = &self.slots[i];
        Contact {
            slot: i as u8,
            phase,
            x: self.map.x(s.x),
            y: self.map.y(s.y),
            time_us,
        }
    }

    fn report(&mut self, time_us: u64, out: &mut Vec<Contact>) {
        for i in 0..MAX_SLOTS {
            let s = self.slots[i];
            if s.foreign {
                let s = &mut self.slots[i];
                match s.tracking.take() {
                    Some(false) => {
                        *s = Slot {
                            x: s.x,
                            y: s.y,
                            ..Slot::default()
                        }
                    } // lifted: forget it
                    Some(true) => {
                        // replaced by a new contact: that one is ours
                        s.foreign = false;
                        s.live = true;
                        out.push(self.contact(i, Phase::Down, time_us));
                    }
                    None => {}
                }
                self.slots[i].moved = false;
                continue;
            }
            let phase = match (s.live, s.tracking) {
                (false, Some(true)) => Some(Phase::Down),
                (true, Some(false)) => Some(Phase::Up),
                // A new tracking id on a live slot without an Up between: end the old one, start new.
                (true, Some(true)) => {
                    out.push(self.contact(i, Phase::Up, time_us));
                    Some(Phase::Down)
                }
                (true, None) if s.moved => Some(Phase::Move),
                _ => None,
            };
            if let Some(p) = phase {
                out.push(self.contact(i, p, time_us));
            }
            let s = &mut self.slots[i];
            if let Some(t) = s.tracking.take() {
                s.live = t;
            }
            s.moved = false;
        }
    }

    /// Cancel every live contact (SYN_DROPPED, device gone).
    pub fn cancel_all(&mut self, time_us: u64, out: &mut Vec<Contact>) {
        for i in 0..MAX_SLOTS {
            if self.slots[i].live && !self.slots[i].foreign {
                out.push(self.contact(i, Phase::Cancel, time_us));
            }
            self.slots[i] = Slot {
                x: self.slots[i].x,
                y: self.slots[i].y,
                ..Slot::default()
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: Mapping = Mapping {
        x_min: 0,
        x_max: 23044,
        y_min: 0,
        y_max: 639,
        len: 2008,
        thickness: 60,
    };

    fn run(d: &mut Decoder, evs: &[(u16, u16, i32)]) -> Vec<Contact> {
        let mut out = Vec::new();
        for (i, &(t, c, v)) in evs.iter().enumerate() {
            d.feed(t, c, v, i as u64, &mut out);
        }
        out
    }

    fn phases(v: &[Contact]) -> Vec<(u8, Phase)> {
        v.iter().map(|c| (c.slot, c.phase)).collect()
    }

    const SYN: (u16, u16, i32) = (EV_SYN, SYN_REPORT, 0);

    #[test]
    fn tap_down_move_up() {
        let mut d = Decoder::new(MAP);
        let out = run(
            &mut d,
            &[
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 7),
                (EV_ABS, ABS_MT_POSITION_X, 800),
                (EV_ABS, ABS_MT_POSITION_Y, 320),
                SYN,
                (EV_ABS, ABS_MT_POSITION_X, 855),
                SYN,
                SYN, // no change: no event
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                SYN,
            ],
        );
        assert_eq!(
            phases(&out),
            vec![(0, Phase::Down), (0, Phase::Move), (0, Phase::Up)]
        );
        assert!((out[0].x - 800.0 * 2008.0 / 23044.0).abs() < 0.01);
        assert!((out[0].y - 30.0).abs() < 0.1);
        assert!(out[1].x > out[0].x);
    }

    #[test]
    fn three_fingers_independent() {
        let mut d = Decoder::new(MAP);
        let mut evs = vec![];
        for (s, x) in [(0, 5630), (1, 3760), (2, 8000)] {
            evs.extend([
                (EV_ABS, ABS_MT_SLOT, s),
                (EV_ABS, ABS_MT_TRACKING_ID, 10 + s),
                (EV_ABS, ABS_MT_POSITION_X, x),
            ]);
        }
        evs.push(SYN);
        evs.extend([
            (EV_ABS, ABS_MT_SLOT, 1),
            (EV_ABS, ABS_MT_TRACKING_ID, -1),
            SYN,
        ]);
        let out = run(&mut d, &evs);
        assert_eq!(
            phases(&out),
            vec![
                (0, Phase::Down),
                (1, Phase::Down),
                (2, Phase::Down),
                (1, Phase::Up)
            ]
        );
    }

    #[test]
    fn syn_dropped_cancels_and_resyncs() {
        let mut d = Decoder::new(MAP);
        let out = run(
            &mut d,
            &[
                (EV_ABS, ABS_MT_TRACKING_ID, 1),
                (EV_ABS, ABS_MT_POSITION_X, 100),
                SYN,
                (EV_SYN, SYN_DROPPED, 0),
                (EV_ABS, ABS_MT_POSITION_X, 999), // ignored until SYN_REPORT
                SYN,
                (EV_ABS, ABS_MT_TRACKING_ID, 2),
                (EV_ABS, ABS_MT_POSITION_X, 200),
                SYN,
            ],
        );
        assert_eq!(
            phases(&out),
            vec![(0, Phase::Down), (0, Phase::Cancel), (0, Phase::Down)]
        );
    }

    #[test]
    fn reused_slot_without_up() {
        let mut d = Decoder::new(MAP);
        let out = run(
            &mut d,
            &[
                (EV_ABS, ABS_MT_TRACKING_ID, 1),
                SYN,
                (EV_ABS, ABS_MT_TRACKING_ID, 2),
                SYN,
            ],
        );
        assert_eq!(
            phases(&out),
            vec![(0, Phase::Down), (0, Phase::Up), (0, Phase::Down)]
        );
    }

    #[test]
    fn finger_down_before_open_is_foreign() {
        let mut d = Decoder::new(MAP);
        // At open: device's current slot is 1, slot 1 has a contact, slot 0 is free.
        d.sync(1, &[-1, 42], &[0, 5000], &[0, 300]);
        let out = run(
            &mut d,
            &[
                (EV_ABS, ABS_MT_POSITION_X, 5100), // slot 1 (no ABS_MT_SLOT resent): foreign, silent
                SYN,
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 43), // a new finger on slot 0: ours
                (EV_ABS, ABS_MT_POSITION_X, 800),
                SYN,
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, -1), // the foreign finger lifts: no Up
                SYN,
                (EV_ABS, ABS_MT_TRACKING_ID, 44), // next contact on slot 1 is ours
                SYN,
            ],
        );
        assert_eq!(phases(&out), vec![(0, Phase::Down), (1, Phase::Down)]);
        assert!((out[0].x - 800.0 * 2008.0 / 23044.0).abs() < 0.01);
    }

    #[test]
    fn sync_current_slot_routes_unslotted_events() {
        let mut d = Decoder::new(MAP);
        d.sync(2, &[-1, -1, -1], &[], &[]);
        let out = run(&mut d, &[(EV_ABS, ABS_MT_TRACKING_ID, 9), SYN]);
        assert_eq!(phases(&out), vec![(2, Phase::Down)]);
    }

    #[test]
    fn foreign_contacts_never_cancel() {
        let mut d = Decoder::new(MAP);
        d.sync(0, &[5], &[100], &[100]);
        let out = run(&mut d, &[(EV_SYN, SYN_DROPPED, 0), SYN]);
        assert!(out.is_empty());
    }

    #[test]
    fn out_of_range_slots_ignored_and_clamped() {
        let mut d = Decoder::new(MAP);
        let out = run(
            &mut d,
            &[
                (EV_ABS, ABS_MT_SLOT, 200),
                (EV_ABS, ABS_MT_TRACKING_ID, 1),
                SYN,
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 3),
                (EV_ABS, ABS_MT_POSITION_X, 99999),
                SYN,
            ],
        );
        assert_eq!(phases(&out), vec![(0, Phase::Down)]);
        assert!(out[0].x < 2008.0);
    }
}
