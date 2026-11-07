//! The display: dimming and turning off on idle, the backlight, Fn and pixel shift.

use super::*;

impl Daemon {
    /// Set by agent policy. Built-in layers are laid out again to keep clear of the ends the
    /// picture moves towards.
    pub(super) fn set_pixel_shift(&mut self, on: bool, now: u64) {
        if !self.shift.set_enabled(on, now) {
            return;
        }
        let x = if on {
            crate::shift::RANGE_X as f32
        } else {
            0.0
        };
        self.renderer.set_extra_inset([0.0, x, 0.0, x]);
        let (w, h) = (self.panel.width, self.panel.height);
        let esc = !self.profile.physical_esc;
        self.fkeys = BuiltinLayer::new(
            BuiltinKind::FKeys,
            esc,
            &mut self.renderer,
            &self.sheet,
            w,
            h,
        );
        self.media = BuiltinLayer::new(
            BuiltinKind::Media,
            esc,
            &mut self.renderer,
            &self.sheet,
            w,
            h,
        );
        self.recompose();
    }

    pub(super) fn fn_key(&mut self, now: u64, held: bool) {
        self.activity(now);
        let was = self.hatch.active();
        let open = self.hatch.fn_key(held, now);
        if open != was {
            // The visible layer changes owner: nothing captured on the old one stays pressed.
            let routed = self.router.cancel_all();
            self.route(now, routed, 0);
            if let Some(c) = self.active {
                self.send(c, DaemonMsg::Hatch { active: open });
            }
        }
        if self.fn_held != held || open != was {
            self.fn_held = held;
            if let Some(c) = self.active {
                self.send(c, DaemonMsg::FnKey { held });
            }
            self.recompose();
        }
    }

    pub(super) fn activity(&mut self, now: u64) {
        let before = self.idle.state();
        let after = self.idle.activity(now);
        if after != before {
            self.display_changed(now, after);
        } else if self.idle.follow_screen() {
            self.apply_backlight();
        }
    }

    pub(super) fn display_changed(&mut self, now: u64, state: DisplayState) {
        self.apply_backlight();
        if let Some(c) = self.active {
            self.send(c, DaemonMsg::Display { state });
        }
        if state == DisplayState::Off {
            // Nobody holds a key on a dark bar.
            let routed = self.router.cancel_all();
            self.route(now, routed, 0);
        } else {
            self.flush(); // whatever changed while off
        }
        if state == DisplayState::On {
            self.ping(now, true);
        }
    }

    pub(super) fn apply_backlight(&mut self) {
        let screen = if self.idle.follow_screen() {
            self.hw.backlight.screen_fraction()
        } else {
            None
        };
        let v = self.idle.brightness(screen);
        if self.backlight != Some(v) {
            self.backlight = Some(v);
            self.hw.backlight.set(v);
        }
    }
}
