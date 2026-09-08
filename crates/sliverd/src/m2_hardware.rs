//! The concrete M2 Touch Bar adapter.
//!
//! This module owns all device-specific details: DRM mastership and scanout,
//! evdev readers, the virtual function-key device, and release. Callers see
//! only the small `TouchBarHardware` interface in `hardware.rs`.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::raw::c_void;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use cairo::{Filter, ImageSurface, Operator};
use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{self, connector, crtc, framebuffer, Device as _, Mode};
use drm::Device as _;
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AttributeSet, EventType, InputEvent, Key};

use crate::hardware::{
    output_key_metadata, validate_backlight, ConsumerKey, HardwareCapability, HardwareEvent,
    InputState, KeyboardKey, LogicalFrame, Modifier, ModifierState, OutputKey, SyntheticKeyEvent,
    TouchBarHardware,
};

/// The panel's visible width; the buffer is padded to 64 for pitch sanity.
const PANEL_W: u32 = 60;
const FB_PAD: u32 = 4;

/// A small wrapper keeps the Linux polling dependency private to this adapter.
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
    sleep_monitor: Option<&SleepMonitor>,
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
    if let Some(sleep_monitor) = sleep_monitor {
        fds[count] = libc::pollfd {
            fd: sleep_monitor.fd(),
            events: sleep_monitor.events(),
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

#[repr(C)]
struct SdBus {
    _private: [u8; 0],
}

#[repr(C)]
struct SdBusMessage {
    _private: [u8; 0],
}

#[repr(C)]
struct SdBusSlot {
    _private: [u8; 0],
}

/// Receives login1's PrepareForSleep signal without making suspend a worker
/// concern. The broker's normal hardware poll remains the only event pump.
struct PendingSleepStates {
    preparing: Vec<bool>,
}

struct SleepMonitor {
    bus: *mut SdBus,
    slot: *mut SdBusSlot,
    events: Box<PendingSleepStates>,
}

unsafe extern "C" fn prepare_for_sleep(
    message: *mut SdBusMessage,
    userdata: *mut c_void,
    _error: *mut c_void,
) -> libc::c_int {
    let mut preparing = 0;
    let signature = b"b\0";
    if ffi::sd_bus_message_read(message, signature.as_ptr().cast(), &mut preparing) >= 0 {
        // userdata is a boxed queue whose address remains stable for the
        // lifetime of the subscription.
        (&mut *userdata.cast::<PendingSleepStates>())
            .preparing
            .push(preparing != 0);
    }
    0
}

impl SleepMonitor {
    fn new() -> Result<Self> {
        let mut bus = std::ptr::null_mut();
        let result = unsafe { ffi::sd_bus_open_system(&mut bus) };
        ensure!(result >= 0, "opening the system D-Bus: {result}");
        let mut monitor = Self {
            bus,
            slot: std::ptr::null_mut(),
            events: Box::new(PendingSleepStates {
                preparing: Vec::new(),
            }),
        };
        let match_rule = b"type='signal',sender='org.freedesktop.login1',interface='org.freedesktop.login1.Manager',member='PrepareForSleep'\0";
        let result = unsafe {
            ffi::sd_bus_add_match(
                monitor.bus,
                &mut monitor.slot,
                match_rule.as_ptr().cast(),
                Some(prepare_for_sleep),
                (&mut *monitor.events as *mut PendingSleepStates).cast(),
            )
        };
        if result < 0 {
            unsafe {
                ffi::sd_bus_unref(monitor.bus);
            }
            bail!("subscribing to PrepareForSleep: {result}");
        }
        ensure!(
            unsafe { ffi::sd_bus_get_fd(monitor.bus) } >= 0,
            "system D-Bus has no pollable descriptor"
        );
        Ok(monitor)
    }

    fn fd(&self) -> RawFd {
        unsafe { ffi::sd_bus_get_fd(self.bus) }
    }

    fn events(&self) -> libc::c_short {
        unsafe { ffi::sd_bus_get_events(self.bus) as libc::c_short }
    }

    fn drain(&mut self, output: &mut Vec<HardwareEvent>) -> Result<()> {
        loop {
            let mut message = std::ptr::null_mut();
            let result = unsafe { ffi::sd_bus_process(self.bus, &mut message) };
            if !message.is_null() {
                unsafe {
                    ffi::sd_bus_message_unref(message);
                }
            }
            ensure!(result >= 0, "processing the system D-Bus: {result}");
            if result == 0 {
                break;
            }
        }
        for preparing in self.events.preparing.drain(..) {
            output.push(HardwareEvent::Visibility {
                visible: !preparing,
            });
        }
        Ok(())
    }
}

impl Drop for SleepMonitor {
    fn drop(&mut self) {
        unsafe {
            if !self.slot.is_null() {
                ffi::sd_bus_slot_unref(self.slot);
            }
            if !self.bus.is_null() {
                ffi::sd_bus_unref(self.bus);
            }
        }
    }
}

mod ffi {
    use super::{SdBus, SdBusMessage, SdBusSlot};
    use std::os::raw::{c_char, c_void};

    pub(super) type MessageHandler = unsafe extern "C" fn(
        message: *mut SdBusMessage,
        userdata: *mut c_void,
        error: *mut c_void,
    ) -> libc::c_int;

    extern "C" {
        pub(super) fn sd_bus_open_system(bus: *mut *mut SdBus) -> libc::c_int;
        pub(super) fn sd_bus_add_match(
            bus: *mut SdBus,
            slot: *mut *mut SdBusSlot,
            match_rule: *const c_char,
            callback: Option<MessageHandler>,
            userdata: *mut c_void,
        ) -> libc::c_int;
        pub(super) fn sd_bus_get_fd(bus: *mut SdBus) -> libc::c_int;
        pub(super) fn sd_bus_get_events(bus: *mut SdBus) -> libc::c_int;
        pub(super) fn sd_bus_process(
            bus: *mut SdBus,
            message: *mut *mut SdBusMessage,
        ) -> libc::c_int;
        pub(super) fn sd_bus_message_read(
            message: *mut SdBusMessage,
            types: *const c_char,
            ...
        ) -> libc::c_int;
        pub(super) fn sd_bus_message_unref(message: *mut SdBusMessage) -> *mut SdBusMessage;
        pub(super) fn sd_bus_slot_unref(slot: *mut SdBusSlot) -> *mut SdBusSlot;
        pub(super) fn sd_bus_unref(bus: *mut SdBus) -> *mut SdBus;
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

fn is_numbered_device(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
}

fn numbered_device_paths(directory: &str, prefix: &str) -> io::Result<Vec<PathBuf>> {
    let mut paths = std::fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| is_numbered_device(name, prefix))
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

/// Find the card that exposes the connected Touch Bar DSI connector. Card
/// numbers are assigned dynamically, so they are not part of the hardware
/// profile.
fn claim_card() -> Result<CardClaim> {
    let paths = numbered_device_paths("/dev/dri", "card")?;
    let mut last_error = None;
    for path in &paths {
        match claim_card_at(path) {
            Ok(claim) => return Ok(claim),
            Err(error) => last_error = Some(error.context(format!("probing {}", path.display()))),
        }
    }
    match last_error {
        Some(error) => Err(error.context("no connected DSI Touch Bar connector found")),
        None => bail!("no DRM card devices found"),
    }
}

fn validate_panel_mode(width: u32, height: u32) -> Result<()> {
    ensure!(
        (width, height) == (PANEL_W, crate::DISPLAY_WIDTH as u32),
        "unsupported Touch Bar mode {width}x{height}; expected {PANEL_W}x{}",
        crate::DISPLAY_WIDTH
    );
    Ok(())
}

fn claim_card_at(path: &Path) -> Result<CardClaim> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {} (need the sliver-drm group)", path.display()))?;
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
    validate_panel_mode(u32::from(w), u32::from(h))?;
    eprintln!("panel mode: {w}x{h} ({})", path.display());

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
    let Err(error) = result else {
        return;
    };
    remember_release_error(first, error.into());
}

fn remember_release_error(first: &mut Option<anyhow::Error>, error: anyhow::Error) {
    // Releasing hardware may race with the device or the previous owner's
    // DRM objects disappearing. Those resources are already released.
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.raw_os_error() == Some(libc::ENOENT))
    }) {
        return;
    }
    if first.is_none() {
        *first = Some(error);
    }
}

impl Drop for CardClaim {
    fn drop(&mut self) {
        if let Err(error) = self.release(None, None) {
            crate::system_log::broker_error(format!("DRM release failed: {error:#}"));
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct AxisRange {
    min: i32,
    max: i32,
}

impl AxisRange {
    fn new(min: i32, max: i32) -> Option<Self> {
        (max > min).then_some(Self { min, max })
    }

    fn normalize(self, raw: i32) -> f64 {
        let position =
            (f64::from(raw) - f64::from(self.min)) / (f64::from(self.max) - f64::from(self.min));
        position.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
fn normalize_axis(raw: i32, range: (i32, i32), extent: f64) -> f64 {
    AxisRange::new(range.0, range.1).map_or(0.0, |range| range.normalize(raw) * extent)
}

#[cfg(test)]
fn normalize_touch_x(raw: i32, range: (i32, i32)) -> f64 {
    normalize_axis(raw, range, crate::DISPLAY_WIDTH_F64)
}

fn normalize_backlight_level(current: u32, maximum: u32) -> Result<f64> {
    ensure!(maximum > 0, "backlight maximum must be positive");
    ensure!(
        current <= maximum,
        "backlight value {current} exceeds maximum {maximum}"
    );
    Ok(f64::from(current) / f64::from(maximum))
}

fn backlight_value(level: f64, maximum: u32) -> Result<u32> {
    validate_backlight(level)?;
    ensure!(maximum > 0, "backlight maximum must be positive");
    Ok((level * f64::from(maximum)).round() as u32)
}

struct Backlight {
    directory: PathBuf,
    brightness: PathBuf,
    maximum: PathBuf,
}

impl Backlight {
    fn discover() -> Result<Self> {
        let mut candidates = std::fs::read_dir("/sys/class/backlight")?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("dsi"))
            })
            .filter_map(|directory| {
                let brightness = directory.join("brightness");
                let maximum = directory.join("max_brightness");
                (brightness.exists() && maximum.exists()).then_some(Self {
                    directory,
                    brightness,
                    maximum,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.directory.cmp(&right.directory));
        candidates
            .into_iter()
            .next()
            .context("no DSI backlight found")
    }

    fn values(&self) -> Result<(u32, u32)> {
        let maximum = std::fs::read_to_string(&self.maximum)
            .with_context(|| format!("reading {}/max_brightness", self.directory.display()))?
            .trim()
            .parse::<u32>()
            .with_context(|| format!("parsing {}/max_brightness", self.directory.display()))?;
        let current = std::fs::read_to_string(&self.brightness)
            .with_context(|| format!("reading {}/brightness", self.directory.display()))?
            .trim()
            .parse::<u32>()
            .with_context(|| format!("parsing {}/brightness", self.directory.display()))?;
        Ok((current, maximum))
    }

    fn set(&self, level: f64) -> Result<()> {
        let (_, maximum) = self.values()?;
        let value = backlight_value(level, maximum)?;
        let mut brightness = OpenOptions::new()
            .write(true)
            .open(&self.brightness)
            .with_context(|| format!("opening {}", self.brightness.display()))?;
        write!(brightness, "{value}")
            .with_context(|| format!("writing {}", self.brightness.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct TouchSlot {
    id: Option<crate::hardware::ContactId>,
    x: i32,
    y: i32,
    pressure: Option<i32>,
    width: Option<i32>,
    height: Option<i32>,
    phase: Option<crate::hardware::TouchPhase>,
    changed: bool,
}

#[derive(Debug, Clone, Copy)]
struct TouchProfile {
    slot_min: i32,
    slot_count: usize,
    x_range: AxisRange,
    y_range: AxisRange,
    pressure_range: Option<AxisRange>,
    width_range: Option<AxisRange>,
    height_range: Option<AxisRange>,
    single_touch_fallback: bool,
}

struct TouchState {
    slots: Vec<TouchSlot>,
    profile: TouchProfile,
    current_slot: usize,
    started: std::time::Instant,
}

impl TouchState {
    fn new(profile: TouchProfile) -> Self {
        Self {
            slots: vec![TouchSlot::default(); profile.slot_count.max(1)],
            profile,
            current_slot: 0,
            started: std::time::Instant::now(),
        }
    }

    fn process(
        &mut self,
        event: InputEvent,
        modifiers: ModifierState,
        output: &mut Vec<crate::hardware::TouchEvent>,
    ) {
        use evdev::{AbsoluteAxisType, Synchronization};

        match event.event_type() {
            EventType::ABSOLUTE => match event.code() {
                code if code == AbsoluteAxisType::ABS_MT_SLOT.0 => {
                    self.current_slot =
                        event.value().saturating_sub(self.profile.slot_min).max(0) as usize;
                    if self.current_slot >= self.slots.len() {
                        self.current_slot = self.slots.len() - 1;
                    }
                }
                code if code == AbsoluteAxisType::ABS_MT_TRACKING_ID.0 => {
                    let index = self.current_slot;
                    if event.value() < 0 {
                        if self.slots[index].id.is_some() {
                            self.slots[index].phase = Some(crate::hardware::TouchPhase::Up);
                        }
                    } else {
                        let id = event.value() as crate::hardware::ContactId;
                        if self.slots[index].id.is_some_and(|old_id| old_id != id) {
                            self.emit_slot(
                                index,
                                crate::hardware::TouchPhase::Cancel,
                                modifiers,
                                output,
                            );
                        }
                        let slot = &mut self.slots[index];
                        slot.id = Some(id);
                        slot.phase = Some(crate::hardware::TouchPhase::Down);
                        slot.changed = false;
                    }
                }
                code if code == AbsoluteAxisType::ABS_MT_POSITION_X.0
                    || (self.profile.single_touch_fallback
                        && code == AbsoluteAxisType::ABS_X.0) =>
                {
                    self.slots[self.current_slot].x = event.value();
                    self.slots[self.current_slot].changed = true;
                }
                code if code == AbsoluteAxisType::ABS_MT_POSITION_Y.0
                    || (self.profile.single_touch_fallback
                        && code == AbsoluteAxisType::ABS_Y.0) =>
                {
                    self.slots[self.current_slot].y = event.value();
                    self.slots[self.current_slot].changed = true;
                }
                code if code == AbsoluteAxisType::ABS_MT_PRESSURE.0
                    || (self.profile.single_touch_fallback
                        && code == AbsoluteAxisType::ABS_PRESSURE.0) =>
                {
                    self.slots[self.current_slot].pressure = Some(event.value());
                    self.slots[self.current_slot].changed = true;
                }
                code if code == AbsoluteAxisType::ABS_MT_TOUCH_MAJOR.0 => {
                    self.slots[self.current_slot].width = Some(event.value());
                    self.slots[self.current_slot].changed = true;
                }
                code if code == AbsoluteAxisType::ABS_MT_TOUCH_MINOR.0 => {
                    self.slots[self.current_slot].height = Some(event.value());
                    self.slots[self.current_slot].changed = true;
                }
                _ => {}
            },
            EventType::KEY
                if self.profile.single_touch_fallback && event.code() == Key::BTN_TOUCH.code() =>
            {
                let slot = &mut self.slots[0];
                if event.value() == 1 {
                    slot.id = Some(0);
                    slot.phase = Some(crate::hardware::TouchPhase::Down);
                } else if event.value() == 0 && slot.id.is_some() {
                    slot.phase = Some(crate::hardware::TouchPhase::Up);
                }
            }
            EventType::SYNCHRONIZATION if event.code() == Synchronization::SYN_DROPPED.0 => {
                self.cancel_all(modifiers, output);
            }
            EventType::SYNCHRONIZATION if event.code() == Synchronization::SYN_REPORT.0 => {
                for index in 0..self.slots.len() {
                    let phase = self.slots[index].phase.or_else(|| {
                        self.slots[index]
                            .id
                            .filter(|_| self.slots[index].changed)
                            .map(|_| crate::hardware::TouchPhase::Move)
                    });
                    if let Some(phase) = phase {
                        self.emit_slot(index, phase, modifiers, output);
                    }
                    self.slots[index].changed = false;
                }
            }
            _ => {}
        }
    }

    fn cancel_all(
        &mut self,
        modifiers: ModifierState,
        output: &mut Vec<crate::hardware::TouchEvent>,
    ) {
        for index in 0..self.slots.len() {
            if self.slots[index].id.is_some() {
                self.emit_slot(
                    index,
                    crate::hardware::TouchPhase::Cancel,
                    modifiers,
                    output,
                );
            }
        }
    }

    fn emit_slot(
        &mut self,
        index: usize,
        phase: crate::hardware::TouchPhase,
        modifiers: ModifierState,
        output: &mut Vec<crate::hardware::TouchEvent>,
    ) {
        let slot = &mut self.slots[index];
        let Some(id) = slot.id else { return };
        output.push(crate::hardware::TouchEvent {
            phase,
            id,
            time: self.started.elapsed().as_secs_f64(),
            x: self.profile.x_range.normalize(slot.x) * crate::DISPLAY_WIDTH_F64,
            y: self.profile.y_range.normalize(slot.y) * crate::DISPLAY_HEIGHT_F64,
            modifiers,
            pressure: slot
                .pressure
                .zip(self.profile.pressure_range)
                .map(|(value, range)| range.normalize(value)),
            width: slot
                .width
                .zip(self.profile.width_range)
                .map(|(value, range)| range.normalize(value)),
            height: slot
                .height
                .zip(self.profile.height_range)
                .map(|(value, range)| range.normalize(value)),
        });
        if matches!(
            phase,
            crate::hardware::TouchPhase::Up | crate::hardware::TouchPhase::Cancel
        ) {
            *slot = TouchSlot::default();
        } else {
            slot.phase = None;
        }
    }
}

struct TouchInput {
    path: PathBuf,
    device: evdev::Device,
    state: TouchState,
    grabbed: bool,
}

#[derive(Clone, Copy)]
enum EventDeviceKind {
    Touch,
    Keyboard,
}

fn udev_property_value<'a>(properties: &'a str, key: &str) -> Option<&'a str> {
    properties.lines().find_map(|line| {
        let property = line.strip_prefix("E:")?;
        property.strip_prefix(key)?.strip_prefix('=')
    })
}

fn udev_property(path: &Path, key: &str) -> io::Result<Option<String>> {
    use std::os::unix::fs::MetadataExt;

    let device = std::fs::metadata(path)?;
    let database_path = format!(
        "/run/udev/data/c{}:{}",
        libc::major(device.rdev()),
        libc::minor(device.rdev())
    );
    match std::fs::read_to_string(database_path) {
        Ok(properties) => Ok(udev_property_value(&properties, key).map(str::to_owned)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn event_device_is_on_seat(path: &Path, seat: &str, require_tag: bool) -> bool {
    udev_property(path, "ID_SEAT")
        .ok()
        .flatten()
        .map_or(!require_tag, |device_seat| device_seat == seat)
}

fn has_touch_capabilities(device: &evdev::Device) -> bool {
    use evdev::AbsoluteAxisType;

    let Some(axes) = device.supported_absolute_axes() else {
        return false;
    };
    let multitouch = axes.contains(AbsoluteAxisType::ABS_MT_SLOT)
        && axes.contains(AbsoluteAxisType::ABS_MT_POSITION_X)
        && axes.contains(AbsoluteAxisType::ABS_MT_POSITION_Y);
    let single_touch = axes.contains(AbsoluteAxisType::ABS_X)
        && axes.contains(AbsoluteAxisType::ABS_Y)
        && device
            .supported_keys()
            .is_some_and(|keys| keys.contains(Key::BTN_TOUCH));
    multitouch || single_touch
}

fn has_keyboard_capabilities(device: &evdev::Device) -> bool {
    let Some(keys) = device.supported_keys() else {
        return false;
    };
    keys.contains(Key::KEY_FN)
        && [
            Key::KEY_LEFTCTRL,
            Key::KEY_RIGHTCTRL,
            Key::KEY_LEFTALT,
            Key::KEY_RIGHTALT,
            Key::KEY_LEFTSHIFT,
            Key::KEY_RIGHTSHIFT,
            Key::KEY_LEFTMETA,
            Key::KEY_RIGHTMETA,
        ]
        .iter()
        .any(|key| keys.contains(*key))
}

fn matches_event_device(path: &Path, device: &evdev::Device, kind: EventDeviceKind) -> bool {
    let (seat, require_seat_tag, udev_kind, capabilities) = match kind {
        // Asahi marks the isolated Touch Bar input seat separately from the
        // physical login seat. The keyboard commonly has no ID_SEAT entry and
        // therefore uses udev's default seat.
        EventDeviceKind::Touch => (
            "seat-touchbar",
            true,
            "ID_INPUT_TOUCHSCREEN",
            has_touch_capabilities(device),
        ),
        EventDeviceKind::Keyboard => (
            "seat0",
            false,
            "ID_INPUT_KEYBOARD",
            has_keyboard_capabilities(device),
        ),
    };
    if !event_device_is_on_seat(path, seat, require_seat_tag) {
        return false;
    }
    capabilities
        && udev_property(path, udev_kind)
            .ok()
            .flatten()
            .is_some_and(|value| value == "1")
}

fn open_event_device(kind: EventDeviceKind) -> io::Result<(PathBuf, evdev::Device)> {
    for path in numbered_device_paths("/dev/input", "event")? {
        let Ok(device) = evdev::Device::open(&path) else {
            continue;
        };
        if matches_event_device(&path, &device, kind) {
            return Ok((path, device));
        }
    }
    let description = match kind {
        EventDeviceKind::Touch => "Touch Bar touch input",
        EventDeviceKind::Keyboard => "internal keyboard",
    };
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("input device for {description} not found"),
    ))
}

fn open_touch_device() -> io::Result<(PathBuf, evdev::Device)> {
    open_event_device(EventDeviceKind::Touch)
}

impl TouchInput {
    fn open() -> io::Result<Self> {
        use evdev::AbsoluteAxisType;

        let (path, mut device) = open_touch_device()?;
        set_nonblocking(device.as_raw_fd())?;

        // Exclusive: touches on the strip are ours, not the compositor's cursor.
        device.grab()?;
        let grabbed = true;

        let abs_state = device.get_abs_state().ok();
        let range_for = |axis: AbsoluteAxisType| {
            abs_state.as_ref().and_then(|state| {
                let info = state[axis.0 as usize];
                AxisRange::new(info.minimum, info.maximum)
            })
        };
        let mt_x = range_for(AbsoluteAxisType::ABS_MT_POSITION_X);
        let mt_y = range_for(AbsoluteAxisType::ABS_MT_POSITION_Y);
        let single_x = range_for(AbsoluteAxisType::ABS_X);
        let single_y = range_for(AbsoluteAxisType::ABS_Y);
        let x_range = mt_x.or(single_x).unwrap_or(AxisRange {
            min: 0,
            max: crate::DISPLAY_WIDTH as i32,
        });
        let y_range = mt_y.or(single_y).unwrap_or(AxisRange {
            min: 0,
            max: crate::DISPLAY_HEIGHT as i32,
        });
        let slot_range = range_for(AbsoluteAxisType::ABS_MT_SLOT);
        let slot_count = slot_range
            .map(|range| (range.max - range.min + 1).max(1) as usize)
            .unwrap_or(1);
        let has_mt = mt_x.is_some() && mt_y.is_some() && slot_range.is_some();
        let slot_min = slot_range.map_or(0, |range| range.min);
        let pressure_range = range_for(AbsoluteAxisType::ABS_MT_PRESSURE)
            .or_else(|| range_for(AbsoluteAxisType::ABS_PRESSURE));
        let width_range = range_for(AbsoluteAxisType::ABS_MT_TOUCH_MAJOR);
        let height_range = range_for(AbsoluteAxisType::ABS_MT_TOUCH_MINOR);
        let profile = TouchProfile {
            slot_min,
            slot_count,
            x_range,
            y_range,
            pressure_range,
            width_range,
            height_range,
            single_touch_fallback: !has_mt,
        };
        eprintln!("touch: logical ranges x={x_range:?} y={y_range:?}, slots={slot_count}");

        Ok(Self {
            path,
            device,
            state: TouchState::new(profile),
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

    fn cancel(&mut self, output: &mut Vec<HardwareEvent>, modifiers: ModifierState) {
        let mut touch_events = Vec::new();
        self.state.cancel_all(modifiers, &mut touch_events);
        output.extend(touch_events.into_iter().map(HardwareEvent::Touch));
    }

    fn drain(
        &mut self,
        output: &mut Vec<HardwareEvent>,
        modifiers: ModifierState,
    ) -> io::Result<bool> {
        let events: Vec<InputEvent> = match self.device.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(e) => return Err(e),
        };

        if events.is_empty() {
            return Ok(false);
        }

        let mut touch_events = Vec::new();
        for event in events {
            self.state.process(event, modifiers, &mut touch_events);
        }
        let progress = !touch_events.is_empty();
        output.extend(touch_events.into_iter().map(HardwareEvent::Touch));
        Ok(progress)
    }
}

impl Drop for TouchInput {
    fn drop(&mut self) {
        if let Err(error) = self.ungrab() {
            crate::system_log::broker_error(format!("touch: ungrab failed: {error}"));
        }
    }
}

/// Find the internal keyboard by local-seat udev identity and input
/// capabilities rather than assuming event1 will remain event1 forever.
fn open_main_keyboard() -> io::Result<(PathBuf, evdev::Device)> {
    open_event_device(EventDeviceKind::Keyboard)
}

struct KeyboardInput {
    path: PathBuf,
    device: evdev::Device,
    initial_fn_active: bool,
    initial_modifiers: ModifierState,
    pending: Vec<HardwareEvent>,
}

impl KeyboardInput {
    fn open() -> io::Result<Self> {
        let (path, device) = open_main_keyboard()?;
        set_nonblocking(device.as_raw_fd())?;
        let key_state = device.get_key_state()?;
        let initial_fn_active = initial_fn_state_from_key_state(&key_state);
        let initial_modifiers = modifier_state_from_key_state(&key_state);
        let pending = initial_keyboard_events(&key_state);
        eprintln!("fn: watching internal keyboard at {}", path.display());
        Ok(Self {
            path,
            device,
            initial_fn_active,
            initial_modifiers,
            pending,
        })
    }

    fn initial_fn_active(&self) -> bool {
        self.initial_fn_active
    }

    fn initial_modifiers(&self) -> ModifierState {
        self.initial_modifiers
    }

    fn drain(
        &mut self,
        output: &mut Vec<HardwareEvent>,
        modifiers: &mut ModifierState,
    ) -> io::Result<bool> {
        let mut progress = false;
        if !self.pending.is_empty() {
            output.append(&mut self.pending);
            progress = true;
        }
        let events: Vec<InputEvent> = match self.device.fetch_events() {
            Ok(events) => events.collect(),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(progress),
            Err(e) => return Err(e),
        };

        if events.is_empty() {
            return Ok(progress);
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
                modifiers.set(modifier, active);
                output.push(HardwareEvent::Modifier { modifier, active });
            }
        }
        Ok(true)
    }
}

fn modifier_for(code: u16) -> Option<Modifier> {
    output_key_metadata().iter().find_map(|metadata| {
        let modifier = metadata.modifier?;
        (output_key_code(metadata.key).code() == code).then_some(modifier)
    })
}

fn initial_fn_state_from_key_state(key_state: &AttributeSet<Key>) -> bool {
    key_state.contains(Key::KEY_FN)
}

fn modifier_state_from_key_state(key_state: &AttributeSet<Key>) -> ModifierState {
    let mut modifiers = ModifierState::default();
    for metadata in output_key_metadata() {
        let Some(modifier) = metadata.modifier else {
            continue;
        };
        if key_state.contains(output_key_code(metadata.key)) {
            modifiers.set(modifier, true);
        }
    }
    modifiers
}

fn initial_keyboard_events(key_state: &AttributeSet<Key>) -> Vec<HardwareEvent> {
    let mut events = Vec::new();
    if key_state.contains(Key::KEY_FN) {
        events.push(HardwareEvent::Fn { active: true });
    }
    for metadata in output_key_metadata() {
        let Some(modifier) = metadata.modifier else {
            continue;
        };
        if key_state.contains(output_key_code(metadata.key)) {
            events.push(HardwareEvent::Modifier {
                modifier,
                active: true,
            });
        }
    }
    events
}

/// One virtual keyboard shared by the Lua worker and the fixed Fn row.
struct KeyboardEmitter {
    #[cfg(not(test))]
    device: VirtualDevice,
    #[cfg(test)]
    device: TestKeyboardDevice,
}

// Only tests substitute the uinput sink; event encoding and emitter ownership
// still cross the real KeyboardEmitter and M2TouchBar::release_inner paths.
#[cfg(test)]
enum TestKeyboardDevice {
    Real(VirtualDevice),
    Recording(std::rc::Rc<std::cell::RefCell<Vec<InputEvent>>>),
}

#[cfg(test)]
impl TestKeyboardDevice {
    fn emit(&mut self, events: &[InputEvent]) -> io::Result<()> {
        match self {
            Self::Real(device) => device.emit(events),
            Self::Recording(recorded) => {
                recorded.borrow_mut().extend_from_slice(events);
                Ok(())
            }
        }
    }
}

impl KeyboardEmitter {
    fn new() -> Result<Self> {
        let keys: AttributeSet<Key> = output_key_metadata()
            .iter()
            .map(|metadata| output_key_code(metadata.key))
            .collect();
        let device = VirtualDeviceBuilder::new()?
            .name("Sliver Keyboard")
            .with_keys(&keys)?
            .build()?;
        eprintln!("keyboard: virtual Sliver Keyboard ready");
        Ok(Self {
            #[cfg(not(test))]
            device,
            #[cfg(test)]
            device: TestKeyboardDevice::Real(device),
        })
    }

    fn emit(&mut self, events: &[SyntheticKeyEvent]) -> io::Result<()> {
        let events = encode_synthetic_key_events(events);
        if !events.is_empty() {
            self.device.emit(&events)?;
        }
        Ok(())
    }
}

fn encode_synthetic_key_events(events: &[SyntheticKeyEvent]) -> Vec<InputEvent> {
    events
        .iter()
        .map(|event| {
            InputEvent::new(
                EventType::KEY,
                output_key_code(event.key).code(),
                i32::from(event.active),
            )
        })
        .collect()
}

fn output_key_code(key: OutputKey) -> Key {
    match key {
        OutputKey::Keyboard(key) => match key {
            KeyboardKey::Escape => Key::KEY_ESC,
            KeyboardKey::F1 => Key::KEY_F1,
            KeyboardKey::F2 => Key::KEY_F2,
            KeyboardKey::F3 => Key::KEY_F3,
            KeyboardKey::F4 => Key::KEY_F4,
            KeyboardKey::F5 => Key::KEY_F5,
            KeyboardKey::F6 => Key::KEY_F6,
            KeyboardKey::F7 => Key::KEY_F7,
            KeyboardKey::F8 => Key::KEY_F8,
            KeyboardKey::F9 => Key::KEY_F9,
            KeyboardKey::F10 => Key::KEY_F10,
            KeyboardKey::F11 => Key::KEY_F11,
            KeyboardKey::F12 => Key::KEY_F12,
            KeyboardKey::LeftCtrl => Key::KEY_LEFTCTRL,
            KeyboardKey::RightCtrl => Key::KEY_RIGHTCTRL,
            KeyboardKey::LeftAlt => Key::KEY_LEFTALT,
            KeyboardKey::RightAlt => Key::KEY_RIGHTALT,
            KeyboardKey::LeftShift => Key::KEY_LEFTSHIFT,
            KeyboardKey::RightShift => Key::KEY_RIGHTSHIFT,
            KeyboardKey::LeftSuper => Key::KEY_LEFTMETA,
            KeyboardKey::RightSuper => Key::KEY_RIGHTMETA,
        },
        OutputKey::Consumer(key) => match key {
            ConsumerKey::BrightnessDown => Key::KEY_BRIGHTNESSDOWN,
            ConsumerKey::BrightnessUp => Key::KEY_BRIGHTNESSUP,
            ConsumerKey::Previous => Key::KEY_PREVIOUSSONG,
            ConsumerKey::PlayPause => Key::KEY_PLAYPAUSE,
            ConsumerKey::Next => Key::KEY_NEXTSONG,
            ConsumerKey::Mute => Key::KEY_MUTE,
            ConsumerKey::VolumeDown => Key::KEY_VOLUMEDOWN,
            ConsumerKey::VolumeUp => Key::KEY_VOLUMEUP,
        },
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

fn black_logical_frame() -> Result<LogicalFrame> {
    let surface = ImageSurface::create(
        cairo::Format::ARgb32,
        crate::DISPLAY_WIDTH as i32,
        crate::DISPLAY_HEIGHT as i32,
    )?;
    let context = cairo::Context::new(&surface)?;
    context.set_operator(Operator::Source);
    context.set_source_rgb(0.0, 0.0, 0.0);
    context.paint()?;
    surface.flush();
    LogicalFrame::from_surface(&surface)
}

fn paint_logical_frame(frame: &LogicalFrame, physical: &ImageSurface) -> Result<()> {
    ensure!(
        frame.width() == crate::DISPLAY_WIDTH && frame.height() == crate::DISPLAY_HEIGHT,
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
    sleep_monitor: Option<SleepMonitor>,
    backlight: Option<Backlight>,
    fn_active: bool,
    modifiers: ModifierState,
    keyboard_emitter: Option<KeyboardEmitter>,
    available: bool,
    unavailable_capability: Option<HardwareCapability>,
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
            sleep_monitor: None,
            backlight: None,
            fn_active: false,
            modifiers: ModifierState::default(),
            keyboard_emitter: None,
            available: true,
            unavailable_capability: None,
        }
    }

    fn is_claimed(&self) -> bool {
        self.claim.is_some()
    }

    fn mark_unavailable(&mut self, capability: HardwareCapability) {
        self.available = false;
        self.unavailable_capability = Some(capability);
    }

    fn lose_hardware(&mut self, capability: HardwareCapability) {
        self.mark_unavailable(capability);
        if self.is_claimed() {
            let _ = self.release_inner();
        }
    }

    fn finish_claim_setup(&mut self, setup_result: Result<()>) -> Result<()> {
        if let Err(error) = setup_result {
            if let Err(cleanup_error) = self.release_inner() {
                return Err(error.context(format!(
                    "rolling back Touch Bar claim after setup failure also failed: {cleanup_error:#}"
                )));
            }
            return Err(error);
        }
        Ok(())
    }

    fn claim_inner(&mut self) -> Result<()> {
        ensure!(!self.is_claimed(), "Touch Bar is already claimed");
        self.fn_active = false;
        self.modifiers = ModifierState::default();

        let mut claim = claim_card()?;
        let (panel_width, panel_height) = claim.mode.size();
        let (panel_width, panel_height) = (u32::from(panel_width), u32::from(panel_height));
        validate_panel_mode(panel_width, panel_height)?;

        let mut dumb_buffer = None;
        let mut framebuffer = None;
        let physical_surface = (|| -> Result<ImageSurface> {
            let created_dumb_buffer = claim.card.create_dumb_buffer(
                (panel_width + FB_PAD, panel_height),
                DrmFourcc::Xrgb8888,
                32,
            )?;
            dumb_buffer = Some(created_dumb_buffer);

            let created_framebuffer = claim.card.add_framebuffer(
                dumb_buffer.as_ref().expect("dumb buffer was set"),
                24,
                32,
            )?;
            framebuffer = Some(created_framebuffer);

            // Canvas spans only the visible panel width; the 4px pad is padding.
            let pixel_stride = panel_width * 4;
            let pixels = vec![0u8; (pixel_stride * panel_height) as usize];
            Ok(cairo::ImageSurface::create_for_data(
                pixels,
                cairo::Format::ARgb32,
                panel_width as i32,
                panel_height as i32,
                pixel_stride as i32,
            )?)
        })();

        let physical_surface = match physical_surface {
            Ok(surface) => surface,
            Err(error) => {
                if let Err(release_error) = claim.release(framebuffer, dumb_buffer) {
                    crate::system_log::broker_error(format!(
                        "DRM release failed after claim error: {release_error:#}"
                    ));
                }
                return Err(error);
            }
        };

        self.claim = Some(claim);
        self.framebuffer = framebuffer;
        self.dumb_buffer = dumb_buffer;
        self.physical_surface = Some(physical_surface);

        let setup_result = (|| -> Result<()> {
            self.touch = Some(match TouchInput::open() {
                Ok(touch) => touch,
                Err(error) => {
                    self.mark_unavailable(HardwareCapability::Touch);
                    return Err(error).context("opening Touch Bar touch input");
                }
            });
            let keyboard = match KeyboardInput::open() {
                Ok(keyboard) => keyboard,
                Err(error) => {
                    self.mark_unavailable(HardwareCapability::Fn);
                    return Err(error).context("opening internal keyboard");
                }
            };
            self.fn_active = keyboard.initial_fn_active();
            self.modifiers = keyboard.initial_modifiers();
            self.keyboard = Some(keyboard);
            self.sleep_monitor =
                Some(SleepMonitor::new().context("subscribing to the suspend monitor")?);
            self.backlight = Some(match Backlight::discover() {
                Ok(backlight) => backlight,
                Err(error) => {
                    self.mark_unavailable(HardwareCapability::Backlight);
                    return Err(error).context("discovering DSI backlight");
                }
            });
            if self.keyboard_emitter.is_none() {
                self.keyboard_emitter = Some(match KeyboardEmitter::new() {
                    Ok(emitter) => emitter,
                    Err(error) => {
                        self.mark_unavailable(HardwareCapability::SyntheticKeys);
                        return Err(error).context("creating Sliver Keyboard");
                    }
                });
            }
            Ok(())
        })();
        self.finish_claim_setup(setup_result)
    }

    fn remember_keyboard_events(&mut self, events: &[HardwareEvent]) {
        for event in events {
            match *event {
                HardwareEvent::Fn { active } => self.fn_active = active,
                HardwareEvent::Modifier { modifier, active } => {
                    self.modifiers.set(modifier, active)
                }
                HardwareEvent::Touch(_)
                | HardwareEvent::Device { .. }
                | HardwareEvent::Capability { .. }
                | HardwareEvent::Visibility { .. } => {}
            }
        }
    }

    fn reset_keyboard_state(&mut self, output: &mut Vec<HardwareEvent>) {
        if self.fn_active {
            output.push(HardwareEvent::Fn { active: false });
        }
        for modifier in Modifier::ALL {
            if self.modifiers.is_active(modifier) {
                output.push(HardwareEvent::Modifier {
                    modifier,
                    active: false,
                });
            }
        }
        self.fn_active = false;
        self.modifiers = ModifierState::default();
    }

    fn poll_claimed_hardware(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        if !self.is_claimed() {
            if self.reacquire().is_err() {
                return Ok(Vec::new());
            }
            return Ok(vec![HardwareEvent::Device { present: true }]);
        }
        if let Err(_error) = wait_for_input(
            self.touch.as_ref(),
            self.keyboard.as_ref(),
            self.sleep_monitor.as_ref(),
            timeout,
        ) {
            self.mark_unavailable(HardwareCapability::Display);
            let _ = self.release_inner();
            return Ok(vec![HardwareEvent::Capability {
                capability: HardwareCapability::Display,
                present: false,
            }]);
        }

        let mut output = Vec::new();
        if let Some(sleep_monitor) = self.sleep_monitor.as_mut() {
            sleep_monitor.drain(&mut output)?;
        }
        loop {
            let mut made_progress = false;

            // Preserve cross-device ordering: update Fn and
            // modifiers before interpreting a touch from the same poll.
            let keyboard_start = output.len();
            let keyboard_error = match self.keyboard.as_mut() {
                Some(keyboard) => match keyboard.drain(&mut output, &mut self.modifiers) {
                    Ok(progress) => {
                        made_progress |= progress;
                        None
                    }
                    Err(error) => Some(error),
                },
                None => None,
            };
            self.remember_keyboard_events(&output[keyboard_start..]);
            let mut lost_capabilities = Vec::new();
            if let Some(error) = keyboard_error {
                if let Some(keyboard) = self.keyboard.as_ref() {
                    crate::system_log::broker_error(format!(
                        "fn: keyboard reader stopped at {}: {error}",
                        keyboard.path.display()
                    ));
                } else {
                    crate::system_log::broker_error(format!(
                        "fn: keyboard reader stopped: {error}"
                    ));
                }
                self.reset_keyboard_state(&mut output);
                lost_capabilities.push(HardwareCapability::Fn);
            }

            let touch_error = match self.touch.as_mut() {
                Some(touch) => match touch.drain(&mut output, self.modifiers) {
                    Ok(progress) => {
                        made_progress |= progress;
                        None
                    }
                    Err(error) => Some(error),
                },
                None => None,
            };
            if let Some(error) = touch_error {
                if let Some(touch) = self.touch.as_ref() {
                    crate::system_log::broker_error(format!(
                        "touch: reader stopped at {}: {error}",
                        touch.path.display()
                    ));
                } else {
                    crate::system_log::broker_error(format!("touch: reader stopped: {error}"));
                }
                if let Some(touch) = self.touch.as_mut() {
                    touch.cancel(&mut output, self.modifiers);
                }
                lost_capabilities.push(HardwareCapability::Touch);
            }

            if !lost_capabilities.is_empty() {
                let _ = self.release_inner();
                for capability in lost_capabilities {
                    self.mark_unavailable(capability);
                    output.push(HardwareEvent::Capability {
                        capability,
                        present: false,
                    });
                }
                break;
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
        claim
            .card
            .dirty_framebuffer(
                framebuffer,
                &[control::ClipRect::new(0, 0, PANEL_W as u16, height as u16)],
            )
            .context("flushing Touch Bar framebuffer")?;

        if !self.shown {
            claim.show(framebuffer)?;
            self.shown = true;
        }
        Ok(())
    }

    fn release_inner(&mut self) -> Result<()> {
        self.available = false;
        // Ungrab before dropping the device. Keep releasing the remaining
        // resources after an error, then report the first failure.
        let mut first_error = None;
        if let Some(touch) = self.touch.as_mut() {
            remember_error(&mut first_error, touch.ungrab());
        }
        self.touch = None;
        self.keyboard = None;
        self.sleep_monitor = None;
        self.backlight = None;
        self.keyboard_emitter = None;
        self.fn_active = false;
        self.modifiers = ModifierState::default();

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
            crate::system_log::broker_error(format!("Touch Bar release failed: {error:#}"));
        }
    }
}

impl TouchBarHardware for M2TouchBar {
    fn claim(&mut self) -> Result<()> {
        self.unavailable_capability = None;
        match self.claim_inner() {
            Ok(()) => {
                self.available = true;
                self.unavailable_capability = None;
                Ok(())
            }
            Err(error) => {
                if self.unavailable_capability.is_none() {
                    self.mark_unavailable(HardwareCapability::Display);
                } else {
                    self.available = false;
                }
                Err(error)
            }
        }
    }

    fn reacquire(&mut self) -> Result<()> {
        if self.is_claimed() {
            return Ok(());
        }
        self.claim()
    }

    fn is_available(&self) -> bool {
        self.available && self.is_claimed()
    }

    fn unavailable_capability(&self) -> Option<HardwareCapability> {
        self.unavailable_capability
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        self.poll_claimed_hardware(timeout)
    }

    fn input_state(&self) -> InputState {
        InputState {
            fn_active: self.fn_active,
            modifiers: self.modifiers,
        }
    }

    fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
        match self.present_inner(frame) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.mark_unavailable(HardwareCapability::Display);
                let _ = self.release_inner();
                Err(error)
            }
        }
    }

    fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
        ensure!(self.is_claimed(), "Touch Bar is not claimed");
        let Some(emitter) = self.keyboard_emitter.as_mut() else {
            self.lose_hardware(HardwareCapability::SyntheticKeys);
            return Err(anyhow::anyhow!("Sliver Keyboard is unavailable"));
        };
        if let Err(error) = emitter.emit(events) {
            self.lose_hardware(HardwareCapability::SyntheticKeys);
            return Err(error).context("emitting synthetic keyboard events");
        }
        Ok(())
    }

    fn get_backlight(&mut self) -> Result<f64> {
        ensure!(self.is_claimed(), "Touch Bar is not claimed");
        let (current, maximum) = match self
            .backlight
            .as_ref()
            .context("Touch Bar backlight is not initialized")
            .and_then(Backlight::values)
        {
            Ok(values) => values,
            Err(error) => {
                self.lose_hardware(HardwareCapability::Backlight);
                return Err(error);
            }
        };
        match normalize_backlight_level(current, maximum) {
            Ok(level) => Ok(level),
            Err(error) => {
                self.lose_hardware(HardwareCapability::Backlight);
                Err(error)
            }
        }
    }

    fn set_backlight(&mut self, level: f64) -> Result<()> {
        ensure!(self.is_claimed(), "Touch Bar is not claimed");
        validate_backlight(level)?;
        let result = self
            .backlight
            .as_ref()
            .context("Touch Bar backlight is not initialized")
            .and_then(|backlight| backlight.set(level));
        if let Err(error) = result {
            self.lose_hardware(HardwareCapability::Backlight);
            return Err(error);
        }
        Ok(())
    }

    fn release(&mut self) -> Result<()> {
        let mut first_error = None;
        if self.is_claimed() {
            match black_logical_frame().and_then(|frame| self.present_inner(&frame)) {
                Ok(()) => {}
                Err(error) => remember_release_error(
                    &mut first_error,
                    error.context("painting black during release"),
                ),
            }
            if let Err(error) = self.set_backlight(0.0) {
                remember_release_error(
                    &mut first_error,
                    error.context("turning off the Touch Bar during release"),
                );
            }
        }
        if let Err(error) = self.release_inner() {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[cfg(feature = "calibration")]
/// Calibration pattern, straight into the raw buffer. One photo of the
/// glass tells us exactly how memory maps to pixels:
///   red    — the first 200 rows of scanout
///   blue   — the last 200 rows
///   green  — columns 0..20 (start of each scanline)
///   white  — columns 40..60 (end of the visible window)
///   magenta— the 4 padding columns (should never be visible)
pub(crate) fn calibration() -> Result<()> {
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
                    crate::system_log::broker_error(format!(
                        "DRM release failed after calibration error: {release_error:#}"
                    ));
                }
                return Err(error.into());
            }
        };
    let framebuffer = match claim.card.add_framebuffer(&dumb_buffer, 24, 32) {
        Ok(framebuffer) => framebuffer,
        Err(error) => {
            if let Err(release_error) = claim.release(None, Some(dumb_buffer)) {
                crate::system_log::broker_error(format!(
                    "DRM release failed after calibration error: {release_error:#}"
                ));
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
        eprintln!("calibration pattern on glass: red/blue ends, green/white flanks, magenta pad");
        hold()
    })();

    let release_result = claim.release(Some(framebuffer), Some(dumb_buffer));
    match (result, release_result) {
        (Err(error), Err(release_error)) => {
            crate::system_log::broker_error(format!(
                "DRM release failed after calibration error: {release_error:#}"
            ));
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("releasing calibration hardware"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(feature = "calibration")]
/// Hold until Ctrl-C (calibration's simpler pulse).
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
    fn failed_claim_setup_rolls_back_before_retry() {
        let mut hardware = M2TouchBar::new();
        let first = hardware
            .finish_claim_setup(Err(anyhow::anyhow!("injected setup failure")))
            .expect_err("injected setup failure was swallowed");
        assert!(format!("{first:#}").contains("injected setup failure"));
        assert!(!hardware.is_claimed());

        let second = hardware
            .finish_claim_setup(Err(anyhow::anyhow!("retry setup failure")))
            .expect_err("retry setup failure was swallowed");
        assert!(format!("{second:#}").contains("retry setup failure"));
        assert!(!format!("{second:#}").contains("already claimed"));
    }

    #[test]
    fn release_discards_keyboard_emitter_for_reclaim() -> Result<()> {
        use std::cell::RefCell;
        use std::rc::Rc;

        let mut hardware = M2TouchBar::new();
        let first = Rc::new(RefCell::new(Vec::new()));
        hardware.keyboard_emitter = Some(KeyboardEmitter {
            device: TestKeyboardDevice::Recording(first.clone()),
        });
        let event = SyntheticKeyEvent {
            key: OutputKey::Keyboard(KeyboardKey::F1),
            active: true,
        };
        hardware.keyboard_emitter.as_mut().unwrap().emit(&[event])?;
        let assert_f1_down = |events: &[InputEvent]| {
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event_type(), EventType::KEY);
            assert_eq!(events[0].code(), Key::KEY_F1.code());
            assert_eq!(events[0].value(), 1);
        };
        assert_f1_down(&first.borrow());
        hardware.release_inner()?;
        assert!(hardware.keyboard_emitter.is_none());
        assert_eq!(
            Rc::strong_count(&first),
            1,
            "release retained the old device"
        );

        // Supply a fresh sink as claim setup would, then verify output cannot
        // reach the released device and a second release drops the new one too.
        let second = Rc::new(RefCell::new(Vec::new()));
        hardware.keyboard_emitter = Some(KeyboardEmitter {
            device: TestKeyboardDevice::Recording(second.clone()),
        });
        hardware.keyboard_emitter.as_mut().unwrap().emit(&[event])?;
        assert_eq!(first.borrow().len(), 1);
        assert_f1_down(&second.borrow());
        hardware.release_inner()?;
        assert!(hardware.keyboard_emitter.is_none());
        assert_eq!(
            Rc::strong_count(&second),
            1,
            "release retained the replacement device"
        );
        Ok(())
    }

    #[test]
    fn release_treats_missing_resources_as_already_released() {
        let mut first_error = None;
        remember_error(
            &mut first_error,
            Err::<(), _>(io::Error::from_raw_os_error(libc::ENOENT)),
        );

        assert!(first_error.is_none());

        let mut first_error = None;
        remember_release_error(
            &mut first_error,
            anyhow::anyhow!(io::Error::from_raw_os_error(libc::ENOENT))
                .context("restoring the previous CRTC"),
        );
        assert!(first_error.is_none());

        let mut first_error = None;
        remember_release_error(
            &mut first_error,
            anyhow::anyhow!(io::Error::from_raw_os_error(libc::EIO)),
        );
        assert!(first_error.is_some());
    }

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
    fn mt_slots_emit_normalized_lifecycle_and_modifier_snapshots() -> Result<()> {
        use evdev::{AbsoluteAxisType as Axis, Synchronization};

        let mut touch = TouchState::new(TouchProfile {
            slot_min: 0,
            slot_count: 2,
            x_range: AxisRange::new(0, 100).expect("valid x range"),
            y_range: AxisRange::new(0, 10).expect("valid y range"),
            pressure_range: AxisRange::new(0, 10),
            width_range: AxisRange::new(0, 100),
            height_range: AxisRange::new(0, 100),
            single_touch_fallback: false,
        });
        let mut output = Vec::new();
        let mut down_modifiers = ModifierState::default();
        down_modifiers.set(Modifier::LeftCtrl, true);
        let mut move_modifiers = ModifierState::default();
        move_modifiers.set(Modifier::RightAlt, true);

        for event in [
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_SLOT.0, 0),
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_TRACKING_ID.0, 41),
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_POSITION_X.0, 50),
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_POSITION_Y.0, 5),
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_PRESSURE.0, 5),
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_TOUCH_MAJOR.0, 25),
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_TOUCH_MINOR.0, 10),
            InputEvent::new(EventType::SYNCHRONIZATION, Synchronization::SYN_REPORT.0, 0),
        ] {
            touch.process(event, down_modifiers, &mut output);
        }

        touch.process(
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_POSITION_X.0, 75),
            move_modifiers,
            &mut output,
        );
        touch.process(
            InputEvent::new(EventType::SYNCHRONIZATION, Synchronization::SYN_REPORT.0, 0),
            move_modifiers,
            &mut output,
        );
        touch.process(
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_TRACKING_ID.0, -1),
            ModifierState::default(),
            &mut output,
        );
        touch.process(
            InputEvent::new(EventType::SYNCHRONIZATION, Synchronization::SYN_REPORT.0, 0),
            ModifierState::default(),
            &mut output,
        );

        assert_eq!(output.len(), 3);
        assert_eq!(output[0].phase, crate::hardware::TouchPhase::Down);
        assert_eq!(output[0].id, 41);
        assert_eq!(output[0].x, 1004.0);
        assert_eq!(output[0].y, 30.0);
        assert!(output[0].modifiers.is_active(Modifier::LeftCtrl));
        assert_eq!(output[0].pressure, Some(0.5));
        assert_eq!(output[0].width, Some(0.25));
        assert_eq!(output[0].height, Some(0.1));
        assert_eq!(output[1].phase, crate::hardware::TouchPhase::Move);
        assert!(output[1].modifiers.is_active(Modifier::RightAlt));
        assert_eq!(output[1].x, 1506.0);
        assert_eq!(output[2].phase, crate::hardware::TouchPhase::Up);
        assert!(output[2].time >= output[0].time);
        Ok(())
    }

    #[test]
    fn syn_dropped_cancels_active_contacts() {
        use evdev::{AbsoluteAxisType as Axis, Synchronization};

        let mut touch = TouchState::new(TouchProfile {
            slot_min: 0,
            slot_count: 1,
            x_range: AxisRange::new(0, 100).expect("valid x range"),
            y_range: AxisRange::new(0, 10).expect("valid y range"),
            pressure_range: None,
            width_range: None,
            height_range: None,
            single_touch_fallback: false,
        });
        let mut output = Vec::new();
        for event in [
            InputEvent::new(EventType::ABSOLUTE, Axis::ABS_MT_TRACKING_ID.0, 9),
            InputEvent::new(EventType::SYNCHRONIZATION, Synchronization::SYN_REPORT.0, 0),
            InputEvent::new(
                EventType::SYNCHRONIZATION,
                Synchronization::SYN_DROPPED.0,
                0,
            ),
        ] {
            touch.process(event, ModifierState::default(), &mut output);
        }

        assert_eq!(output[0].phase, crate::hardware::TouchPhase::Down);
        assert_eq!(output[1].phase, crate::hardware::TouchPhase::Cancel);
    }

    #[test]
    fn device_number_matching_ignores_unrelated_input_names() {
        assert!(is_numbered_device("card0", "card"));
        assert!(is_numbered_device("event12", "event"));
        assert!(!is_numbered_device("card", "card"));
        assert!(!is_numbered_device("eventx", "event"));
        assert!(!is_numbered_device("card0-extra", "card"));
        assert!(!is_numbered_device("event1", "card"));
    }

    #[test]
    fn backlight_values_normalize_and_round() -> Result<()> {
        assert_eq!(normalize_backlight_level(3, 4)?, 0.75);
        assert_eq!(backlight_value(0.75, 4)?, 3);
        assert!(normalize_backlight_level(5, 4).is_err());
        assert!(backlight_value(0.5, 0).is_err());
        Ok(())
    }

    #[test]
    fn keyboard_failure_releases_fn_and_modifier_snapshots() {
        let mut hardware = M2TouchBar::new();
        hardware.fn_active = true;
        hardware.modifiers.set(Modifier::LeftCtrl, true);
        hardware.modifiers.set(Modifier::RightAlt, true);
        let mut output = Vec::new();

        hardware.reset_keyboard_state(&mut output);

        assert_eq!(
            output,
            vec![
                HardwareEvent::Fn { active: false },
                HardwareEvent::Modifier {
                    modifier: Modifier::LeftCtrl,
                    active: false,
                },
                HardwareEvent::Modifier {
                    modifier: Modifier::RightAlt,
                    active: false,
                },
            ]
        );
        assert!(!hardware.fn_active);
        assert_eq!(hardware.modifiers, ModifierState::default());
    }

    #[test]
    fn evdev_key_state_seeds_modifier_snapshots_before_initial_events() -> Result<()> {
        let key_state: AttributeSet<Key> = [Key::KEY_LEFTCTRL, Key::KEY_RIGHTALT, Key::KEY_FN]
            .into_iter()
            .collect();
        let modifiers = modifier_state_from_key_state(&key_state);
        assert!(modifiers.is_active(Modifier::LeftCtrl));
        assert!(modifiers.is_active(Modifier::RightAlt));
        assert!(!modifiers.is_active(Modifier::LeftAlt));
        assert_eq!(
            initial_keyboard_events(&key_state),
            vec![
                HardwareEvent::Fn { active: true },
                HardwareEvent::Modifier {
                    modifier: Modifier::LeftCtrl,
                    active: true,
                },
                HardwareEvent::Modifier {
                    modifier: Modifier::RightAlt,
                    active: true,
                },
            ]
        );

        Ok(())
    }

    #[test]
    fn udev_properties_identify_the_local_seat() {
        let properties = "E:ID_SEAT=seat0\nE:ID_INPUT=1\n";

        assert_eq!(udev_property_value(properties, "ID_SEAT"), Some("seat0"));
        assert_eq!(udev_property_value(properties, "ID_INPUT"), Some("1"));
        assert_eq!(udev_property_value(properties, "ID_INPUT_KEYBOARD"), None);
    }

    #[test]
    fn rejects_non_native_touch_bar_modes() {
        assert!(validate_panel_mode(PANEL_W, crate::DISPLAY_WIDTH as u32).is_ok());
        assert!(validate_panel_mode(2008, 60).is_err());
        assert!(validate_panel_mode(60, 2007).is_err());
    }

    #[test]
    fn initial_keyboard_state_includes_fn_for_staging() {
        let key_state: AttributeSet<Key> = [Key::KEY_FN].into_iter().collect();

        assert!(initial_fn_state_from_key_state(&key_state));
    }

    #[test]
    fn logical_top_and_bottom_map_to_the_m2_panel_flanks() -> Result<()> {
        let logical = ImageSurface::create(
            cairo::Format::ARgb32,
            crate::DISPLAY_WIDTH as i32,
            crate::DISPLAY_HEIGHT as i32,
        )?;
        let context = cairo::Context::new(&logical)?;
        context.set_source_rgb(0.0, 0.0, 0.0);
        context.paint()?;
        context.set_source_rgb(1.0, 0.0, 0.0);
        context.rectangle(0.0, 0.0, crate::DISPLAY_WIDTH_F64, 10.0);
        context.fill()?;
        context.set_source_rgb(0.0, 0.0, 1.0);
        context.rectangle(0.0, 50.0, crate::DISPLAY_WIDTH_F64, 10.0);
        context.fill()?;
        logical.flush();

        let physical = ImageSurface::create(
            cairo::Format::ARgb32,
            PANEL_W as i32,
            crate::DISPLAY_WIDTH as i32,
        )?;
        paint_logical_frame(&LogicalFrame::from_surface(&logical)?, &physical)?;
        for scanline in 0..crate::DISPLAY_WIDTH {
            assert_eq!(rgba_at(&physical, 55, scanline)?, [255, 0, 0, 255]);
            assert_eq!(rgba_at(&physical, 5, scanline)?, [0, 0, 255, 255]);
            assert_eq!(rgba_at(&physical, 30, scanline)?, [0, 0, 0, 255]);
        }
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
