//! Real hardware: [`crate::backend`] traits implemented for `sliver_hw`'s concrete types, plus
//! the mapping from `sliver_hw` input events to [`crate::Input`]. Owner: birch (T30).
//!
//! Every constructor here refuses unless `SLIVER_SAFESWAP=1` (the safe-swap harness or, later,
//! the real sliverd unit). Hardware facts: docs/hardware.md. Harness: docs/safe-swap.md.

use std::io;
use std::path::Path;

use sliver_hw::backlight::SysfsBacklight;
use sliver_hw::input::{HwEvent, HwInput};
use sliver_hw::mt::{Contact, Phase};
use sliver_hw::panel::DrmPanel;
use sliver_hw::uinput::UinputKeys;
use sliver_ipc::proto::TouchPhase;
use sliver_render::Pixmap;
use sliver_render::panel::{Orientation, Span, blit_xrgb8888};

use crate::Input;
use crate::backend::{Backends, Backlight, Display, Keys, TouchEvent};

impl Display for DrmPanel {
    fn flush(&mut self, frame: &Pixmap, damage: &[Span]) {
        let orientation = if self.rotated() {
            Orientation::Rotate90
        } else {
            Orientation::Landscape
        };
        let res = self.with_buffer(|buf, stride| {
            for &span in damage {
                blit_xrgb8888(frame, orientation, Some(span), buf, stride);
            }
        });
        if let Err(e) = res {
            eprintln!("sliverd: panel map failed: {e}");
            return;
        }
        let spans: Vec<(u32, u32)> = damage.iter().map(|s| (s.x, s.w)).collect();
        self.dirty(&spans);
    }
}

impl Backlight for SysfsBacklight {
    fn set(&mut self, value: u32) {
        if let Err(e) = SysfsBacklight::set(self, value) {
            eprintln!("sliverd: backlight write failed: {e}");
        }
    }
    fn screen_fraction(&mut self) -> Option<f32> {
        SysfsBacklight::screen_fraction(self)
    }
}

impl Keys for UinputKeys {
    fn key(&mut self, code: u16, down: bool) {
        UinputKeys::key(self, code, down);
    }
    fn sync(&mut self) {
        if let Err(e) = UinputKeys::sync(self) {
            eprintln!("sliverd: uinput write failed: {e}");
        }
    }
}

pub fn touch_event(c: Contact) -> TouchEvent {
    TouchEvent {
        slot: c.slot,
        phase: match c.phase {
            Phase::Down => TouchPhase::Down,
            Phase::Move => TouchPhase::Move,
            Phase::Up => TouchPhase::Up,
            Phase::Cancel => TouchPhase::Cancel,
        },
        x: c.x,
        y: c.y,
        time_us: c.time_us,
    }
}

/// `sliver_hw` input event -> daemon input.
pub fn input(ev: HwEvent) -> Input {
    match ev {
        HwEvent::Touch(c) => Input::Touch(touch_event(c)),
        HwEvent::Fn(held) => Input::FnKey { held },
        HwEvent::Activity => Input::Activity,
        HwEvent::Lid { closed } => Input::Lid { closed },
    }
}

impl crate::backend::InputSource for HwInput {
    fn drain(&mut self) -> Vec<crate::backend::HwEvent> {
        use crate::backend::HwEvent as E;
        HwInput::drain(self)
            .into_iter()
            .map(|ev| match ev {
                HwEvent::Touch(c) => E::Touch(touch_event(c)),
                HwEvent::Fn(held) => E::FnKey { held },
                HwEvent::Activity => E::Activity,
                HwEvent::Lid { closed } => E::Lid { closed },
            })
            .collect()
    }
}

/// Everything opened for a real run.
pub struct RealHardware {
    pub backends: Backends,
    /// Pollable (one epoll fd). Call `drain()` when readable and feed each event via [`input`].
    pub input: HwInput,
    /// Logical bar size, e.g. 2008 x 60.
    pub width: u32,
    pub height: u32,
    pub max_brightness: u32,
}

/// Discovers and opens the bar's panel, touch + seat0 input, backlight and a uinput keyboard.
pub fn open() -> io::Result<RealHardware> {
    let card = sliver_hw::discover::bar_card(Path::new("/sys/class/drm"))
        .ok_or_else(|| io::Error::other("no Touch Bar DRM card"))?;
    let (touch, _) = sliver_hw::discover::touch_device(Path::new("/sys/class/input"))
        .ok_or_else(|| io::Error::other("no Touch Bar digitizer"))?;
    let panel = DrmPanel::open(&card)?;
    let (width, height) = panel.logical_size();
    let input = HwInput::open(&touch, width, height)?;
    let backlight = SysfsBacklight::open()?;
    let max_brightness = backlight.max();
    let keys = UinputKeys::create()?;
    Ok(RealHardware {
        backends: Backends {
            display: Box::new(panel),
            backlight: Box::new(backlight),
            keys: Box::new(keys),
        },
        input,
        width,
        height,
        max_brightness,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_events() {
        let c = Contact {
            slot: 2,
            phase: Phase::Cancel,
            x: 10.0,
            y: 30.0,
            time_us: 5,
        };
        let want = TouchEvent {
            slot: 2,
            phase: TouchPhase::Cancel,
            x: 10.0,
            y: 30.0,
            time_us: 5,
        };
        assert!(matches!(input(HwEvent::Touch(c)), Input::Touch(t) if t == want));
        assert!(matches!(
            input(HwEvent::Fn(true)),
            Input::FnKey { held: true }
        ));
        assert!(matches!(
            input(HwEvent::Lid { closed: false }),
            Input::Lid { closed: false }
        ));
        assert!(matches!(input(HwEvent::Activity), Input::Activity));
    }

    #[test]
    fn refuses_outside_harness() {
        if std::env::var("SLIVER_SAFESWAP").is_ok() {
            return;
        }
        assert!(open().is_err());
    }
}
