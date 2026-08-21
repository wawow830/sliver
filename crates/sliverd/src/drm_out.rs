//! Hardware takeover: claim the touchbar panel and hold it.
//!
//! The M2 13" touchbar is a MIPI DSI panel, natively portrait with a
//! 60x2008 mode. Hard-won truths, kept close:
//!   - the driver rounds the dumb buffer up (2008 -> 2048 rows): paint by
//!     the *mode's* height, never the allocation's,
//!   - the buffer wants to be 64px wide (pitch 256); only 60 show,
//!   - the DSI glass freezes its last frame: dead processes haunt it,
//!   - `--probe` paints calibration bands when orientation is in doubt.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{self, connector, crtc, framebuffer, Device as _, Mode};
use drm::Device as _;
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AttributeSet, EventType, InputEvent, Key};

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
/// How long a tapped widget stays lit.
const FLASH: Duration = Duration::from_millis(220);

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
}

/// Open card1, take DRM master, find the DSI touchbar and its CRTC.
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
        &self,
        fb: Option<framebuffer::Handle>,
        db: Option<control::dumbbuffer::DumbBuffer>,
    ) {
        if let Some(old) = &self.old_crtc {
            let _ = self.card.set_crtc(
                self.crtc,
                old.framebuffer(),
                old.position(),
                &[self.conn],
                old.mode(),
            );
        }
        if let Some(fb) = fb {
            let _ = self.card.destroy_framebuffer(fb);
        }
        if let Some(db) = db {
            let _ = self.card.destroy_dumb_buffer(db);
        }
        let _ = self.card.release_master_lock();
    }
}

/// Read touch taps off the touchbar input device, mapped to strip
/// x-coordinates. Runs its own thread; taps arrive on the returned channel.
/// If the device can't be opened, the daemon lives on without touch.
fn spawn_touch() -> mpsc::Receiver<f64> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use evdev::AbsoluteAxisType;

        let mut dev = match evdev::Device::open(TOUCH_DEV) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("touch: can't open {TOUCH_DEV}: {e} (continuing untouchable)");
                return;
            }
        };
        // Exclusive: taps on the strip are ours, not the compositor's cursor.
        if let Err(e) = dev.grab() {
            eprintln!("touch: grab failed: {e} (sharing, then)");
        }

        let range = dev
            .get_abs_state()
            .ok()
            .and_then(|s| {
                let mt = &s[AbsoluteAxisType::ABS_MT_POSITION_X.0 as usize];
                let plain = &s[AbsoluteAxisType::ABS_X.0 as usize];
                let pick = if mt.maximum > mt.minimum { mt } else { plain };
                (pick.maximum > pick.minimum).then_some((pick.minimum, pick.maximum))
            })
            .unwrap_or((0, sliver_core::STRIP_W as i32));
        eprintln!("touch: x range {range:?}");

        let mut last_x: Option<i32> = None;
        let mut touching = false;
        loop {
            let events = match dev.fetch_events() {
                Ok(e) => e,
                Err(_) => break,
            };
            for ev in events {
                match ev.event_type() {
                    EventType::ABSOLUTE => {
                        let c = ev.code();
                        if c == AbsoluteAxisType::ABS_MT_POSITION_X.0
                            || c == AbsoluteAxisType::ABS_X.0
                        {
                            last_x = Some(ev.value());
                        } else if c == AbsoluteAxisType::ABS_MT_TRACKING_ID.0 {
                            touching = ev.value() >= 0;
                        }
                    }
                    EventType::KEY => {
                        if ev.code() == Key::BTN_TOUCH.code() {
                            touching = ev.value() == 1;
                        }
                    }
                    // A touch that just ended with a position on record
                    // is a tap. (If taps land mirrored, flip the mapping.)
                    EventType::SYNCHRONIZATION if !touching => {
                        if let Some(raw) = last_x.take() {
                            let (min, max) = range;
                            let t = (raw - min) as f64 / (max - min).max(1) as f64;
                            let _ = tx.send(t.clamp(0.0, 1.0) * sliver_core::STRIP_W);
                        }
                    }
                    _ => {}
                }
            }
        }
    });
    rx
}

/// Find the internal keyboard by identity rather than assuming event1 will
/// remain event1 forever.
fn open_main_keyboard() -> io::Result<(std::path::PathBuf, evdev::Device)> {
    for entry in std::fs::read_dir("/dev/input")? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("event") {
            continue;
        }
        let Ok(dev) = evdev::Device::open(&path) else {
            continue;
        };
        if dev.name() == Some(KEYBOARD_NAME) {
            return Ok((path, dev));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("input device {KEYBOARD_NAME:?} not found"),
    ))
}

/// Observe Fn/Globe press/release without grabbing the keyboard away from
/// Hyprland or applications.
fn spawn_fn_key() -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (path, mut dev) = match open_main_keyboard() {
            Ok(found) => found,
            Err(e) => {
                eprintln!("fn: can't find keyboard: {e}");
                return;
            }
        };
        eprintln!("fn: watching {} ({KEYBOARD_NAME})", path.display());
        loop {
            let events = match dev.fetch_events() {
                Ok(events) => events,
                Err(e) => {
                    eprintln!("fn: keyboard reader stopped: {e}");
                    return;
                }
            };
            for event in events {
                if event.event_type() == EventType::KEY && event.code() == Key::KEY_FN.code() {
                    let active = match event.value() {
                        1 => Some(true),
                        0 => Some(false),
                        _ => None,
                    };
                    if let Some(active) = active {
                        let _ = tx.send(active);
                    }
                }
            }
        }
    });
    rx
}

/// Virtual keyboard used solely to emit real F1-F12 key events.
struct FnEmitter {
    device: VirtualDevice,
}

impl FnEmitter {
    fn new() -> Result<Self> {
        let keys: AttributeSet<Key> = F_KEYS.into_iter().collect();
        let device = VirtualDeviceBuilder::new()?
            .name("Sliver Function Row")
            .with_keys(&keys)?
            .build()?;
        eprintln!("fn: virtual F-key keyboard ready");
        Ok(Self { device })
    }

    fn tap(&mut self, index: usize) -> io::Result<()> {
        let key = *F_KEYS.get(index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "function-key index out of range",
            )
        })?;
        self.device
            .emit(&[InputEvent::new(EventType::KEY, key.code(), 1)])?;
        self.device
            .emit(&[InputEvent::new(EventType::KEY, key.code(), 0)])
    }
}

/// A config arriving over the socket, plus the reply line the
/// listener thread owes its client.
type SockMsg = (String, mpsc::Sender<Result<(), String>>);

/// Listen for live config application: read a whole TOML document per
/// connection, hand it to the render loop, report ok/error back.
fn spawn_socket() -> mpsc::Receiver<SockMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let path = sliver_core::socket_path();
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("socket: can't bind {}: {e}", path.display());
                return;
            }
        };
        eprintln!("socket: listening on {}", path.display());
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            let mut text = String::new();
            if stream.read_to_string(&mut text).is_err() {
                continue;
            }
            let (reply_tx, reply_rx) = mpsc::channel();
            if tx.send((text, reply_tx)).is_err() {
                break;
            }
            let reply = reply_rx
                .recv()
                .unwrap_or_else(|_| Err("daemon wandered off".into()));
            let msg = match reply {
                Ok(()) => "ok\n".to_string(),
                Err(e) => format!("error: {e}\n"),
            };
            let _ = stream.write_all(msg.as_bytes());
        }
    });
    rx
}

pub struct Takeover {
    claim: CardClaim,
    fb: framebuffer::Handle,
    db: control::dumbbuffer::DumbBuffer,
    surface: cairo::ImageSurface,
}

impl Takeover {
    pub fn claim(cfg: &sliver_core::Config) -> Result<Takeover> {
        let claim = claim_card()?;
        let (pw, ph) = claim.mode.size();
        let (pw, ph) = (u32::from(pw), u32::from(ph));
        debug_assert_eq!(pw, PANEL_W);

        let db = claim
            .card
            .create_dumb_buffer((pw + FB_PAD, ph), DrmFourcc::Xrgb8888, 32)?;
        let fb = claim.card.add_framebuffer(&db, 24, 32)?;

        // Canvas spans only the visible panel width; the 4px pad is padding.
        let px_stride = pw * 4;
        let pixels = vec![0u8; (px_stride * ph) as usize];
        let surface = cairo::ImageSurface::create_for_data(
            pixels,
            cairo::Format::ARgb32,
            pw as i32,
            ph as i32,
            px_stride as i32,
        )?;

        let mut t = Takeover {
            claim,
            fb,
            db,
            surface,
        };
        t.repaint(cfg, None)?;
        t.claim.show(t.fb)?;
        Ok(t)
    }

    /// Render the strip into the panel buffer, then push it out.
    pub fn repaint(&mut self, cfg: &sliver_core::Config, pressed: Option<usize>) -> Result<()> {
        {
            let cr = cairo::Context::new(&self.surface)?;
            // Probe-settled truth (see probe()): buffer scanout rows run
            // along the strip left->right, scanlines bottom->top, and the
            // 64-wide pad never shows. So logical (x,y) -> buffer (60-y, x).
            cr.translate(f64::from(PANEL_W), 0.0);
            cr.rotate(std::f64::consts::FRAC_PI_2);
            sliver_core::render(cfg, &cr, pressed)?;
        }
        self.surface.flush();

        let src = self.surface.data()?;
        let row = (PANEL_W * 4) as usize;
        let sstride = row;
        let dstride = self.db.pitch() as usize;
        // Paint by the *mode's* height — the driver rounds the allocation up
        // (2008 -> 2048), and those pad rows are not ours to touch.
        let height = u32::from(self.claim.mode.size().1) as usize;
        {
            let mut map = self.claim.card.map_dumb_buffer(&mut self.db)?;
            let dst: &mut [u8] = map.as_mut();
            for y in 0..height {
                dst[y * dstride..y * dstride + row]
                    .copy_from_slice(&src[y * sstride..y * sstride + row]);
            }
        }

        // Command-mode DSI: nothing reaches the glass until we say the
        // framebuffer is dirty. set_crtc flushes the first frame; every
        // heartbeat after that goes through here.
        if let Err(e) = self.claim.card.dirty_framebuffer(
            self.fb,
            &[control::ClipRect::new(0, 0, PANEL_W as u16, height as u16)],
        ) {
            eprintln!("dirty flush failed: {e}");
        }
        Ok(())
    }
}

impl Drop for Takeover {
    fn drop(&mut self) {
        self.claim.release(Some(self.fb), Some(self.db));
    }
}

/// Claim the strip and give it a pulse: heartbeat re-renders, touch
/// highlights, live battery numbers, live configs over the socket.
/// Ctrl-C lets go.
pub fn run(mut cfg: sliver_core::Config) -> Result<()> {
    let mut t = Takeover::claim(&cfg)?;
    eprintln!("the strip is ours — Ctrl-C to let go");

    let touch = spawn_touch();
    let socket = spawn_socket();
    let fn_events = spawn_fn_key();
    let fn_layer = sliver_core::function_row_config();
    let mut fn_emitter = match FnEmitter::new() {
        Ok(emitter) => Some(emitter),
        Err(e) => {
            eprintln!("fn: can't create virtual keyboard: {e:#}");
            None
        }
    };

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;

    let mut pressed: Option<(usize, Instant)> = None;
    let mut fn_active = false;
    let mut last_tick = String::new();
    let mut dirty = true;

    while running.load(Ordering::SeqCst) {
        std::thread::park_timeout(Duration::from_millis(50));

        while let Ok(active) = fn_events.try_recv() {
            if active != fn_active {
                fn_active = active;
                pressed = None;
                dirty = true;
                eprintln!("fn layer: {}", if active { "on" } else { "off" });
            }
        }

        while let Ok(x) = touch.try_recv() {
            let active_cfg = if fn_active { &fn_layer } else { &cfg };
            if let Some(i) = sliver_core::hit(active_cfg, x) {
                pressed = Some((i, Instant::now()));
                dirty = true;
                if fn_active {
                    eprintln!("fn tap at x={x:.0} -> F{}", i + 1);
                    if let Some(emitter) = &mut fn_emitter {
                        if let Err(e) = emitter.tap(i) {
                            eprintln!("fn: failed to emit F{}: {e}", i + 1);
                        }
                    }
                } else {
                    eprintln!("tap at x={x:.0} -> widget {i}");
                    if let Some(cmd) = cfg.action_at(i) {
                        let cmd = cmd.to_string();
                        eprintln!("running action: {cmd}");
                        std::thread::spawn(move || {
                            let _ = std::process::Command::new("sh")
                                .arg("-c")
                                .arg(&cmd)
                                .status();
                        });
                    }
                }
            }
        }
        while let Ok((text, reply)) = socket.try_recv() {
            match sliver_core::parse_config(&text) {
                Ok(new_cfg) => {
                    let n = new_cfg.widgets.len();
                    cfg = new_cfg;
                    pressed = None;
                    dirty = true;
                    eprintln!("socket: applied a config ({n} widgets)");
                    let _ = reply.send(Ok(()));
                }
                Err(e) => {
                    eprintln!("socket: rejected a config: {e}");
                    let _ = reply.send(Err(format!("{e:#}")));
                }
            }
        }
        if let Some((_, since)) = pressed {
            if since.elapsed() > FLASH {
                pressed = None;
                dirty = true;
            }
        }
        // Seconds, not minutes: someone will inevitably want %H:%M:%S.
        let tick = chrono::Local::now().format("%H:%M:%S").to_string();
        if tick != last_tick {
            last_tick = tick;
            dirty = true;
        }

        if dirty {
            let active_cfg = if fn_active { &fn_layer } else { &cfg };
            t.repaint(active_cfg, pressed.map(|(i, _)| i))?;
            dirty = false;
        }
    }

    eprintln!("releasing the strip");
    Ok(())
}

/// Calibration pattern, straight into the raw buffer. One photo of the
/// glass then tells us exactly how memory maps to pixels:
///   red    — the first 200 rows of scanout
///   blue   — the last 200 rows
///   green  — columns 0..20 (start of each scanline)
///   white  — columns 40..60 (end of the visible window)
///   magenta— the 4 padding columns (should never be visible)
pub fn probe() -> Result<()> {
    let claim = claim_card()?;
    let (pw, ph) = claim.mode.size();
    let (pw, ph) = (u32::from(pw), u32::from(ph));
    let fb_w = pw + FB_PAD;

    let mut db = claim
        .card
        .create_dumb_buffer((fb_w, ph), DrmFourcc::Xrgb8888, 32)?;
    let fb = claim.card.add_framebuffer(&db, 24, 32)?;

    const RED: u32 = 0xffff_0000;
    const BLUE: u32 = 0xff00_00ff;
    const GREEN: u32 = 0xff00_ff00;
    const WHITE: u32 = 0xffff_ffff;
    const MAGENTA: u32 = 0xffff_00ff;
    const BASE: u32 = 0xff20_2020;

    let pitch = db.pitch() as usize;
    {
        let mut map = claim.card.map_dumb_buffer(&mut db)?;
        let data: &mut [u8] = map.as_mut();
        for row in 0..ph as usize {
            for col in 0..fb_w as usize {
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

    claim.show(fb)?;
    eprintln!("probe on glass: red/blue ends, green/white flanks, magenta pad");
    hold()?;
    claim.release(Some(fb), Some(db));
    Ok(())
}

/// Hold until Ctrl-C (probe's simpler pulse).
fn hold() -> Result<()> {
    eprintln!("holding — Ctrl-C to let go");
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;
    while running.load(Ordering::SeqCst) {
        std::thread::park_timeout(Duration::from_millis(250));
    }
    eprintln!("releasing the strip");
    Ok(())
}
