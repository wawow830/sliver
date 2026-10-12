//! The connection to sliverd (api.md §6): handshake, shared-memory buffers, presenting, and the
//! messages sliverd sends back.
//!
//! Each layer (`base`, `fn`) owns two buffers: the frame and its all-pressed twin. sliverd copies
//! them on `present` and answers `presented`; until then the buffers are busy, so a newer frame
//! for that layer waits and replaces any older waiting one. Frames whose pixels and regions
//! didn't change aren't sent at all (invariant 3).

use std::io;
use std::os::fd::{AsFd, BorrowedFd};

use sliver_ipc::proto::{
    AgentMsg, DaemonMsg, DisplayState, ErrorCode, KeyMode, Layer, PROTO, PanelInfo, PixelFormat,
    Present, Region, Role, TouchPhase,
};
use sliver_ipc::shm::ShmBuffer;
use sliver_ipc::transport::Conn;
use sliver_render::Pixmap;
use sliver_render::panel::{Orientation, blit_xrgb8888, damage};

/// What sliverd told us, minus the bookkeeping the link handles itself (pings, `presented`).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Touch {
        region: u32,
        slot: u8,
        phase: TouchPhase,
        x: f32,
        y: f32,
        time_us: u64,
    },
    Fn(bool),
    Hatch(bool),
    Display(DisplayState),
    Role(Role),
    Error {
        code: ErrorCode,
        msg: String,
    },
    /// sliverd went away. The agent should reconnect (sliverd shows its safe layer meanwhile).
    Closed,
}

struct Slot {
    /// Serial of the present sliverd hasn't confirmed yet.
    inflight: Option<u32>,
    /// The newest frame waiting for the buffers to free up.
    waiting: Option<(Pixmap, Pixmap, Vec<Region>)>,
    /// What sliverd has (or will have) on screen for this layer: image, pressed, regions.
    shown: Option<(Pixmap, Pixmap, Vec<Region>)>,
    image: ShmBuffer,
    pressed: ShmBuffer,
}

pub struct Link {
    conn: Conn,
    pub panel: PanelInfo,
    pub role: Role,
    slots: [Slot; 2],
    next_serial: u32,
    /// Presents actually sent (for tests and stats).
    pub presents: u32,
}

fn idx(layer: Layer) -> usize {
    match layer {
        Layer::Base => 0,
        Layer::Fn => 1,
    }
}

impl Link {
    /// Says hello, waits for `welcome` (blocking), attaches the buffers, and switches the
    /// connection to non-blocking for [`Link::poll`].
    pub fn handshake(conn: Conn, agent: &str) -> io::Result<Link> {
        conn.set_nonblocking(false)?;
        conn.send(&AgentMsg::Hello {
            proto: PROTO,
            agent: agent.into(),
        })?;
        let (panel, role) = loop {
            let Some(p) = conn.recv::<DaemonMsg>()? else {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "sliverd closed",
                ));
            };
            match p.msg {
                Ok(DaemonMsg::Welcome { panel, role, .. }) => break (panel, role),
                Ok(DaemonMsg::Error { code, msg, .. }) => {
                    return Err(io::Error::other(format!(
                        "sliverd refused: {code:?}: {msg}"
                    )));
                }
                _ => continue,
            }
        };
        if panel.format != PixelFormat::Xrgb8888 {
            return Err(io::Error::other(format!(
                "unsupported pixel format {:?}",
                panel.format
            )));
        }
        let new = || ShmBuffer::new(panel.width, panel.height, panel.format);
        let mut slots = Vec::new();
        for _ in 0..2 {
            slots.push(Slot {
                inflight: None,
                waiting: None,
                shown: None,
                image: new()?,
                pressed: new()?,
            });
        }
        let slots: [Slot; 2] = slots.try_into().map_err(|_| io::Error::other("slots"))?;
        for (i, s) in slots.iter().enumerate() {
            let base = i as u32 * 2 + 1;
            conn.send_with_fd(&s.image.attach_msg(base), Some(s.image.as_fd()))?;
            conn.send_with_fd(&s.pressed.attach_msg(base + 1), Some(s.pressed.as_fd()))?;
        }
        conn.set_nonblocking(true)?;
        Ok(Link {
            conn,
            panel,
            role,
            slots,
            next_serial: 1,
            presents: 0,
        })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.conn.as_fd()
    }

    /// Shows a frame on `layer` as soon as the layer's buffers are free. Unchanged frames are
    /// dropped.
    pub fn present(
        &mut self,
        layer: Layer,
        image: &Pixmap,
        pressed: &Pixmap,
        regions: &[Region],
    ) -> io::Result<()> {
        let s = &mut self.slots[idx(layer)];
        let unchanged = match (&s.waiting, &s.shown) {
            (Some((img, _, reg)), _) => img.data() == image.data() && reg == regions,
            (None, Some((img, _, reg))) => img.data() == image.data() && reg == regions,
            _ => false,
        };
        if unchanged {
            return Ok(());
        }
        s.waiting = Some((image.clone(), pressed.clone(), regions.to_vec()));
        self.flush(layer)
    }

    fn flush(&mut self, layer: Layer) -> io::Result<()> {
        let i = idx(layer);
        if self.slots[i].inflight.is_some() || self.role != Role::Active {
            return Ok(());
        }
        let Some((image, pressed, regions)) = self.slots[i].waiting.take() else {
            return Ok(());
        };
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1).max(1);
        let s = &mut self.slots[i];
        let spans = s.shown.as_ref().map(|(prev, _, _)| damage(prev, &image, 8));
        let stride = s.image.stride as usize;
        blit_xrgb8888(
            &image,
            Orientation::Landscape,
            None,
            s.image.pixels_mut(),
            stride,
        );
        blit_xrgb8888(
            &pressed,
            Orientation::Landscape,
            None,
            s.pressed.pixels_mut(),
            stride,
        );
        let base = i as u32 * 2 + 1;
        self.conn.send(&AgentMsg::Present(Present {
            serial,
            layer,
            image: base,
            pressed: Some(base + 1),
            regions: regions.clone(),
            damage: spans.map(|v| v.iter().map(|s| [s.x, s.w]).collect()),
        }))?;
        s.inflight = Some(serial);
        s.shown = Some((image, pressed, regions));
        self.presents += 1;
        Ok(())
    }

    /// Asks sliverd to press keys on our behalf (agent-decided keys: holds, macros).
    pub fn emit_keys(&self, keys: &[u16], mode: KeyMode) -> io::Result<()> {
        self.conn.send(&AgentMsg::EmitKeys {
            keys: keys.to_vec(),
            mode,
        })
    }

    pub fn send(&self, msg: &AgentMsg) -> io::Result<()> {
        self.conn.send(msg)
    }

    /// Reads everything sliverd has sent. Never blocks.
    pub fn poll(&mut self) -> io::Result<Vec<Event>> {
        let mut out = Vec::new();
        loop {
            let p = match self.conn.recv::<DaemonMsg>() {
                Ok(Some(p)) => p,
                Ok(None) => {
                    out.push(Event::Closed);
                    return Ok(out);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(out),
                Err(e) => return Err(e),
            };
            let Ok(msg) = p.msg else { continue };
            match msg {
                DaemonMsg::Presented { serial } => {
                    for layer in [Layer::Base, Layer::Fn] {
                        if self.slots[idx(layer)].inflight == Some(serial) {
                            self.slots[idx(layer)].inflight = None;
                            self.flush(layer)?;
                        }
                    }
                }
                DaemonMsg::Ping { nonce } => self.conn.send(&AgentMsg::Pong { nonce })?,
                DaemonMsg::Touch {
                    region,
                    slot,
                    phase,
                    x,
                    y,
                    time_us,
                } => out.push(Event::Touch {
                    region,
                    slot,
                    phase,
                    x,
                    y,
                    time_us,
                }),
                DaemonMsg::FnKey { held } => out.push(Event::Fn(held)),
                DaemonMsg::Hatch { active } => out.push(Event::Hatch(active)),
                DaemonMsg::Display { state } => out.push(Event::Display(state)),
                DaemonMsg::Role { role } => {
                    self.role = role;
                    if role == Role::Active {
                        // Whatever we showed before may be gone: send it again.
                        for layer in [Layer::Base, Layer::Fn] {
                            let s = &mut self.slots[idx(layer)];
                            if s.waiting.is_none() {
                                s.waiting = s.shown.take();
                            }
                            self.flush(layer)?;
                        }
                    }
                    out.push(Event::Role(role));
                }
                DaemonMsg::Error { code, msg, serial } => {
                    // A rejected present frees its buffers too.
                    for s in &mut self.slots {
                        if serial.is_some() && s.inflight == serial {
                            s.inflight = None;
                            s.shown = None;
                        }
                    }
                    out.push(Event::Error { code, msg });
                }
                DaemonMsg::Welcome { .. } => {}
            }
        }
    }

    /// Clean goodbye: sliverd switches to its safe layer right away.
    pub fn bye(&self) -> io::Result<()> {
        self.conn.send(&AgentMsg::Bye)
    }
}
