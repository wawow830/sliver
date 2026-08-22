use std::time::Duration;

use anyhow::Result;
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum HardwareEvent {
    TouchTap { x: f64 },
    Fn { active: bool },
    Modifier { modifier: Modifier, active: bool },
    Device { present: bool },
    Visibility { visible: bool },
}

pub(crate) struct LogicalFrame {
    surface: ImageSurface,
}

impl LogicalFrame {
    pub(crate) fn new(surface: ImageSurface) -> Self {
        Self { surface }
    }

    pub(crate) fn surface(&self) -> &ImageSurface {
        &self.surface
    }
}

pub(crate) trait TouchBarHardware {
    fn claim(&mut self) -> Result<()>;
    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>>;
    fn present(&mut self, frame: &LogicalFrame) -> Result<()>;
    fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()>;
    fn set_backlight(&mut self, level: f64) -> Result<()>;
    fn release(&mut self) -> Result<()>;
}

#[cfg(test)]
mod fake {
    use std::mem;

    use anyhow::{ensure, Result};

    use super::{HardwareEvent, LogicalFrame, ModifierState, TouchBarHardware};

    #[derive(Debug, Clone, PartialEq)]
    pub(crate) enum FakeAction {
        Grab,
        Present,
        FunctionKeyTap {
            index: usize,
            modifiers: ModifierState,
        },
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
        fn capture(frame: &LogicalFrame) -> Result<Self> {
            let surface = frame.surface();
            let mut pixels = Vec::new();
            surface.with_data(|data| pixels.extend_from_slice(data))?;
            Ok(Self {
                width: surface.width() as usize,
                height: surface.height() as usize,
                stride: surface.stride() as usize,
                pixels,
            })
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
            self.frames.push(FrameSnapshot::capture(frame)?);
            self.actions.push(FakeAction::Present);
            Ok(())
        }

        fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            self.actions
                .push(FakeAction::FunctionKeyTap { index, modifiers });
            Ok(())
        }

        fn set_backlight(&mut self, level: f64) -> Result<()> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
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
pub(crate) use fake::{FakeAction, FakeTouchBar};
