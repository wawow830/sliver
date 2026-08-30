use std::time::Duration;

use anyhow::{ensure, Result};
use cairo::ImageSurface;

use crate::frame_slots::{CompletedFrame, FrameTiming};

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

    pub(crate) const fn index(self) -> usize {
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

impl KeyboardKey {
    pub(crate) fn from_wire(value: u8) -> Result<Self> {
        let key = match value {
            0 => Self::Escape,
            1 => Self::F1,
            2 => Self::F2,
            3 => Self::F3,
            4 => Self::F4,
            5 => Self::F5,
            6 => Self::F6,
            7 => Self::F7,
            8 => Self::F8,
            9 => Self::F9,
            10 => Self::F10,
            11 => Self::F11,
            12 => Self::F12,
            13 => Self::LeftCtrl,
            14 => Self::RightCtrl,
            15 => Self::LeftAlt,
            16 => Self::RightAlt,
            17 => Self::LeftShift,
            18 => Self::RightShift,
            19 => Self::LeftSuper,
            20 => Self::RightSuper,
            _ => anyhow::bail!("unknown Lua worker keyboard key {value}"),
        };
        Ok(key)
    }
}

impl ConsumerKey {
    pub(crate) fn from_wire(value: u8) -> Result<Self> {
        let key = match value {
            0 => Self::BrightnessDown,
            1 => Self::BrightnessUp,
            2 => Self::Previous,
            3 => Self::PlayPause,
            4 => Self::Next,
            5 => Self::Mute,
            6 => Self::VolumeDown,
            7 => Self::VolumeUp,
            _ => anyhow::bail!("unknown Lua worker consumer key {value}"),
        };
        Ok(key)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutputKeyMetadata {
    pub(crate) name: &'static str,
    pub(crate) key: OutputKey,
    pub(crate) function_row: Option<usize>,
    pub(crate) modifier: Option<Modifier>,
}

impl OutputKeyMetadata {
    const fn keyboard(name: &'static str, key: KeyboardKey) -> Self {
        Self {
            name,
            key: OutputKey::Keyboard(key),
            function_row: None,
            modifier: None,
        }
    }

    const fn function_row(name: &'static str, key: KeyboardKey, row: usize) -> Self {
        Self {
            name,
            key: OutputKey::Keyboard(key),
            function_row: Some(row),
            modifier: None,
        }
    }

    const fn modifier_key(name: &'static str, key: KeyboardKey, modifier: Modifier) -> Self {
        Self {
            name,
            key: OutputKey::Keyboard(key),
            function_row: None,
            modifier: Some(modifier),
        }
    }

    const fn consumer(name: &'static str, key: ConsumerKey) -> Self {
        Self {
            name,
            key: OutputKey::Consumer(key),
            function_row: None,
            modifier: None,
        }
    }
}

const OUTPUT_KEY_METADATA: &[OutputKeyMetadata] = &[
    OutputKeyMetadata::keyboard("escape", KeyboardKey::Escape),
    OutputKeyMetadata::function_row("f1", KeyboardKey::F1, 0),
    OutputKeyMetadata::function_row("f2", KeyboardKey::F2, 1),
    OutputKeyMetadata::function_row("f3", KeyboardKey::F3, 2),
    OutputKeyMetadata::function_row("f4", KeyboardKey::F4, 3),
    OutputKeyMetadata::function_row("f5", KeyboardKey::F5, 4),
    OutputKeyMetadata::function_row("f6", KeyboardKey::F6, 5),
    OutputKeyMetadata::function_row("f7", KeyboardKey::F7, 6),
    OutputKeyMetadata::function_row("f8", KeyboardKey::F8, 7),
    OutputKeyMetadata::function_row("f9", KeyboardKey::F9, 8),
    OutputKeyMetadata::function_row("f10", KeyboardKey::F10, 9),
    OutputKeyMetadata::function_row("f11", KeyboardKey::F11, 10),
    OutputKeyMetadata::function_row("f12", KeyboardKey::F12, 11),
    OutputKeyMetadata::modifier_key("left_ctrl", KeyboardKey::LeftCtrl, Modifier::LeftCtrl),
    OutputKeyMetadata::modifier_key("right_ctrl", KeyboardKey::RightCtrl, Modifier::RightCtrl),
    OutputKeyMetadata::modifier_key("left_alt", KeyboardKey::LeftAlt, Modifier::LeftAlt),
    OutputKeyMetadata::modifier_key("right_alt", KeyboardKey::RightAlt, Modifier::RightAlt),
    OutputKeyMetadata::modifier_key("left_shift", KeyboardKey::LeftShift, Modifier::LeftShift),
    OutputKeyMetadata::modifier_key("right_shift", KeyboardKey::RightShift, Modifier::RightShift),
    OutputKeyMetadata::modifier_key("left_super", KeyboardKey::LeftSuper, Modifier::LeftSuper),
    OutputKeyMetadata::modifier_key("right_super", KeyboardKey::RightSuper, Modifier::RightSuper),
    OutputKeyMetadata::consumer("brightness_down", ConsumerKey::BrightnessDown),
    OutputKeyMetadata::consumer("brightness_up", ConsumerKey::BrightnessUp),
    OutputKeyMetadata::consumer("previous", ConsumerKey::Previous),
    OutputKeyMetadata::consumer("play_pause", ConsumerKey::PlayPause),
    OutputKeyMetadata::consumer("next", ConsumerKey::Next),
    OutputKeyMetadata::consumer("mute", ConsumerKey::Mute),
    OutputKeyMetadata::consumer("volume_down", ConsumerKey::VolumeDown),
    OutputKeyMetadata::consumer("volume_up", ConsumerKey::VolumeUp),
];

pub(crate) fn output_key_metadata() -> &'static [OutputKeyMetadata] {
    OUTPUT_KEY_METADATA
}

fn modifier_metadata(modifier: Modifier) -> &'static OutputKeyMetadata {
    output_key_metadata()
        .iter()
        .find(|metadata| metadata.modifier == Some(modifier))
        .expect("every modifier has output key metadata")
}

pub(crate) fn modifier_output_key(modifier: Modifier) -> OutputKey {
    modifier_metadata(modifier).key
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
        .map(modifier_output_key)
        .collect()
}

pub(crate) fn function_key_output(index: usize) -> Option<OutputKey> {
    output_key_metadata()
        .iter()
        .find(|metadata| metadata.function_row == Some(index))
        .map(|metadata| metadata.key)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum HardwareCapability {
    Display,
    Touch,
    Fn,
    SyntheticKeys,
    Backlight,
}

impl HardwareCapability {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Touch => "touch input",
            Self::Fn => "Fn observation",
            Self::SyntheticKeys => "synthetic key output",
            Self::Backlight => "backlight",
        }
    }

    pub(crate) fn to_wire(self) -> u8 {
        match self {
            Self::Display => 0,
            Self::Touch => 1,
            Self::Fn => 2,
            Self::SyntheticKeys => 3,
            Self::Backlight => 4,
        }
    }

    pub(crate) fn from_wire(value: u8) -> Result<Self> {
        Ok(match value {
            0 => Self::Display,
            1 => Self::Touch,
            2 => Self::Fn,
            3 => Self::SyntheticKeys,
            4 => Self::Backlight,
            _ => anyhow::bail!("unknown hardware capability {value}"),
        })
    }
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
    // Device means that the complete hardware contract appeared or vanished.
    Device {
        present: bool,
    },
    Capability {
        capability: HardwareCapability,
        present: bool,
    },
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

    pub(crate) fn from_wire(width: usize, height: usize, stride: usize, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            stride,
            pixels,
        }
    }

    pub(crate) fn from_completed(completed: CompletedFrame) -> (Self, FrameTiming) {
        (
            Self {
                width: completed.width,
                height: completed.height,
                stride: completed.stride,
                pixels: completed.pixels,
            },
            completed.timing,
        )
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
    /// Reacquire devices after a transient disappearance. Adapters that keep
    /// their claim while reporting lifecycle events need no work here.
    fn reacquire(&mut self) -> Result<()> {
        Ok(())
    }
    fn is_available(&self) -> bool {
        true
    }
    fn unavailable_capability(&self) -> Option<HardwareCapability> {
        None
    }
    fn session_revoked(&self) -> bool {
        false
    }
    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>>;
    fn input_state(&self) -> InputState;
    fn present(&mut self, frame: &LogicalFrame) -> Result<()>;
    /// Confirm ownership again after a candidate frame has been presented.
    /// Adapters with a session owner use this as the handoff commit check.
    fn confirm_owner(&mut self) -> Result<()> {
        Ok(())
    }
    /// Sends the complete ordered sequence as one virtual-device batch.
    /// Implementations must preserve slice order and must not split or delay it.
    fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()>;
    fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()>;
    fn get_backlight(&mut self) -> Result<f64>;
    fn set_backlight(&mut self, level: f64) -> Result<()>;
    fn release(&mut self) -> Result<()>;
}

#[cfg(test)]
pub(crate) use fake::{FakeAction, FakeKey, FakeKeyEvent, FakeTouchBar, FrameSnapshot};

#[cfg(test)]
mod fake {
    use std::mem;

    use anyhow::{ensure, Result};

    use super::{
        function_key_output, modifier_output_keys, tap_key_events, ConsumerKey, HardwareCapability,
        HardwareEvent, InputState, KeyboardKey, LogicalFrame, Modifier, ModifierState, ObservedKey,
        OutputKey, SyntheticKeyEvent, TouchBarHardware,
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
        input_state: InputState,
        available: bool,
        discovery_available: bool,
        unavailable_capability: Option<HardwareCapability>,
        virtual_keyboard_name: Option<String>,
        virtual_keyboard_creations: usize,
        synthetic_keys: Vec<FakeKeyEvent>,
        synthetic_transactions: Vec<Vec<FakeKeyEvent>>,
    }

    impl FakeTouchBar {
        pub(crate) fn new() -> Self {
            Self {
                available: true,
                discovery_available: true,
                ..Self::default()
            }
        }

        pub(crate) fn unavailable() -> Self {
            Self {
                available: false,
                discovery_available: false,
                ..Self::new()
            }
        }

        pub(crate) fn make_available(&mut self) {
            self.discovery_available = true;
        }

        pub(crate) fn with_input_state(input_state: InputState) -> Self {
            Self {
                input_state,
                available: true,
                discovery_available: true,
                ..Self::default()
            }
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
            ensure!(
                self.discovery_available,
                "fake Touch Bar hardware is unavailable"
            );
            ensure!(!self.claimed, "fake Touch Bar is already claimed");
            self.claimed = true;
            self.available = true;
            self.unavailable_capability = None;
            if self.virtual_keyboard_name.is_none() {
                self.virtual_keyboard_name = Some("Sliver Keyboard".into());
                self.virtual_keyboard_creations += 1;
            }
            self.actions.push(FakeAction::Grab);
            Ok(())
        }

        fn reacquire(&mut self) -> Result<()> {
            if self.claimed {
                return Ok(());
            }
            self.claim()
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
            let events = mem::take(&mut self.events);
            for event in &events {
                match *event {
                    HardwareEvent::Fn { active } => {
                        self.input_state.apply(ObservedKey::Fn, active);
                    }
                    HardwareEvent::Modifier { modifier, active } => {
                        self.input_state
                            .apply(ObservedKey::Modifier(modifier), active);
                    }
                    HardwareEvent::Touch(_) | HardwareEvent::Visibility { .. } => {}
                    HardwareEvent::Device { present } => {
                        self.available = present;
                        self.unavailable_capability =
                            (!present).then_some(HardwareCapability::Display);
                    }
                    HardwareEvent::Capability {
                        capability,
                        present,
                    } => {
                        self.available = present;
                        self.unavailable_capability = (!present).then_some(capability);
                    }
                }
            }
            Ok(events)
        }

        fn input_state(&self) -> InputState {
            self.input_state
        }

        fn is_available(&self) -> bool {
            self.available
        }

        fn unavailable_capability(&self) -> Option<HardwareCapability> {
            self.unavailable_capability
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
            let function_key = function_key_output(index)
                .ok_or_else(|| anyhow::anyhow!("function-key index is out of range"))?;
            for event in tap_key_events(function_key, &modifier_output_keys(modifiers)) {
                let key = match event.key {
                    key if key == function_key => FakeKey::Function(index),
                    OutputKey::Keyboard(KeyboardKey::LeftCtrl) => {
                        FakeKey::Modifier(Modifier::LeftCtrl)
                    }
                    OutputKey::Keyboard(KeyboardKey::RightCtrl) => {
                        FakeKey::Modifier(Modifier::RightCtrl)
                    }
                    OutputKey::Keyboard(KeyboardKey::LeftAlt) => {
                        FakeKey::Modifier(Modifier::LeftAlt)
                    }
                    OutputKey::Keyboard(KeyboardKey::RightAlt) => {
                        FakeKey::Modifier(Modifier::RightAlt)
                    }
                    OutputKey::Keyboard(KeyboardKey::LeftShift) => {
                        FakeKey::Modifier(Modifier::LeftShift)
                    }
                    OutputKey::Keyboard(KeyboardKey::RightShift) => {
                        FakeKey::Modifier(Modifier::RightShift)
                    }
                    OutputKey::Keyboard(KeyboardKey::LeftSuper) => {
                        FakeKey::Modifier(Modifier::LeftSuper)
                    }
                    OutputKey::Keyboard(KeyboardKey::RightSuper) => {
                        FakeKey::Modifier(Modifier::RightSuper)
                    }
                    _ => unreachable!("function-key planner emitted an unsupported key"),
                };
                self.actions.push(FakeAction::SyntheticKey(FakeKeyEvent {
                    key,
                    active: event.active,
                }));
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
                self.available = false;
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
    fn fake_key_batch_preserves_one_ordered_transaction() -> Result<()> {
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        hardware.emit_key_events(&[
            SyntheticKeyEvent {
                key: OutputKey::Keyboard(KeyboardKey::LeftCtrl),
                active: true,
            },
            SyntheticKeyEvent {
                key: OutputKey::Keyboard(KeyboardKey::F2),
                active: true,
            },
            SyntheticKeyEvent {
                key: OutputKey::Keyboard(KeyboardKey::F2),
                active: false,
            },
            SyntheticKeyEvent {
                key: OutputKey::Keyboard(KeyboardKey::LeftCtrl),
                active: false,
            },
        ])?;

        assert_eq!(
            hardware.synthetic_transactions(),
            &[vec![
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false,
                },
            ]]
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
