//! Hardware seams. Everything sliverd touches on the machine goes through one of these traits, so
//! the daemon core runs unchanged against the fakes in [`crate::fake`] (tests, headless runs) and
//! against the real DRM/evdev/uinput/sysfs backends (only ever through the safe-swap harness).

use sliver_ipc::proto::TouchPhase;
use sliver_render::Pixmap;
use sliver_render::panel::Span;

/// The Touch Bar panel. Frames arrive in logical (landscape) bar coordinates, already composed.
/// The backend rotates them into its framebuffer (`panel::blit_xrgb8888`) and flushes only the
/// damaged columns (DIRTYFB / `FB_DAMAGE_CLIPS`).
pub trait Display {
    /// Shows `frame`. `damage` lists the bar columns that changed since the previous flush, and is
    /// never empty. The first flush after startup covers the whole bar.
    fn flush(&mut self, frame: &Pixmap, damage: &[Span]);
}

/// The Touch Bar backlight, in device units `0..=max` (`max_brightness` from sysfs: 255 on Apple
/// Silicon, 2 on T2).
pub trait Backlight {
    fn set(&mut self, value: u32);
    /// Main display brightness as a fraction of its own `max_brightness`, for `follow_screen`.
    /// `None` if there's no main backlight.
    fn screen_fraction(&mut self) -> Option<f32>;
}

/// Key output (uinput). The device must be registered for every emittable key up front, so layout
/// changes never need a new device.
pub trait Keys {
    fn key(&mut self, code: u16, down: bool);
    /// `SYN_REPORT`: everything since the last sync becomes visible at once (combos).
    fn sync(&mut self);
}

/// The hardware the daemon drives.
pub struct Backends {
    pub display: Box<dyn Display>,
    pub backlight: Box<dyn Backlight>,
    pub keys: Box<dyn Keys>,
}

/// One digitizer contact, already mapped to logical bar pixels (x along the bar, 0..width).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchEvent {
    /// Multitouch slot. Independent slots make chords work.
    pub slot: u8,
    pub phase: TouchPhase,
    pub x: f32,
    pub y: f32,
    pub time_us: u64,
}

/// A sealed shared-memory pixel buffer from the agent (`attach_buffer`), already imported by the
/// transport glue (seals and size checked, see [`crate::ipc::import_attachment`]). The agent may
/// still be writing to it, which is why sliverd copies on `present`.
pub trait PixelSource {
    /// Copies the frame out, tightly packed (`width * 4` bytes per row), replacing `dst`.
    fn copy_tight(&self, dst: &mut Vec<u8>);
}

/// Input from the hardware: the bar digitizer plus seat0 (Fn, lid, other activity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HwEvent {
    Touch(TouchEvent),
    FnKey {
        held: bool,
    },
    /// Any other user input on seat0 (keyboard, pointer): keeps the bar awake.
    Activity,
    Lid {
        closed: bool,
    },
    /// The main display's brightness changed (adaptive bar brightness, no polling).
    ScreenBacklight,
}

/// A pollable input source. The event loop polls [`std::os::fd::AsFd::as_fd`] for readability
/// and then calls [`InputSource::drain`], which must not block.
pub trait InputSource: std::os::fd::AsFd {
    fn drain(&mut self) -> Vec<HwEvent>;
}
