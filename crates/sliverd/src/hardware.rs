use std::time::Duration;

use anyhow::{ensure, Result};
use cairo::ImageSurface;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modifier {
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftShift,
    RightShift,
    LeftSuper,
    RightSuper,
}

impl Modifier {
    pub(crate) const ALL: [Self; 8] = [
        Self::LeftCtrl,
        Self::RightCtrl,
        Self::LeftAlt,
        Self::RightAlt,
        Self::LeftShift,
        Self::RightShift,
        Self::LeftSuper,
        Self::RightSuper,
    ];

    const fn index(self) -> usize {
        match self {
            Self::LeftCtrl => 0,
            Self::RightCtrl => 1,
            Self::LeftAlt => 2,
            Self::RightAlt => 3,
            Self::LeftShift => 4,
            Self::RightShift => 5,
            Self::LeftSuper => 6,
            Self::RightSuper => 7,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ModifierState([bool; 8]);

impl ModifierState {
    pub(crate) fn set(&mut self, modifier: Modifier, active: bool) {
        self.0[modifier.index()] = active;
    }

    pub(crate) fn is_active(self, modifier: Modifier) -> bool {
        self.0[modifier.index()]
    }
}

pub(crate) type ContactId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TouchEvent {
    pub(crate) phase: TouchPhase,
    pub(crate) id: ContactId,
    pub(crate) time: f64,
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) modifiers: ModifierState,
    pub(crate) pressure: Option<f64>,
    pub(crate) width: Option<f64>,
    pub(crate) height: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum HardwareEvent {
    Touch(TouchEvent),
    Fn {
        active: bool,
    },
    Modifier {
        modifier: Modifier,
        active: bool,
    },
    // Hardware lifecycle inputs remain generic at this seam.
    #[allow(dead_code)]
    Device {
        present: bool,
    },
    #[allow(dead_code)]
    Visibility {
        visible: bool,
    },
}

#[derive(Clone)]
pub(crate) struct LogicalFrame {
    width: usize,
    height: usize,
    stride: usize,
    pixels: Vec<u8>,
}

impl LogicalFrame {
    pub(crate) fn from_surface(surface: &ImageSurface) -> Result<Self> {
        let mut pixels = Vec::new();
        surface.with_data(|data| pixels.extend_from_slice(data))?;
        Ok(Self {
            width: surface.width() as usize,
            height: surface.height() as usize,
            stride: surface.stride() as usize,
            pixels,
        })
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    pub(crate) fn stride(&self) -> usize {
        self.stride
    }

    pub(crate) fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

pub(crate) fn validate_backlight(level: f64) -> Result<()> {
    ensure!(
        level.is_finite() && (0.0..=1.0).contains(&level),
        "backlight level must be between 0.0 and 1.0"
    );
    Ok(())
}

pub(crate) trait TouchBarHardware {
    fn claim(&mut self) -> Result<()>;
    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>>;
    fn present(&mut self, frame: &LogicalFrame) -> Result<()>;
    fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()>;
    fn get_backlight(&mut self) -> Result<f64>;
    fn set_backlight(&mut self, level: f64) -> Result<()>;
    fn release(&mut self) -> Result<()>;
}

#[cfg(test)]
pub(crate) use fake::{FakeAction, FakeKey, FakeKeyEvent, FakeTouchBar};

#[cfg(test)]
mod fake {
    use std::mem;

    use anyhow::{ensure, Result};

    use super::{HardwareEvent, LogicalFrame, Modifier, ModifierState, TouchBarHardware};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum FakeKey {
        Function(usize),
        Modifier(Modifier),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct FakeKeyEvent {
        pub(crate) key: FakeKey,
        pub(crate) active: bool,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum FakeAction {
        Grab,
        Present,
        SyntheticKey(FakeKeyEvent),
        Backlight(f64),
        Release,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct FrameSnapshot {
        width: usize,
        height: usize,
        stride: usize,
        pixels: Vec<u8>,
    }

    impl FrameSnapshot {
        fn capture(frame: &LogicalFrame) -> Self {
            Self {
                width: frame.width(),
                height: frame.height(),
                stride: frame.stride(),
                pixels: frame.pixels().to_vec(),
            }
        }

        pub(crate) fn dimensions(&self) -> (usize, usize) {
            (self.width, self.height)
        }

        pub(crate) fn rgba_at(&self, x: usize, y: usize) -> [u8; 4] {
            assert!(x < self.width && y < self.height);
            let offset = y * self.stride + x * 4;
            let pixel = u32::from_ne_bytes(
                self.pixels[offset..offset + 4]
                    .try_into()
                    .expect("ARGB32 pixel is four bytes"),
            );
            [
                ((pixel >> 16) & 0xff) as u8,
                ((pixel >> 8) & 0xff) as u8,
                (pixel & 0xff) as u8,
                ((pixel >> 24) & 0xff) as u8,
            ]
        }
    }

    #[derive(Default)]
    pub(crate) struct FakeTouchBar {
        claimed: bool,
        events: Vec<HardwareEvent>,
        actions: Vec<FakeAction>,
        frames: Vec<FrameSnapshot>,
        backlight: f64,
    }

    impl FakeTouchBar {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn inject(&mut self, event: HardwareEvent) {
            self.events.push(event);
        }

        pub(crate) fn actions(&self) -> &[FakeAction] {
            &self.actions
        }

        pub(crate) fn presented_frames(&self) -> &[FrameSnapshot] {
            &self.frames
        }

        pub(crate) fn backlight_level(&self) -> f64 {
            self.backlight
        }
    }

    impl TouchBarHardware for FakeTouchBar {
        fn claim(&mut self) -> Result<()> {
            ensure!(!self.claimed, "fake Touch Bar is already claimed");
            self.claimed = true;
            self.actions.push(FakeAction::Grab);
            Ok(())
        }

        fn poll(&mut self, _timeout: std::time::Duration) -> Result<Vec<HardwareEvent>> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            Ok(mem::take(&mut self.events))
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            self.frames.push(FrameSnapshot::capture(frame));
            self.actions.push(FakeAction::Present);
            Ok(())
        }

        fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            ensure!(index < 12, "function-key index is out of range");
            for modifier in Modifier::ALL {
                if modifiers.is_active(modifier) {
                    self.actions.push(FakeAction::SyntheticKey(FakeKeyEvent {
                        key: FakeKey::Modifier(modifier),
                        active: true,
                    }));
                }
            }
            self.actions.push(FakeAction::SyntheticKey(FakeKeyEvent {
                key: FakeKey::Function(index),
                active: true,
            }));
            self.actions.push(FakeAction::SyntheticKey(FakeKeyEvent {
                key: FakeKey::Function(index),
                active: false,
            }));
            for modifier in Modifier::ALL.into_iter().rev() {
                if modifiers.is_active(modifier) {
                    self.actions.push(FakeAction::SyntheticKey(FakeKeyEvent {
                        key: FakeKey::Modifier(modifier),
                        active: false,
                    }));
                }
            }
            Ok(())
        }

        fn get_backlight(&mut self) -> Result<f64> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            Ok(self.backlight)
        }

        fn set_backlight(&mut self, level: f64) -> Result<()> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            super::validate_backlight(level)?;
            self.backlight = level;
            self.actions.push(FakeAction::Backlight(level));
            Ok(())
        }

        fn release(&mut self) -> Result<()> {
            if self.claimed {
                self.claimed = false;
                self.actions.push(FakeAction::Release);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_round_trips_lifecycle_touch_snapshots() -> Result<()> {
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let mut modifiers = ModifierState::default();
        modifiers.set(Modifier::LeftCtrl, true);
        let touch = TouchEvent {
            phase: TouchPhase::Down,
            id: 7,
            time: 1.25,
            x: 100.0,
            y: 20.0,
            modifiers,
            pressure: Some(0.5),
            width: Some(0.25),
            height: None,
        };

        hardware.inject(HardwareEvent::Touch(touch));

        assert_eq!(
            hardware.poll(Duration::ZERO)?,
            vec![HardwareEvent::Touch(touch)]
        );
        hardware.release()?;
        Ok(())
    }

    #[test]
    fn fake_backlight_tracks_validated_writes() -> Result<()> {
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        assert_eq!(hardware.get_backlight()?, 0.0);

        hardware.set_backlight(0.75)?;

        assert_eq!(hardware.get_backlight()?, 0.75);
        assert_eq!(
            hardware.actions(),
            &[FakeAction::Grab, FakeAction::Backlight(0.75)]
        );
        assert!(hardware.set_backlight(1.01).is_err());
        assert_eq!(hardware.backlight_level(), 0.75);
        Ok(())
    }
}
