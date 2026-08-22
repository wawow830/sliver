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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct InputState {
    pub(crate) fn_active: bool,
    pub(crate) modifiers: ModifierState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObservedKey {
    Fn,
    Modifier(Modifier),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputTransition {
    pub(crate) key: ObservedKey,
    pub(crate) active: bool,
    pub(crate) state: InputState,
}

impl InputState {
    pub(crate) fn apply(&mut self, key: ObservedKey, active: bool) -> bool {
        let old = match key {
            ObservedKey::Fn => self.fn_active,
            ObservedKey::Modifier(modifier) => self.modifiers.is_active(modifier),
        };
        if old == active {
            return false;
        }
        match key {
            ObservedKey::Fn => self.fn_active = active,
            ObservedKey::Modifier(modifier) => self.modifiers.set(modifier, active),
        }
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum KeyboardKey {
    Escape,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftShift,
    RightShift,
    LeftSuper,
    RightSuper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ConsumerKey {
    BrightnessDown,
    BrightnessUp,
    Previous,
    PlayPause,
    Next,
    Mute,
    VolumeDown,
    VolumeUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OutputKey {
    Keyboard(KeyboardKey),
    Consumer(ConsumerKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SyntheticKeyEvent {
    pub(crate) key: OutputKey,
    pub(crate) active: bool,
}

pub(crate) fn tap_key_events(key: OutputKey, modifiers: &[OutputKey]) -> Vec<SyntheticKeyEvent> {
    let mut events = modifiers
        .iter()
        .copied()
        .map(|key| SyntheticKeyEvent { key, active: true })
        .collect::<Vec<_>>();
    events.push(SyntheticKeyEvent { key, active: true });
    events.push(SyntheticKeyEvent { key, active: false });
    events.extend(
        modifiers
            .iter()
            .rev()
            .copied()
            .map(|key| SyntheticKeyEvent { key, active: false }),
    );
    events
}

pub(crate) fn modifier_output_keys(state: ModifierState) -> Vec<OutputKey> {
    Modifier::ALL
        .into_iter()
        .filter(|modifier| state.is_active(*modifier))
        .map(|modifier| {
            OutputKey::Keyboard(match modifier {
                Modifier::LeftCtrl => KeyboardKey::LeftCtrl,
                Modifier::RightCtrl => KeyboardKey::RightCtrl,
                Modifier::LeftAlt => KeyboardKey::LeftAlt,
                Modifier::RightAlt => KeyboardKey::RightAlt,
                Modifier::LeftShift => KeyboardKey::LeftShift,
                Modifier::RightShift => KeyboardKey::RightShift,
                Modifier::LeftSuper => KeyboardKey::LeftSuper,
                Modifier::RightSuper => KeyboardKey::RightSuper,
            })
        })
        .collect()
}

pub(crate) fn function_key_output(index: usize) -> Option<OutputKey> {
    Some(OutputKey::Keyboard(match index {
        0 => KeyboardKey::F1,
        1 => KeyboardKey::F2,
        2 => KeyboardKey::F3,
        3 => KeyboardKey::F4,
        4 => KeyboardKey::F5,
        5 => KeyboardKey::F6,
        6 => KeyboardKey::F7,
        7 => KeyboardKey::F8,
        8 => KeyboardKey::F9,
        9 => KeyboardKey::F10,
        10 => KeyboardKey::F11,
        11 => KeyboardKey::F12,
        _ => return None,
    }))
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
    fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()>;
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

    use super::{
        ConsumerKey, HardwareEvent, KeyboardKey, LogicalFrame, Modifier, ModifierState, OutputKey,
        SyntheticKeyEvent, TouchBarHardware,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum FakeKey {
        Function(usize),
        Modifier(Modifier),
        Keyboard(KeyboardKey),
        Consumer(ConsumerKey),
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
        scheduled_events: Vec<(usize, HardwareEvent)>,
        poll_count: usize,
        actions: Vec<FakeAction>,
        frames: Vec<FrameSnapshot>,
        backlight: f64,
        virtual_keyboard_name: Option<String>,
        virtual_keyboard_creations: usize,
        synthetic_keys: Vec<FakeKeyEvent>,
        synthetic_transactions: Vec<Vec<FakeKeyEvent>>,
    }

    impl FakeTouchBar {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn inject(&mut self, event: HardwareEvent) {
            self.events.push(event);
        }

        pub(crate) fn inject_on_poll(&mut self, poll: usize, event: HardwareEvent) {
            self.scheduled_events.push((poll, event));
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

        pub(crate) fn virtual_keyboard_name(&self) -> Option<&str> {
            self.virtual_keyboard_name.as_deref()
        }

        pub(crate) fn virtual_keyboard_creations(&self) -> usize {
            self.virtual_keyboard_creations
        }

        pub(crate) fn synthetic_keys(&self) -> &[FakeKeyEvent] {
            &self.synthetic_keys
        }

        pub(crate) fn synthetic_transactions(&self) -> &[Vec<FakeKeyEvent>] {
            &self.synthetic_transactions
        }
    }

    impl TouchBarHardware for FakeTouchBar {
        fn claim(&mut self) -> Result<()> {
            ensure!(!self.claimed, "fake Touch Bar is already claimed");
            self.claimed = true;
            if self.virtual_keyboard_name.is_none() {
                self.virtual_keyboard_name = Some("Sliver Keyboard".into());
                self.virtual_keyboard_creations += 1;
            }
            self.actions.push(FakeAction::Grab);
            Ok(())
        }

        fn poll(&mut self, _timeout: std::time::Duration) -> Result<Vec<HardwareEvent>> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            self.poll_count += 1;
            let poll = self.poll_count;
            self.events.extend(
                self.scheduled_events
                    .extract_if(.., |(scheduled, _)| *scheduled == poll)
                    .map(|(_, event)| event),
            );
            Ok(mem::take(&mut self.events))
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            self.frames.push(FrameSnapshot::capture(frame));
            self.actions.push(FakeAction::Present);
            Ok(())
        }

        fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
            ensure!(self.claimed, "fake Touch Bar is not claimed");
            let mut transaction = Vec::with_capacity(events.len());
            for event in events {
                let key = match event.key {
                    OutputKey::Keyboard(key) => FakeKey::Keyboard(key),
                    OutputKey::Consumer(key) => FakeKey::Consumer(key),
                };
                let event = FakeKeyEvent {
                    key,
                    active: event.active,
                };
                self.synthetic_keys.push(event);
                transaction.push(event);
                self.actions.push(FakeAction::SyntheticKey(event));
            }
            self.synthetic_transactions.push(transaction);
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
