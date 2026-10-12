//! Fake hardware for tests and headless runs. Each fake is a cheap handle around shared state:
//! clone it, give one clone to the daemon, and inspect the other.

use std::cell::RefCell;
use std::rc::Rc;

use sliver_render::Pixmap;
use sliver_render::panel::{Orientation, Span, blit_xrgb8888};

use crate::backend::{Backends, Backlight, Display, Keys, PixelSource};

/// A panel with a real XRGB8888 framebuffer in its native orientation (60x2008 on Apple
/// Silicon), written through the same `panel::blit_xrgb8888` path a DRM backend uses.
#[derive(Clone)]
pub struct FakeDisplay(Rc<RefCell<FakeDisplayState>>);

pub struct FakeDisplayState {
    pub orientation: Orientation,
    /// Logical (landscape) size.
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub fb: Vec<u8>,
    /// Damage of every flush so far.
    pub flushes: Vec<Vec<Span>>,
}

impl FakeDisplay {
    pub fn new(width: u32, height: u32, orientation: Orientation) -> FakeDisplay {
        let (pw, ph) = orientation.panel_size(width, height);
        let stride = pw as usize * 4;
        FakeDisplay(Rc::new(RefCell::new(FakeDisplayState {
            orientation,
            width,
            height,
            stride,
            fb: vec![0; stride * ph as usize],
            flushes: Vec::new(),
        })))
    }

    pub fn state(&self) -> std::cell::Ref<'_, FakeDisplayState> {
        self.0.borrow()
    }

    pub fn flush_count(&self) -> usize {
        self.0.borrow().flushes.len()
    }

    /// Reads the framebuffer back into a landscape bar image, as the user would see it.
    pub fn frame(&self) -> Pixmap {
        let s = self.0.borrow();
        let mut out = Pixmap::new(s.width, s.height).expect("non-empty panel");
        let w = s.width as usize;
        let data = out.data_mut();
        for y in 0..s.height as usize {
            for x in 0..w {
                let off = match s.orientation {
                    Orientation::Landscape => y * s.stride + x * 4,
                    Orientation::Rotate90 => x * s.stride + (s.height as usize - 1 - y) * 4,
                };
                let px = &s.fb[off..off + 4];
                data[(y * w + x) * 4..][..4].copy_from_slice(&[px[2], px[1], px[0], 255]);
            }
        }
        out
    }
}

impl Display for FakeDisplay {
    fn flush(&mut self, frame: &Pixmap, damage: &[Span]) {
        let mut s = self.0.borrow_mut();
        assert_eq!((frame.width(), frame.height()), (s.width, s.height));
        assert!(!damage.is_empty(), "flush without damage");
        let (orientation, stride) = (s.orientation, s.stride);
        for span in damage {
            blit_xrgb8888(frame, orientation, Some(*span), &mut s.fb, stride);
        }
        s.flushes.push(damage.to_vec());
    }
}

/// Backlight that records every value written.
#[derive(Clone, Default)]
pub struct FakeBacklight(Rc<RefCell<FakeBacklightState>>);

#[derive(Default)]
pub struct FakeBacklightState {
    pub writes: Vec<u32>,
    pub screen: Option<f32>,
}

impl FakeBacklight {
    pub fn current(&self) -> Option<u32> {
        self.0.borrow().writes.last().copied()
    }
    pub fn writes(&self) -> Vec<u32> {
        self.0.borrow().writes.clone()
    }
    pub fn set_screen(&self, fraction: Option<f32>) {
        self.0.borrow_mut().screen = fraction;
    }
}

impl Backlight for FakeBacklight {
    fn set(&mut self, value: u32) {
        self.0.borrow_mut().writes.push(value);
    }
    fn screen_fraction(&mut self) -> Option<f32> {
        self.0.borrow().screen
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Down(u16),
    Up(u16),
    Sync,
}

/// uinput stand-in that records emitted events.
#[derive(Clone, Default)]
pub struct FakeKeys(Rc<RefCell<Vec<KeyEvent>>>);

impl FakeKeys {
    /// Returns and clears everything emitted so far, without `Sync`s.
    pub fn take(&self) -> Vec<KeyEvent> {
        self.0
            .borrow_mut()
            .drain(..)
            .filter(|e| *e != KeyEvent::Sync)
            .collect()
    }
    /// Returns and clears everything emitted so far, including `Sync`s.
    pub fn take_raw(&self) -> Vec<KeyEvent> {
        self.0.borrow_mut().drain(..).collect()
    }
}

impl Keys for FakeKeys {
    fn key(&mut self, code: u16, down: bool) {
        self.0.borrow_mut().push(if down {
            KeyEvent::Down(code)
        } else {
            KeyEvent::Up(code)
        });
    }
    fn sync(&mut self) {
        self.0.borrow_mut().push(KeyEvent::Sync);
    }
}

/// An agent buffer (tightly packed XRGB8888) the test can keep writing to after attaching it.
#[derive(Clone)]
pub struct FakeShm(pub Rc<RefCell<Vec<u8>>>);

impl FakeShm {
    pub fn new(len: usize) -> FakeShm {
        FakeShm(Rc::new(RefCell::new(vec![0; len])))
    }

    /// Writes `frame` as tightly packed landscape XRGB8888 (stride = width * 4), the layout the
    /// agent sends.
    pub fn from_pixmap(frame: &Pixmap) -> FakeShm {
        let shm = FakeShm::new(frame.width() as usize * frame.height() as usize * 4);
        shm.write(frame);
        shm
    }

    pub fn write(&self, frame: &Pixmap) {
        let mut buf = self.0.borrow_mut();
        let stride = frame.width() as usize * 4;
        blit_xrgb8888(frame, Orientation::Landscape, None, &mut buf, stride);
    }
}

impl PixelSource for FakeShm {
    fn copy_tight(&self, dst: &mut Vec<u8>) {
        dst.clear();
        dst.extend_from_slice(&self.0.borrow());
    }
}

/// All fakes for one panel, plus the [`Backends`] to hand to the daemon.
#[derive(Clone)]
pub struct FakeHardware {
    pub display: FakeDisplay,
    pub backlight: FakeBacklight,
    pub keys: FakeKeys,
}

impl FakeHardware {
    /// A rotated panel like every Touch Bar (portrait framebuffer).
    pub fn new(width: u32, height: u32) -> FakeHardware {
        FakeHardware {
            display: FakeDisplay::new(width, height, Orientation::Rotate90),
            backlight: FakeBacklight::default(),
            keys: FakeKeys::default(),
        }
    }

    pub fn backends(&self) -> Backends {
        Backends {
            display: Box::new(self.display.clone()),
            backlight: Box::new(self.backlight.clone()),
            keys: Box::new(self.keys.clone()),
        }
    }
}
