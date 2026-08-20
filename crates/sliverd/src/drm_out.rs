//! Hardware takeover: claim the touchbar panel and hold it.
//!
//! The M2 13" touchbar is a MIPI DSI panel, natively portrait with a
//! 60x2008 mode. Lessons borrowed from tiny-dfr's display.rs:
//!   - the dumb buffer should be 64px wide (pitch alignment); the CRTC
//!     only scans the first 60 columns,
//!   - orientation is settled not by theory but by `--probe`: raw bands
//!     painted straight into the buffer, read off a photo of the glass.

use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use drm::buffer::{Buffer as _, DrmFourcc};
use drm::control::{self, connector, crtc, framebuffer, Device as _, Mode};
use drm::Device as _;

/// The panel's visible width; the buffer is padded to 64 for pitch sanity.
const PANEL_W: u32 = 60;
const FB_PAD: u32 = 4;

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
    card.acquire_master_lock()
        .context("becoming DRM master")?;

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

    Ok(CardClaim { card, conn: conn.handle(), crtc, old_crtc, mode })
}

impl CardClaim {
    fn show(&self, fb: framebuffer::Handle) -> Result<()> {
        self.card
            .set_crtc(self.crtc, Some(fb), (0, 0), &[self.conn], Some(self.mode))
            .context("setting CRTC")?;
        Ok(())
    }

    fn release(&self, fb: Option<framebuffer::Handle>, db: Option<control::dumbbuffer::DumbBuffer>) {
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

/// Hold the strip until Ctrl-C, then let the caller's cleanup run.
fn hold() -> Result<()> {
    eprintln!("the strip is ours — Ctrl-C to let go");
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))?;
    while running.load(Ordering::SeqCst) {
        std::thread::park_timeout(Duration::from_millis(250));
    }
    eprintln!("releasing the strip");
    Ok(())
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
        eprintln!("dumb buffer: requested {}x{}, got size {:?} pitch {}",
            pw + FB_PAD, ph, db.size(), db.pitch());
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

        let mut t = Takeover { claim, fb, db, surface };
        t.repaint(cfg)?;
        t.claim.show(t.fb)?;
        Ok(t)
    }

    /// Render the strip into the panel buffer, then push it out.
    pub fn repaint(&mut self, cfg: &sliver_core::Config) -> Result<()> {
        {
            let cr = cairo::Context::new(&self.surface)?;
            // Probe-settled truth (see probe()): buffer scanout rows run
            // along the strip left->right, scanlines bottom->top, and the
            // 64-wide pad never shows. So logical (x,y) -> buffer (60-y, x):
            // translate, then rotate. The earlier "sideways" photos were a
            // dead process's frozen frame, not this code's output.
            cr.translate(f64::from(PANEL_W), 0.0);
            cr.rotate(std::f64::consts::FRAC_PI_2);
            sliver_core::render(cfg, &cr)?;
        }
        self.surface.flush();

        let src = self.surface.data()?;
        let row = (PANEL_W * 4) as usize;
        let sstride = row;
        let dstride = self.db.pitch() as usize;
        // NOTE: paint by the *mode's* height — the driver may round the
        // allocation up (2008 -> 2048 here), and those pad rows are not ours.
        let height = u32::from(self.claim.mode.size().1) as usize;
        let mut map = self.claim.card.map_dumb_buffer(&mut self.db)?;
        let dst: &mut [u8] = map.as_mut();
        for y in 0..height {
            dst[y * dstride..y * dstride + row]
                .copy_from_slice(&src[y * sstride..y * sstride + row]);
        }
        Ok(())
    }
}

impl Drop for Takeover {
    fn drop(&mut self) {
        self.claim.release(Some(self.fb), Some(self.db));
    }
}

/// Claim the strip and hold it until Ctrl-C.
pub fn run(cfg: &sliver_core::Config) -> Result<()> {
    let _takeover = Takeover::claim(cfg)?;
    hold()
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
