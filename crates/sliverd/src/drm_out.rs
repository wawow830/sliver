//! Hardware takeover: claim the touchbar panel and hold it.
//!
//! The M2 13" touchbar is a MIPI DSI panel, natively portrait with a
//! 60x2008 mode. We allocate a dumb buffer at the panel's native mode,
//! render our 2008x60 strip through cairo with a 90° rotation, copy it
//! in, and set the CRTC. Ctrl-C hands the CRTC back to whatever had it.

use std::fs::{File, OpenOptions};
use std::os::unix::io::{AsFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use drm::buffer::{Buffer as _, DrmFourcc};
use drm::Device as _;
use drm::control::{self, connector, Device as _};

#[derive(Debug)]
struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl control::Device for Card {}

pub struct Takeover {
    card: Card,
    crtc: control::crtc::Handle,
    conn: connector::Handle,
    old_crtc: Option<control::crtc::Info>,
    mode: control::Mode,
    fb: control::framebuffer::Handle,
    db: control::dumbbuffer::DumbBuffer,
    surface: cairo::ImageSurface,
    width: u32,
    height: u32,
}

impl Takeover {
    pub fn claim(cfg: &sliver_core::Config) -> Result<Takeover> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/dri/card1")
            .context("opening /dev/dri/card1 (need root, or the video group)")?;
        let card = Card(file);
        card.acquire_master_lock()
            .context("becoming DRM master (root usually required)")?;

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
        let (width, height) = mode.size();
        let (width, height) = (u32::from(width), u32::from(height));
        eprintln!("panel mode: {width}x{height}");

        let crtc = conn
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| res.crtcs().first().copied())
            .context("no crtc available")?;
        let old_crtc = card.get_crtc(crtc).ok();

        let db = card.create_dumb_buffer((width, height), DrmFourcc::Xrgb8888, 32)?;
        let fb = card.add_framebuffer(&db, 24, 32)?;

        // Our canvas, panel-native orientation (portrait). The render step
        // rotates the 2008x60 strip into it.
        let px_stride = width * 4;
        let pixels = vec![0u8; (px_stride * height) as usize];
        let surface = cairo::ImageSurface::create_for_data(
            pixels,
            cairo::Format::ARgb32,
            width as i32,
            height as i32,
            px_stride as i32,
        )?;

        let mut t = Takeover {
            card,
            crtc,
            conn: conn.handle(),
            old_crtc,
            mode,
            fb,
            db,
            surface,
            width,
            height,
        };
        t.repaint(cfg)?;
        t.card
            .set_crtc(t.crtc, Some(t.fb), (0, 0), &[t.conn], Some(t.mode))
            .context("setting CRTC (need DRM master — run as root if this fails)")?;
        Ok(t)
    }

    /// Render the strip rotated into the panel buffer, then push it out.
    pub fn repaint(&mut self, cfg: &sliver_core::Config) -> Result<()> {
        {
            let cr = cairo::Context::new(&self.surface)?;
            // Logical (x: 0..2008, y: 0..60) -> panel (bx: 60-y, by: x).
            // If the strip ever reads upside-down, flip the sign of the
            // rotation and translate by (0, -STRIP_W) instead.
            cr.translate(f64::from(self.width), 0.0);
            cr.rotate(std::f64::consts::FRAC_PI_2);
            sliver_core::render(cfg, &cr)?;
        }
        self.surface.flush();

        let src = self.surface.data()?;
        let row = (self.width * 4) as usize;
        let dstride = self.db.pitch() as usize;
        let mut map = self.card.map_dumb_buffer(&mut self.db)?;
        let dst: &mut [u8] = map.as_mut();
        let sstride = row;
        for y in 0..self.height as usize {
            dst[y * dstride..y * dstride + row].copy_from_slice(&src[y * sstride..y * sstride + row]);
        }
        Ok(())
    }
}

impl Drop for Takeover {
    fn drop(&mut self) {
        if let Some(old) = &self.old_crtc {
            let _ = self.card.set_crtc(
                self.crtc,
                old.framebuffer(),
                old.position(),
                &[self.conn],
                old.mode(),
            );
        }
        let _ = self.card.destroy_framebuffer(self.fb);
        let _ = self.card.destroy_dumb_buffer(self.db);
        let _ = self.card.release_master_lock();
    }
}

/// Claim the strip and hold it until Ctrl-C.
pub fn run(cfg: &sliver_core::Config) -> Result<()> {
    let _takeover = Takeover::claim(cfg)?;
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
