//! The Touch Bar DRM panel: one XRGB8888 dumb buffer scanned out by an atomic modeset, updated
//! by writing into the mapped buffer and flushing damaged columns with DIRTYFB. That's what the
//! Mac14,7's `adp` driver needs: one XR24-only primary plane with no FB_DAMAGE_CLIPS and no
//! rotation (measured, docs/hardware.md). The same approach works on T2 `appletbdrm` (XRGB
//! emulated, damage helper).
//!
//! The buffer is in *scanout* orientation. The bar is portrait on every known model (60x2008,
//! 60x2170), so [`DrmPanel::rotated`] is true and landscape column `x` = framebuffer row `x`.
//! Callers write pixels with `sliver_render::panel::blit_xrgb8888(.., Rotate90, ..)` and then
//! call [`DrmPanel::dirty`] with landscape column spans.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use drm::buffer::{Buffer, DrmFourcc};
use drm::control::{
    AtomicCommitFlags, ClipRect, Device as ControlDevice, Mode, ResourceHandle, atomic, connector,
    crtc, dumbbuffer::DumbBuffer, framebuffer, plane, property,
};
use drm::{ClientCapability, Device};

struct Card(File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

fn other(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

fn find_prop<T: ResourceHandle>(card: &Card, h: T, name: &str) -> io::Result<property::Handle> {
    let props = card.get_properties(h)?;
    for id in props.as_props_and_values().0 {
        if card.get_property(*id)?.name().to_bytes() == name.as_bytes() {
            return Ok(*id);
        }
    }
    Err(other(format!("DRM property {name} not found")))
}

/// Buffer width for a scanout `w` px wide: rounded up to a multiple of 64 px, so the row stride
/// (4 bytes/px) is always a multiple of 256 bytes, like tiny-dfr's 64x2008 buffer.
pub const fn padded_width(w: u16) -> u16 {
    w.div_ceil(64) * 64
}

/// Landscape column spans `(x, w)` -> framebuffer clip rects.
pub fn clips_for(spans: &[(u32, u32)], rotated: bool, fb_w: u16, fb_h: u16) -> Vec<ClipRect> {
    spans
        .iter()
        .filter(|&&(_, w)| w > 0)
        .map(|&(x, w)| {
            if rotated {
                let y2 = (x + w).min(u32::from(fb_h)) as u16;
                ClipRect::new(0, x.min(u32::from(fb_h)) as u16, fb_w, y2)
            } else {
                let x2 = (x + w).min(u32::from(fb_w)) as u16;
                ClipRect::new(x.min(u32::from(fb_w)) as u16, 0, x2, fb_h)
            }
        })
        .collect()
}

pub struct DrmPanel {
    card: Card,
    mode: Mode,
    crtc: crtc::Handle,
    db: Option<DumbBuffer>,
    fb: framebuffer::Handle,
    stride: usize,
    dirty_supported: bool,
}

impl DrmPanel {
    /// Opens `card`, becomes master (works once tiny-dfr is gone), and does one atomic modeset
    /// showing a black buffer. Refuses outside the harness.
    pub fn open(path: &Path) -> io::Result<DrmPanel> {
        crate::require_harness().map_err(io::Error::other)?;
        let card = Card(
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_CLOEXEC)
                .open(path)?,
        );
        card.set_client_capability(ClientCapability::UniversalPlanes, true)?;
        card.set_client_capability(ClientCapability::Atomic, true)?;
        card.acquire_master_lock().map_err(|e| {
            other(format!(
                "DRM master on {} (is tiny-dfr still running?): {e}",
                path.display()
            ))
        })?;

        let res = card.resource_handles()?;
        let (conn, mode) = res
            .connectors()
            .iter()
            .filter_map(|&c| card.get_connector(c, false).ok())
            .find(|i| i.state() == connector::State::Connected)
            .and_then(|i| i.modes().first().map(|m| (i.handle(), *m)))
            .ok_or_else(|| other("no connected connector with a mode"))?;
        let crtc = *res.crtcs().first().ok_or_else(|| other("no CRTC"))?;
        let plane = primary_plane(&card, &res, crtc)?;

        let (w, h) = mode.size();
        // Pad the buffer width to a multiple of 64 px (tiny-dfr allocates 64x2008 for the 60 px
        // wide panel). Found on hardware: with a 60 px (240-byte pitch) buffer the adp scanout
        // garbled fine detail (text striped) while a CPU readback looked perfect, consistent with
        // the hardware wanting a 64-byte-aligned stride. Only the mode width is scanned out
        // (SRC_W/CRTC_W below).
        let mut db =
            card.create_dumb_buffer((padded_width(w).into(), h.into()), DrmFourcc::Xrgb8888, 32)?;
        let stride = db.pitch() as usize;
        card.map_dumb_buffer(&mut db)?.as_mut().fill(0);
        let fb = card.add_framebuffer(&db, 24, 32)?;
        let mut panel = DrmPanel {
            card,
            mode,
            crtc,
            db: Some(db),
            fb,
            stride,
            dirty_supported: true,
        };
        panel.modeset(conn, plane)?;
        panel.dirty(&[(0, panel.logical_size().0)]);
        Ok(panel)
    }

    fn modeset(&self, conn: connector::Handle, plane: plane::Handle) -> io::Result<()> {
        let c = &self.card;
        let (w, h) = self.mode.size();
        let mut req = atomic::AtomicModeReq::new();
        req.add_property(
            conn,
            find_prop(c, conn, "CRTC_ID")?,
            property::Value::CRTC(Some(self.crtc)),
        );
        req.add_property(
            self.crtc,
            find_prop(c, self.crtc, "MODE_ID")?,
            c.create_property_blob(&self.mode)?,
        );
        req.add_property(
            self.crtc,
            find_prop(c, self.crtc, "ACTIVE")?,
            property::Value::Boolean(true),
        );
        req.add_property(
            plane,
            find_prop(c, plane, "FB_ID")?,
            property::Value::Framebuffer(Some(self.fb)),
        );
        req.add_property(
            plane,
            find_prop(c, plane, "CRTC_ID")?,
            property::Value::CRTC(Some(self.crtc)),
        );
        for (n, v) in [
            ("SRC_X", 0u64),
            ("SRC_Y", 0),
            ("SRC_W", u64::from(w) << 16),
            ("SRC_H", u64::from(h) << 16),
        ] {
            req.add_property(
                plane,
                find_prop(c, plane, n)?,
                property::Value::UnsignedRange(v),
            );
        }
        req.add_property(
            plane,
            find_prop(c, plane, "CRTC_X")?,
            property::Value::SignedRange(0),
        );
        req.add_property(
            plane,
            find_prop(c, plane, "CRTC_Y")?,
            property::Value::SignedRange(0),
        );
        req.add_property(
            plane,
            find_prop(c, plane, "CRTC_W")?,
            property::Value::UnsignedRange(w.into()),
        );
        req.add_property(
            plane,
            find_prop(c, plane, "CRTC_H")?,
            property::Value::UnsignedRange(h.into()),
        );
        c.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req)?;
        Ok(())
    }

    /// Portrait scanout (true on every known Touch Bar).
    pub fn rotated(&self) -> bool {
        let (w, h) = self.mode.size();
        h > w
    }

    /// Logical (landscape) size: (bar length, bar thickness), e.g. (2008, 60).
    pub fn logical_size(&self) -> (u32, u32) {
        let (w, h) = self.mode.size();
        if self.rotated() {
            (h.into(), w.into())
        } else {
            (w.into(), h.into())
        }
    }

    /// Runs `f(buffer, stride)` on the mapped scanout buffer (XRGB8888 little-endian).
    pub fn with_buffer(&mut self, f: impl FnOnce(&mut [u8], usize)) -> io::Result<()> {
        let stride = self.stride;
        let db = self
            .db
            .as_mut()
            .expect("dumb buffer lives as long as the panel");
        let mut map = self.card.map_dumb_buffer(db)?;
        f(map.as_mut(), stride);
        Ok(())
    }

    /// Flushes landscape column spans `(x, w)` to the panel (DIRTYFB). Drivers that don't
    /// implement it (ENOSYS/EOPNOTSUPP) don't need it, so that's remembered and skipped.
    pub fn dirty(&mut self, spans: &[(u32, u32)]) {
        if !self.dirty_supported {
            return;
        }
        let (w, h) = self.mode.size();
        let clips = clips_for(spans, self.rotated(), w, h);
        if clips.is_empty() {
            return;
        }
        if let Err(e) = self.card.dirty_framebuffer(self.fb, &clips) {
            if matches!(
                e.raw_os_error(),
                Some(libc::ENOSYS) | Some(libc::EOPNOTSUPP)
            ) {
                self.dirty_supported = false;
            }
        }
    }

    /// Turns the CRTC on/off (display power). Untested on hardware so far (docs/hardware.md, open
    /// question 2); the daemon uses backlight 0 for "off" until this is measured.
    pub fn set_active(&self, on: bool) -> io::Result<()> {
        let mut req = atomic::AtomicModeReq::new();
        req.add_property(
            self.crtc,
            find_prop(&self.card, self.crtc, "ACTIVE")?,
            property::Value::Boolean(on),
        );
        self.card
            .atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req)?;
        Ok(())
    }
}

fn primary_plane(
    card: &Card,
    res: &drm::control::ResourceHandles,
    crtc: crtc::Handle,
) -> io::Result<plane::Handle> {
    let planes = card.plane_handles()?;
    for &p in &planes {
        let info = card.get_plane(p)?;
        if !res.filter_crtcs(info.possible_crtcs()).contains(&crtc) {
            continue;
        }
        let props = card.get_properties(p)?;
        let is_primary = props.iter().any(|(id, v)| {
            card.get_property(*id)
                .is_ok_and(|i| i.name().to_bytes() == b"type")
                && *v == 1 // DRM_PLANE_TYPE_PRIMARY
        });
        if is_primary {
            return Ok(p);
        }
    }
    planes.first().copied().ok_or_else(|| other("no plane"))
}

impl Drop for DrmPanel {
    fn drop(&mut self) {
        let _ = self.card.destroy_framebuffer(self.fb);
        if let Some(db) = self.db.take() {
            let _ = self.card.destroy_dumb_buffer(db);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(c: ClipRect) -> (u16, u16, u16, u16) {
        (c.x1(), c.y1(), c.x2(), c.y2())
    }

    #[test]
    fn buffer_width_padded_like_tiny_dfr() {
        assert_eq!(padded_width(60), 64);
        assert_eq!(padded_width(64), 64);
        assert_eq!(padded_width(65), 128);
        assert_eq!(padded_width(2170), 2176);
    }

    #[test]
    fn rotated_clips_are_fb_rows() {
        let c = clips_for(&[(0, 10), (2000, 50), (5, 0)], true, 60, 2008);
        assert_eq!(
            c.into_iter().map(rect).collect::<Vec<_>>(),
            vec![(0, 0, 60, 10), (0, 2000, 60, 2008)]
        );
    }

    #[test]
    fn landscape_clips_are_fb_columns() {
        let c = clips_for(&[(100, 20)], false, 2170, 60);
        assert_eq!(
            c.into_iter().map(rect).collect::<Vec<_>>(),
            vec![(100, 0, 120, 60)]
        );
    }
}
