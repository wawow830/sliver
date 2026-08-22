//! The concrete M2 Touch Bar adapter.
//!
//! This module owns all device-specific details: DRM mastership and scanout,
//! evdev readers, the virtual function-key device, and release. Callers see
//! only the small `TouchBarHardware` interface in `hardware.rs`.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use cairo::{Filter, ImageSurface, Operator};
use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{self, connector, crtc, framebuffer, Device as _, Mode};
use drm::Device as _;
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AttributeSet, EventType, InputEvent, Key};

use crate::hardware::{
    validate_backlight, HardwareEvent, LogicalFrame, Modifier, ModifierState, TouchBarHardware,
};

/// The panel's visible width; the buffer is padded to 64 for pitch sanity.
const PANEL_W: u32 = 60;
const FB_PAD: u32 = 4;

const TOUCH_DEV: &str = "/dev/input/event2";
const KEYBOARD_NAME: &str = "Apple MTP keyboard";
const F_KEYS: [Key; 12] = [
    Key::KEY_F1,
    Key::KEY_F2,
    Key::KEY_F3,
    Key::KEY_F4,
    Key::KEY_F5,
    Key::KEY_F6,
    Key::KEY_F7,
    Key::KEY_F8,
    Key::KEY_F9,
    Key::KEY_F10,
    Key::KEY_F11,
    Key::KEY_F12,
];
const MOD_KEYS: [Key; 8] = [
    Key::KEY_LEFTCTRL,
    Key::KEY_RIGHTCTRL,
    Key::KEY_LEFTALT,
    Key::KEY_RIGHTALT,
    Key::KEY_LEFTSHIFT,
    Key::KEY_RIGHTSHIFT,
    Key::KEY_LEFTMETA,
    Key::KEY_RIGHTMETA,
];

/// A small wrapper keeps the Linux polling dependency private to this adapter.
/// The crate currently gets libc transitively through evdev; it should be made
/// a direct sliverd dependency when this module is wired into the crate.
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }

    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn wait_for_input(
    touch: Option<&TouchInput>,
    keyboard: Option<&KeyboardInput>,
    timeout: Duration,
) -> io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        },
        libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        },
    ];
    let mut count = 0usize;

    if let Some(touch) = touch {
        fds[count] = libc::pollfd {
            fd: touch.device.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        count += 1;
    }
    if let Some(keyboard) = keyboard {
        fds[count] = libc::pollfd {
            fd: keyboard.device.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        count += 1;
    }

    if count == 0 {
        thread::sleep(timeout);
        return Ok(());
    }

    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    loop {
        let result = unsafe { libc::poll(fds.as_mut_ptr(), count as libc::nfds_t, timeout_ms) };
        if result >= 0 {
            return Ok(());
        }
        if io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
}

#[derive(Debug)]
struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl control::Device for Card {}

struct CardClaim {
    card: Card,
    conn: connector::Handle,
    crtc: control::crtc::Handle,
    old_crtc: Option<crtc::Info>,
    mode: Mode,
    released: bool,
}

/// Open card1, take DRM master, find the DSI Touch Bar, and find its CRTC.
fn claim_card() -> Result<CardClaim> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/dri/card1")
        .context("opening /dev/dri/card1 (need root, or the video group)")?;
    let card = Card(file);
    card.acquire_master_lock().context("becoming DRM master")?;

    let res = card.resource_handles().context("drm resources")?;
    let conn = res
        .connectors()
        .iter()
        .find_map(|h| {
            let info = card.get_connector(*h, true).ok()?;
            (info.state() == connector::State::Connected
                && info.interface() == connector::Interface::DSI)
                .then_some(info)
        })
        .context("no connected DSI connector")?;
    let mode = *conn.modes().first().context("connector has no modes")?;
    let (w, h) = mode.size();
    eprintln!("panel mode: {w}x{h}");

    let crtc = conn
        .current_encoder()
        .and_then(|e| card.get_encoder(e).ok())
        .and_then(|e| e.crtc())
        .or_else(|| res.crtcs().first().copied())
        .context("no crtc available")?;
    let old_crtc = card.get_crtc(crtc).ok();

    Ok(CardClaim {
        card,
        conn: conn.handle(),
        crtc,
        old_crtc,
        mode,
        released: false,
    })
}

impl CardClaim {
    fn show(&self, fb: framebuffer::Handle) -> Result<()> {
        self.card
            .set_crtc(self.crtc, Some(fb), (0, 0), &[self.conn], Some(self.mode))
            .context("setting CRTC")?;
        Ok(())
    }

    fn release(
        &mut self,
        fb: Option<framebuffer::Handle>,
        db: Option<control::dumbbuffer::DumbBuffer>,
    ) -> Result<()> {
        if self.released {
            return Ok(());
        }

        let mut first_error = None;
        if let Some(old) = &self.old_crtc {
            remember_error(
                &mut first_error,
                self.card.set_crtc(
                    self.crtc,
                    old.framebuffer(),
                    old.position(),
                    &[self.conn],
                    old.mode(),
                ),
            );
        }
        if let Some(fb) = fb {
            remember_error(&mut first_error, self.card.destroy_framebuffer(fb));
        }
        if let Some(db) = db {
            remember_error(&mut first_error, self.card.destroy_dumb_buffer(db));
        }
        remember_error(&mut first_error, self.card.release_master_lock());
        self.released = true;
        first_error.map_or(Ok(()), Err)
    }
}

fn remember_error<T, E>(first: &mut Option<anyhow::Error>, result: std::result::Result<T, E>)
where
    E: Into<anyhow::Error>,
{
    if let Err(error) = result {
        if first.is_none() {
            *first = Some(error.into());
        }
    }
}

impl Drop for CardClaim {
    fn drop(&mut self) {
        if let Err(error) = self.release(None, None) {
            eprintln!("DRM release failed: {error:#}");
        }
    }
}

fn normalize_touch_x(raw: i32, range: (i32, i32)) -> f64 {
    let (min, max) = range;
    let position = f64::from(raw - min) / f64::from((max - min).max(1));
    position.clamp(0.0, 1.0) * sliver_core::STRIP_W
}

struct TouchInput {
    device: evdev::Device,
    range: (i32, i32),
    last_x: Option<i32>,
    touching: bool,
    grabbed: bool,
}

impl TouchInput {
    fn open() -> io::Result<Self> {
        use evdev::AbsoluteAxisType;

        let mut device = evdev::Device::open(TOUCH_DEV)?;
        set_nonblocking(device.as_raw_fd())?;

        // Exclusive: taps on the strip are ours, not the compositor's cursor.
        let grabbed = match device.grab() {
            Ok(()) => true,
            Err(e) => {
                eprintln!("touch: grab failed: {e} (sharing, then)");
                false
            }
        };

        let range = device
            .get_abs_state()
            .ok()
            .and_then(|state| {
                let mt = &state[AbsoluteAxisType::ABS_MT_POSITION_X.0 as usize];
                let plain = &state[AbsoluteAxisType::ABS_X.0 as usize];
                let pick = if mt.maximum > mt.minimum { mt } else { plain };
                (pick.maximum > pick.minimum).then_some((pick.minimum, pick.maximum))
            })
            .unwrap_or((0, sliver_core::STRIP_W as i32));
        eprintln!("touch: x range {range:?}");

        Ok(Self {
            device,
            range,
            last_x: None,
            touching: false,
            grabbed,
        })
    }

    fn ungrab(&mut self) -> io::Result<()> {
        if !self.grabbed {
            return Ok(());
        }
        let result = self.device.ungrab();
        self.grabbed = false;
        result
    }

    fn drain(&mut self, output: &mut Vec<HardwareEvent>) -> io::Result<bool> {
        let events: Vec<InputEvent> = match self.device.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(e) => return Err(e),
        };

        if events.is_empty() {
            return Ok(false);
        }

        use evdev::AbsoluteAxisType;
        for event in events {
            match event.event_type() {
                EventType::ABSOLUTE => {
                    let code = event.code();
                    if code == AbsoluteAxisType::ABS_MT_POSITION_X.0
                        || code == AbsoluteAxisType::ABS_X.0
                    {
                        self.last_x = Some(event.value());
                    } else if code == AbsoluteAxisType::ABS_MT_TRACKING_ID.0 {
                        self.touching = event.value() >= 0;
                    }
                }
                EventType::KEY => {
                    if event.code() == Key::BTN_TOUCH.code() {
                        self.touching = event.value() == 1;
                    }
                }
                // A touch that just ended with a position on record is a
                // tap. If taps land mirrored, flip the mapping here.
                EventType::SYNCHRONIZATION if !self.touching => {
                    if let Some(raw) = self.last_x.take() {
                        output.push(HardwareEvent::TouchTap {
                            x: normalize_touch_x(raw, self.range),
                        });
                    }
                }
                _ => {}
            }
        }
        Ok(true)
    }
}

impl Drop for TouchInput {
    fn drop(&mut self) {
        if let Err(error) = self.ungrab() {
            eprintln!("touch: ungrab failed: {error}");
        }
    }
}

/// Find the internal keyboard by identity rather than assuming event1 will
/// remain event1 forever.
fn open_main_keyboard() -> io::Result<(PathBuf, evdev::Device)> {
    for entry in std::fs::read_dir("/dev/input")? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("event") {
            continue;
        }
        let Ok(device) = evdev::Device::open(&path) else {
            continue;
        };
        if device.name() == Some(KEYBOARD_NAME) {
            return Ok((path, device));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("input device {KEYBOARD_NAME:?} not found"),
    ))
}

struct KeyboardInput {
    path: PathBuf,
    device: evdev::Device,
}

impl KeyboardInput {
    fn open() -> io::Result<Self> {
        let (path, device) = open_main_keyboard()?;
        set_nonblocking(device.as_raw_fd())?;
        eprintln!("fn: watching {} ({KEYBOARD_NAME})", path.display());
        Ok(Self { path, device })
    }

    fn drain(&mut self, output: &mut Vec<HardwareEvent>) -> io::Result<bool> {
        let events: Vec<InputEvent> = match self.device.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(e) => return Err(e),
        };

        if events.is_empty() {
            return Ok(false);
        }

        for event in events {
            if event.event_type() != EventType::KEY {
                continue;
            }
            let active = match event.value() {
                1 => true,
                0 => false,
                _ => continue,
            };

            if event.code() == Key::KEY_FN.code() {
                output.push(HardwareEvent::Fn { active });
                continue;
            }

            if let Some(modifier) = modifier_for(event.code()) {
                output.push(HardwareEvent::Modifier { modifier, active });
            }
        }
        Ok(true)
    }
}

fn modifier_for(code: u16) -> Option<Modifier> {
    MOD_KEYS
        .into_iter()
        .zip(Modifier::ALL)
        .find_map(|(key, modifier)| (key.code() == code).then_some(modifier))
}

fn function_key_batches(
    index: usize,
    modifiers: ModifierState,
) -> io::Result<Vec<Vec<InputEvent>>> {
    let key = *F_KEYS.get(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "function-key index out of range",
        )
    })?;

    let modifier_down: Vec<_> = Modifier::ALL
        .into_iter()
        .zip(MOD_KEYS)
        .filter(|(modifier, _)| modifiers.is_active(*modifier))
        .map(|(_, key)| InputEvent::new(EventType::KEY, key.code(), 1))
        .collect();
    let modifier_up: Vec<_> = Modifier::ALL
        .into_iter()
        .zip(MOD_KEYS)
        .rev()
        .filter(|(modifier, _)| modifiers.is_active(*modifier))
        .map(|(_, key)| InputEvent::new(EventType::KEY, key.code(), 0))
        .collect();

    let mut batches = Vec::with_capacity(4);
    if !modifier_down.is_empty() {
        batches.push(modifier_down);
    }
    batches.push(vec![InputEvent::new(EventType::KEY, key.code(), 1)]);
    batches.push(vec![InputEvent::new(EventType::KEY, key.code(), 0)]);
    if !modifier_up.is_empty() {
        batches.push(modifier_up);
    }
    Ok(batches)
}

/// Virtual keyboard used solely to emit real F1-F12 key events.
struct FnEmitter {
    device: VirtualDevice,
}

impl FnEmitter {
    fn new() -> Result<Self> {
        let keys: AttributeSet<Key> = F_KEYS.into_iter().chain(MOD_KEYS).collect();
        let device = VirtualDeviceBuilder::new()?
            .name("Sliver Function Row")
            .with_keys(&keys)?
            .build()?;
        eprintln!("fn: virtual F-key keyboard ready");
        Ok(Self { device })
    }

    fn tap(&mut self, index: usize, modifiers: ModifierState) -> io::Result<()> {
        for batch in function_key_batches(index, modifiers)? {
            self.device.emit(&batch)?;
        }
        Ok(())
    }
}

fn copy_visible_rows(
    source: &[u8],
    source_stride: usize,
    destination: &mut [u8],
    destination_stride: usize,
    visible_row_bytes: usize,
    mode_height: usize,
) -> Result<()> {
    ensure!(
        visible_row_bytes <= source_stride && visible_row_bytes <= destination_stride,
        "visible row does not fit framebuffer stride"
    );
    ensure!(
        mode_height.saturating_mul(source_stride) <= source.len()
            && mode_height.saturating_mul(destination_stride) <= destination.len(),
        "visible framebuffer rows exceed allocated memory"
    );
    for row in 0..mode_height {
        let source_start = row * source_stride;
        let destination_start = row * destination_stride;
        destination[destination_start..destination_start + visible_row_bytes]
            .copy_from_slice(&source[source_start..source_start + visible_row_bytes]);
    }
    Ok(())
}

fn paint_logical_frame(frame: &LogicalFrame, physical: &ImageSurface) -> Result<()> {
    ensure!(
        frame.width() == sliver_core::STRIP_W as usize
            && frame.height() == sliver_core::STRIP_H as usize,
        "logical frame has unexpected dimensions: {}x{}",
        frame.width(),
        frame.height()
    );
    ensure!(
        frame.stride() >= frame.width() * 4,
        "logical frame stride is too small: {}",
        frame.stride()
    );
    let source = ImageSurface::create_for_data(
        frame.pixels().to_vec(),
        cairo::Format::ARgb32,
        frame.width() as i32,
        frame.height() as i32,
        frame.stride() as i32,
    )?;

    let context = cairo::Context::new(physical)?;
    // Probe-settled truth: buffer scanout rows run along the strip
    // left-to-right, scanlines bottom-to-top. The four-pixel pad never
    // shows. This preserves logical (x,y) -> buffer (60-y,x).
    context.translate(f64::from(PANEL_W), 0.0);
    context.rotate(std::f64::consts::FRAC_PI_2);
    context.set_source_surface(source, 0.0, 0.0)?;
    context.source().set_filter(Filter::Nearest);
    context.set_operator(Operator::Source);
    context.paint()?;
    physical.flush();
    Ok(())
}

pub(crate) struct M2TouchBar {
    claim: Option<CardClaim>,
    framebuffer: Option<framebuffer::Handle>,
    dumb_buffer: Option<control::dumbbuffer::DumbBuffer>,
    physical_surface: Option<ImageSurface>,
    shown: bool,
    touch: Option<TouchInput>,
    keyboard: Option<KeyboardInput>,
    fn_emitter: Option<FnEmitter>,
}

impl M2TouchBar {
    pub(crate) fn new() -> Self {
        Self {
            claim: None,
            framebuffer: None,
            dumb_buffer: None,
            physical_surface: None,
            shown: false,
            touch: None,
            keyboard: None,
            fn_emitter: None,
        }
    }

    fn is_claimed(&self) -> bool {
        self.claim.is_some()
    }

    fn claim_inner(&mut self) -> Result<()> {
        ensure!(!self.is_claimed(), "Touch Bar is already claimed");

        let mut claim = claim_card()?;
        let (pw, ph) = claim.mode.size();
        let (pw, ph) = (u32::from(pw), u32::from(ph));
        debug_assert_eq!(pw, PANEL_W);

        let mut dumb_buffer = None;
        let mut framebuffer = None;
        let physical_surface = (|| -> Result<ImageSurface> {
            let db = claim
                .card
                .create_dumb_buffer((pw + FB_PAD, ph), DrmFourcc::Xrgb8888, 32)?;
            dumb_buffer = Some(db);

            let fb = claim.card.add_framebuffer(
                dumb_buffer.as_ref().expect("dumb buffer was set"),
                24,
                32,
            )?;
            framebuffer = Some(fb);

            // Canvas spans only the visible panel width; the 4px pad is padding.
            let px_stride = pw * 4;
            let pixels = vec![0u8; (px_stride * ph) as usize];
            Ok(cairo::ImageSurface::create_for_data(
                pixels,
                cairo::Format::ARgb32,
                pw as i32,
                ph as i32,
                px_stride as i32,
            )?)
        })();

        let physical_surface = match physical_surface {
            Ok(surface) => surface,
            Err(error) => {
                if let Err(release_error) = claim.release(framebuffer, dumb_buffer) {
                    eprintln!("DRM release failed after claim error: {release_error:#}");
                }
                return Err(error);
            }
        };

        self.claim = Some(claim);
        self.framebuffer = framebuffer;
        self.dumb_buffer = dumb_buffer;
        self.physical_surface = Some(physical_surface);

        self.touch = match TouchInput::open() {
            Ok(touch) => Some(touch),
            Err(e) => {
                eprintln!("touch: can't open {TOUCH_DEV}: {e} (continuing untouchable)");
                None
            }
        };
        self.keyboard = match KeyboardInput::open() {
            Ok(keyboard) => Some(keyboard),
            Err(e) => {
                eprintln!("fn: can't find keyboard: {e}");
                None
            }
        };
        self.fn_emitter = match FnEmitter::new() {
            Ok(emitter) => Some(emitter),
            Err(e) => {
                eprintln!("fn: can't create virtual keyboard: {e:#}");
                None
            }
        };
        Ok(())
    }

    fn poll_inner(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        ensure!(self.is_claimed(), "Touch Bar is not claimed");
        wait_for_input(self.touch.as_ref(), self.keyboard.as_ref(), timeout)
            .context("polling Touch Bar input")?;

        let mut output = Vec::new();
        loop {
            let mut made_progress = false;

            // Preserve the old daemon's cross-device ordering: update Fn and
            // modifiers before interpreting a touch from the same poll.
            let keyboard_error = match self.keyboard.as_mut() {
                Some(keyboard) => match keyboard.drain(&mut output) {
                    Ok(progress) => {
                        made_progress |= progress;
                        None
                    }
                    Err(error) => Some(error),
                },
                None => None,
            };
            if let Some(error) = keyboard_error {
                if let Some(keyboard) = self.keyboard.as_ref() {
                    eprintln!(
                        "fn: keyboard reader stopped at {}: {error}",
                        keyboard.path.display()
                    );
                } else {
                    eprintln!("fn: keyboard reader stopped: {error}");
                }
                self.keyboard = None;
            }

            let touch_error = match self.touch.as_mut() {
                Some(touch) => match touch.drain(&mut output) {
                    Ok(progress) => {
                        made_progress |= progress;
                        None
                    }
                    Err(error) => Some(error),
                },
                None => None,
            };
            if let Some(error) = touch_error {
                eprintln!("touch: reader stopped: {error}");
                self.touch = None;
            }

            if !made_progress {
                break;
            }
        }
        Ok(output)
    }

    fn present_inner(&mut self, frame: &LogicalFrame) -> Result<()> {
        ensure!(self.is_claimed(), "Touch Bar is not claimed");
        let physical_surface = self
            .physical_surface
            .as_mut()
            .context("Touch Bar framebuffer is not initialized")?;
        paint_logical_frame(frame, physical_surface)?;

        let src = physical_surface.data()?;
        let row = (PANEL_W * 4) as usize;
        let sstride = row;
        let claim = self
            .claim
            .as_mut()
            .context("Touch Bar card claim is not initialized")?;
        let framebuffer = self
            .framebuffer
            .context("Touch Bar framebuffer handle is not initialized")?;
        let dumb_buffer = self
            .dumb_buffer
            .as_mut()
            .context("Touch Bar dumb buffer is not initialized")?;
        let dstride = dumb_buffer.pitch() as usize;
        // Paint by the mode's height. The driver rounds the allocation up
        // (2008 -> 2048), and those pad rows are not ours to touch.
        let height = u32::from(claim.mode.size().1) as usize;
        {
            let mut map = claim.card.map_dumb_buffer(dumb_buffer)?;
            copy_visible_rows(&src, sstride, map.as_mut(), dstride, row, height)?;
        }

        // Command-mode DSI: nothing reaches the glass until the framebuffer
        // is marked dirty. set_crtc flushes the first frame; heartbeats use
        // the dirty path after that.
        if let Err(e) = claim.card.dirty_framebuffer(
            framebuffer,
            &[control::ClipRect::new(0, 0, PANEL_W as u16, height as u16)],
        ) {
            eprintln!("dirty flush failed: {e}");
        }

        if !self.shown {
            claim.show(framebuffer)?;
            self.shown = true;
        }
        Ok(())
    }

    fn release_inner(&mut self) -> Result<()> {
        // Ungrab before dropping the device. Keep releasing the remaining
        // resources after an error, then report the first failure.
        let mut first_error = None;
        if let Some(touch) = self.touch.as_mut() {
            remember_error(&mut first_error, touch.ungrab());
        }
        self.touch = None;
        self.keyboard = None;
        self.fn_emitter = None;

        let framebuffer = self.framebuffer.take();
        let dumb_buffer = self.dumb_buffer.take();
        if let Some(mut claim) = self.claim.take() {
            remember_error(&mut first_error, claim.release(framebuffer, dumb_buffer));
        }
        self.physical_surface = None;
        self.shown = false;
        first_error.map_or(Ok(()), Err)
    }
}

impl Default for M2TouchBar {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for M2TouchBar {
    fn drop(&mut self) {
        if let Err(error) = self.release_inner() {
            eprintln!("Touch Bar release failed: {error:#}");
        }
    }
}

impl TouchBarHardware for M2TouchBar {
    fn claim(&mut self) -> Result<()> {
        self.claim_inner()
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        self.poll_inner(timeout)
    }

    fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
        self.present_inner(frame)
    }

    fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()> {
        ensure!(self.is_claimed(), "Touch Bar is not claimed");
        if let Some(emitter) = self.fn_emitter.as_mut() {
            if let Err(error) = emitter.tap(index, modifiers) {
                eprintln!("fn: failed to emit F{}: {error}", index + 1);
            }
        }
        Ok(())
    }

    fn set_backlight(&mut self, level: f64) -> Result<()> {
        ensure!(self.is_claimed(), "Touch Bar is not claimed");
        validate_backlight(level)?;

        const DIRECTORY: &str = "/sys/class/backlight/228600000.dsi.0";
        const BRIGHTNESS: &str = "/sys/class/backlight/228600000.dsi.0/brightness";
        const MAX_BRIGHTNESS: &str = "/sys/class/backlight/228600000.dsi.0/max_brightness";

        let max = std::fs::read_to_string(MAX_BRIGHTNESS)
            .with_context(|| format!("reading {DIRECTORY}/max_brightness"))?
            .trim()
            .parse::<u32>()
            .with_context(|| format!("parsing {DIRECTORY}/max_brightness"))?;
        let value = (level * f64::from(max)).round() as u32;
        let mut brightness = OpenOptions::new()
            .write(true)
            .open(BRIGHTNESS)
            .with_context(|| format!("opening {BRIGHTNESS}"))?;
        write!(brightness, "{value}").with_context(|| format!("writing {BRIGHTNESS}"))?;
        Ok(())
    }

    fn release(&mut self) -> Result<()> {
        self.release_inner()
    }
}

/// Calibration pattern, straight into the raw buffer. One photo of the
/// glass tells us exactly how memory maps to pixels:
///   red    — the first 200 rows of scanout
///   blue   — the last 200 rows
///   green  — columns 0..20 (start of each scanline)
///   white  — columns 40..60 (end of the visible window)
///   magenta— the 4 padding columns (should never be visible)
pub(crate) fn probe() -> Result<()> {
    let mut claim = claim_card()?;
    let (pw, ph) = claim.mode.size();
    let (pw, ph) = (u32::from(pw), u32::from(ph));
    let framebuffer_width = pw + FB_PAD;

    let mut dumb_buffer =
        match claim
            .card
            .create_dumb_buffer((framebuffer_width, ph), DrmFourcc::Xrgb8888, 32)
        {
            Ok(dumb_buffer) => dumb_buffer,
            Err(error) => {
                if let Err(release_error) = claim.release(None, None) {
                    eprintln!("DRM release failed after probe error: {release_error:#}");
                }
                return Err(error.into());
            }
        };
    let framebuffer = match claim.card.add_framebuffer(&dumb_buffer, 24, 32) {
        Ok(framebuffer) => framebuffer,
        Err(error) => {
            if let Err(release_error) = claim.release(None, Some(dumb_buffer)) {
                eprintln!("DRM release failed after probe error: {release_error:#}");
            }
            return Err(error.into());
        }
    };

    const RED: u32 = 0xffff_0000;
    const BLUE: u32 = 0xff00_00ff;
    const GREEN: u32 = 0xff00_ff00;
    const WHITE: u32 = 0xffff_ffff;
    const MAGENTA: u32 = 0xffff_00ff;
    const BASE: u32 = 0xff20_2020;

    let result = (|| -> Result<()> {
        let pitch = dumb_buffer.pitch() as usize;
        {
            let mut map = claim.card.map_dumb_buffer(&mut dumb_buffer)?;
            let data: &mut [u8] = map.as_mut();
            for row in 0..ph as usize {
                for col in 0..framebuffer_width as usize {
                    let px = if col >= pw as usize {
                        MAGENTA
                    } else if row < 200 {
                        RED
                    } else if row >= ph as usize - 200 {
                        BLUE
                    } else if col < 20 {
                        GREEN
                    } else if col >= 40 {
                        WHITE
                    } else {
                        BASE
                    };
                    let off = row * pitch + col * 4;
                    data[off..off + 4].copy_from_slice(&px.to_le_bytes());
                }
            }
        }

        claim.show(framebuffer)?;
        eprintln!("probe on glass: red/blue ends, green/white flanks, magenta pad");
        hold()
    })();

    let release_result = claim.release(Some(framebuffer), Some(dumb_buffer));
    match (result, release_result) {
        (Err(error), Err(release_error)) => {
            eprintln!("DRM release failed after probe error: {release_error:#}");
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("releasing probe hardware"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Hold until Ctrl-C (probe's simpler pulse).
fn hold() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    eprintln!("holding — Ctrl-C to let go");
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;
    while running.load(Ordering::SeqCst) {
        thread::park_timeout(Duration::from_millis(250));
    }
    eprintln!("releasing the strip");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framebuffer_copy_uses_visible_width_and_mode_height() -> Result<()> {
        let source = vec![
            1, 1, 1, 1, 1, 1, 1, 1, // visible row 0
            2, 2, 2, 2, 2, 2, 2, 2, // visible row 1
            3, 3, 3, 3, 3, 3, 3, 3, // allocation-only source row
        ];
        let mut destination = vec![0xaa; 4 * 12];

        copy_visible_rows(&source, 8, &mut destination, 12, 8, 2)?;

        assert_eq!(&destination[0..8], &[1; 8]);
        assert_eq!(&destination[8..12], &[0xaa; 4]);
        assert_eq!(&destination[12..20], &[2; 8]);
        assert_eq!(&destination[20..], &[0xaa; 28]);
        Ok(())
    }

    #[test]
    fn touch_x_is_normalized_and_clamped_to_the_logical_strip() {
        let range = (0, 23_044);
        assert_eq!(normalize_touch_x(-1, range), 0.0);
        assert_eq!(normalize_touch_x(0, range), 0.0);
        assert_eq!(normalize_touch_x(11_522, range), 1004.0);
        assert_eq!(normalize_touch_x(23_044, range), 2008.0);
        assert_eq!(normalize_touch_x(24_000, range), 2008.0);
    }

    #[test]
    fn function_key_batches_bridge_modifiers_in_one_device() -> Result<()> {
        let mut modifiers = ModifierState::default();
        modifiers.set(Modifier::LeftCtrl, true);
        modifiers.set(Modifier::RightAlt, true);

        let batches = function_key_batches(1, modifiers)?;
        let observed: Vec<Vec<(u16, i32)>> = batches
            .iter()
            .map(|batch| {
                batch
                    .iter()
                    .map(|event| (event.code(), event.value()))
                    .collect()
            })
            .collect();
        assert_eq!(
            observed,
            vec![
                vec![(Key::KEY_LEFTCTRL.code(), 1), (Key::KEY_RIGHTALT.code(), 1),],
                vec![(Key::KEY_F2.code(), 1)],
                vec![(Key::KEY_F2.code(), 0)],
                vec![(Key::KEY_RIGHTALT.code(), 0), (Key::KEY_LEFTCTRL.code(), 0),],
            ]
        );
        Ok(())
    }

    #[test]
    fn logical_top_and_bottom_map_to_the_m2_panel_flanks() -> Result<()> {
        let logical = ImageSurface::create(
            cairo::Format::ARgb32,
            sliver_core::STRIP_W as i32,
            sliver_core::STRIP_H as i32,
        )?;
        let context = cairo::Context::new(&logical)?;
        context.set_source_rgb(0.0, 0.0, 0.0);
        context.paint()?;
        context.set_source_rgb(1.0, 0.0, 0.0);
        context.rectangle(0.0, 0.0, sliver_core::STRIP_W, 10.0);
        context.fill()?;
        context.set_source_rgb(0.0, 0.0, 1.0);
        context.rectangle(0.0, 50.0, sliver_core::STRIP_W, 10.0);
        context.fill()?;
        logical.flush();

        let physical = ImageSurface::create(
            cairo::Format::ARgb32,
            PANEL_W as i32,
            sliver_core::STRIP_W as i32,
        )?;
        paint_logical_frame(&LogicalFrame::from_surface(&logical)?, &physical)?;
        assert_eq!(rgba_at(&physical, 55, 1000)?, [255, 0, 0, 255]);
        assert_eq!(rgba_at(&physical, 5, 1000)?, [0, 0, 255, 255]);
        Ok(())
    }

    fn rgba_at(surface: &ImageSurface, x: usize, y: usize) -> Result<[u8; 4]> {
        let mut value = 0;
        surface.with_data(|pixels| {
            let offset = y * surface.stride() as usize + x * 4;
            value = u32::from_ne_bytes(
                pixels[offset..offset + 4]
                    .try_into()
                    .expect("ARGB32 pixel is four bytes"),
            );
        })?;
        Ok([
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
            ((value >> 24) & 0xff) as u8,
        ])
    }
}
