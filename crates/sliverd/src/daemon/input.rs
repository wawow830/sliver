//! Touches routed to regions, and keys sent to the kernel.

use super::*;

impl Daemon {
    pub(super) fn emit_keys(&mut self, now: u64, client: ClientId, keys: Vec<u16>, mode: KeyMode) {
        if let Err(e) = validate::validate_keys(&keys) {
            return self.error(client, e.code(), e.to_string(), None);
        }
        let c = self.clients.get_mut(&client).expect("checked");
        let (start, n) = c.emit_window;
        if now.saturating_sub(start) >= 1_000 {
            c.emit_window = (now, 1);
        } else if n >= EMIT_KEYS_PER_SEC {
            return self.error(client, ErrorCode::RateLimited, "emit_keys".into(), None);
        } else {
            c.emit_window.1 += 1;
        }
        let out = &mut *self.hw.keys;
        match mode {
            KeyMode::Tap => {
                self.keys.press(&keys, out);
                self.keys.release(&keys, out);
            }
            KeyMode::Down => {
                self.keys.press(&keys, out);
                c.held_keys.extend(&keys);
            }
            KeyMode::Up => {
                // Only what this agent holds: it can't release a finger's key.
                let mut mine = Vec::new();
                for k in &keys {
                    if let Some(i) = c.held_keys.iter().rposition(|h| h == k) {
                        c.held_keys.remove(i);
                        mine.push(*k);
                    }
                }
                self.keys.release(&mine, out);
            }
        }
    }

    // ---- touch --------------------------------------------------------------------------------

    pub(super) fn touch(&mut self, now: u64, mut ev: TouchEvent) {
        // The picture is moved by pixel shift: hit-test where the finger is on the frame.
        let (dx, dy) = self.shift.offset();
        ev.x = (ev.x - dx as f32).clamp(0.0, self.panel.width.saturating_sub(1) as f32);
        ev.y = (ev.y - dy as f32).clamp(0.0, self.panel.height.saturating_sub(1) as f32);
        let was_off = self.idle.state() == DisplayState::Off;
        self.activity(now);
        if ev.phase == TouchPhase::Down && (was_off || now < self.settle_until) {
            // The touch that wakes the bar, or one landing right after the agent vanished, is
            // swallowed. The router never sees its down, so the rest of it is ignored too.
            return;
        }
        let regions = match self.shown_view() {
            View::Agent(l) => {
                let c = self.active.and_then(|c| self.clients.get(&c));
                let f = c.and_then(|c| match l {
                    Layer::Base => c.base.as_ref(),
                    Layer::Fn => c.fn_layer.as_ref(),
                });
                f.map_or(&[][..], |f| &f.regions)
            }
            View::Builtin(BuiltinKind::FKeys) => &self.fkeys.regions,
            View::Builtin(BuiltinKind::Media) => &self.media.regions,
        };
        let routed = self.router.touch(regions, ev.slot, ev.phase, ev.x, ev.y);
        self.route(now, routed, ev.time_us);
    }

    /// Carries out the router's decisions: keys to uinput (one sync per batch), agent touches to
    /// the agent. Redraws if keys changed (pressed feedback).
    pub(super) fn route(&mut self, now: u64, routed: Vec<Routed>, time_us: u64) {
        let mut keys = false;
        for r in routed {
            match r {
                Routed::Key { code, down } => {
                    self.hw.keys.key(code, down);
                    keys = true;
                }
                Routed::Agent {
                    region,
                    slot,
                    phase,
                    x,
                    y,
                } => {
                    let Some(c) = self.active else { continue };
                    let msg = DaemonMsg::Touch {
                        region,
                        slot,
                        phase,
                        x,
                        y,
                        time_us,
                    };
                    self.send(c, msg);
                    if phase == TouchPhase::Down {
                        self.ping(now, false);
                    }
                }
            }
        }
        if keys {
            self.hw.keys.sync();
            self.recompose();
        }
    }

    // ---- frames -------------------------------------------------------------------------------
}
