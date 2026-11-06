//! Frames: attaching agent buffers, presents, and composing what the panel shows.

use super::*;

impl Daemon {
    pub(super) fn attach(
        &mut self,
        client: ClientId,
        id: u32,
        shape: validate::BufferShape,
        fd: Attachment,
    ) {
        let c = self.clients.get_mut(&client).expect("checked");
        let res = match fd {
            Attachment::None | Attachment::WrongCount => Err("attach_buffer without an fd".into()),
            Attachment::Rejected(why) => Err(why),
            // Re-attaching an id replaces that buffer.
            Attachment::Buffer(buf) => validate::validate_buffer(
                &self.panel,
                c.buffers.len() - usize::from(c.buffers.contains_key(&id)),
                shape,
            )
            .map(|()| {
                let (width, height) = (shape.width, shape.height);
                c.buffers.insert(id, Attached { width, height, buf });
            })
            .map_err(|e| e.to_string()),
        };
        if let Err(msg) = res {
            self.error(client, ErrorCode::InvalidBuffer, msg, None);
        }
    }

    pub(super) fn present(&mut self, client: ClientId, p: Present) {
        let serial = Some(p.serial);
        if let Err(e) = validate::validate_present(&self.panel, &p) {
            return self.error(client, e.code(), e.to_string(), serial);
        }
        let c = self.clients.get_mut(&client).expect("checked");
        let scratch = &mut self.scratch;
        let mut copy = |id: u32| c.buffers.get(&id).and_then(|a| copy_buffer(a, scratch));
        let Some(image) = copy(p.image) else {
            return self.error(
                client,
                ErrorCode::InvalidFrame,
                "unknown buffer".into(),
                serial,
            );
        };
        let pressed = match p.pressed {
            None => None,
            Some(id) => match copy(id) {
                Some(px) => Some(px),
                None => {
                    return self.error(
                        client,
                        ErrorCode::InvalidFrame,
                        "unknown pressed buffer".into(),
                        serial,
                    );
                }
            },
        };
        let frame = Frame {
            serial: p.serial,
            image,
            pressed,
            regions: p.regions,
        };
        // Layouts are fully editable: no safe-key check. The hatch covers invariant 1.
        match p.layer {
            Layer::Base => c.base = Some(frame),
            Layer::Fn => c.fn_layer = Some(frame),
        }
        self.send(client, DaemonMsg::Presented { serial: p.serial });
        self.recompose();
    }

    /// `(base, fn)` views. The agent drives once it has presented a base layer.
    pub(super) fn layers(&self) -> (View, View) {
        let agent = self.active.and_then(|c| self.clients.get(&c));
        match agent {
            Some(c) if c.base.is_some() => (
                View::Agent(Layer::Base),
                if c.fn_layer.is_some() {
                    View::Agent(Layer::Fn)
                } else {
                    View::Builtin(BuiltinKind::FKeys)
                },
            ),
            _ => (View::Builtin(self.safe_base), View::Builtin(self.safe_fn)),
        }
    }

    /// The active agent's accepted frame for `layer`, if any.
    pub fn agent_frame(&self, layer: Layer) -> Option<&Frame> {
        let c = self.clients.get(&self.active?)?;
        match layer {
            Layer::Base => c.base.as_ref(),
            Layer::Fn => c.fn_layer.as_ref(),
        }
    }

    pub(super) fn builtin(&self, kind: BuiltinKind) -> &BuiltinLayer {
        match kind {
            BuiltinKind::FKeys => &self.fkeys,
            BuiltinKind::Media => &self.media,
        }
    }

    /// The regions a touch would hit right now.
    pub fn visible_regions(&self) -> &[Region] {
        self.regions(self.shown_view())
    }

    pub(super) fn regions(&self, view: View) -> &[Region] {
        match view {
            View::Agent(l) => self.agent_frame(l).map_or(&[], |f| &f.regions),
            View::Builtin(k) => &self.builtin(k).regions,
        }
    }

    /// Regions of `view` currently pressed by a finger (same id and rect as when touched).
    pub(super) fn pressed_regions(&self, view: View) -> Vec<Region> {
        let regions = self.regions(view);
        self.router
            .pressed_keys()
            .into_iter()
            .filter_map(|(id, rect)| regions.iter().find(|r| r.id == id && r.rect == rect))
            .cloned()
            .collect()
    }

    /// Rebuilds the frame from the shown layer plus pressed feedback, then flushes the damage.
    pub(super) fn recompose(&mut self) {
        let source = (self.agent_drives() && !self.hatch.active())
            .then_some(self.active)
            .flatten();
        if source != self.touch_source {
            // The agent's frames vanished without the user asking (not the hatch): give the eye a
            // moment before the safe layer takes taps.
            if self.touch_source.is_some() && source.is_none() && !self.hatch.active() {
                self.settle_until = self.now + AGENT_LOSS_SETTLE_MS;
            }
            // Captures refer to the old source's regions: release everything.
            self.touch_source = source;
            let routed = self.router.cancel_all();
            let mut released = false;
            for r in routed {
                if let Routed::Key { code, down } = r {
                    self.hw.keys.key(code, down);
                    released = true;
                }
            }
            if released {
                self.hw.keys.sync();
            }
        }
        let view = self.shown_view();
        let pressed = self.pressed_regions(view);
        match view {
            View::Builtin(kind) => {
                let layer = match kind {
                    BuiltinKind::FKeys => &self.fkeys,
                    BuiltinKind::Media => &self.media,
                };
                if pressed.is_empty() {
                    self.frame.data_mut().copy_from_slice(layer.image.data());
                } else {
                    let ids: Vec<u32> = pressed.iter().map(|r| r.id).collect();
                    layer.render_pressed(&ids, &mut self.renderer, &self.sheet, &mut self.frame);
                }
            }
            View::Agent(layer) => {
                let c = self.active.and_then(|c| self.clients.get(&c));
                let f = c.and_then(|c| match layer {
                    Layer::Base => c.base.as_ref(),
                    Layer::Fn => c.fn_layer.as_ref(),
                });
                let f = f.expect("agent view implies a frame");
                self.frame.data_mut().copy_from_slice(f.image.data());
                for r in &pressed {
                    match &f.pressed {
                        Some(p) => copy_rect(p, &mut self.frame, r.rect),
                        None => highlight_rect(&mut self.frame, r.rect),
                    }
                }
            }
        }
        self.flush();
    }

    pub(super) fn flush(&mut self) {
        if self.idle.state() == DisplayState::Off {
            return; // flushed on wake
        }
        let (dx, dy) = self.shift.offset();
        let mut shifted = self.shifted.take();
        let target = if (dx, dy) == (0, 0) {
            &self.frame
        } else {
            let buf = shifted.get_or_insert_with(|| self.frame.clone());
            shift_into(&self.frame, buf, dx, dy);
            &*buf
        };
        let damage = match &self.shown {
            None => vec![Span {
                x: 0,
                w: target.width(),
            }],
            Some(prev) => panel::damage(prev, target, DAMAGE_MERGE_GAP),
        };
        if !damage.is_empty() {
            self.hw.display.flush(target, &damage);
            match &mut self.shown {
                Some(s) => s.data_mut().copy_from_slice(target.data()),
                None => self.shown = Some(target.clone()),
            }
        }
        self.shifted = shifted;
    }
}

/// `dst` = `src` moved by `(dx, dy)`, black where nothing lands (pixel shift). Same size.
fn shift_into(src: &Pixmap, dst: &mut Pixmap, dx: i32, dy: i32) {
    let (w, h) = (src.width() as i32, src.height() as i32);
    let row = w as usize * 4;
    let d = dst.data_mut();
    for px in d.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 255]);
    }
    let s = src.data();
    let (x0, x1) = (dx.max(0), (w + dx).min(w)); // destination columns that receive pixels
    if x0 >= x1 {
        return;
    }
    for y in 0..h {
        let sy = y - dy;
        if !(0..h).contains(&sy) {
            continue;
        }
        let (dst_at, src_at) = (y as usize * row, sy as usize * row);
        let len = (x1 - x0) as usize * 4;
        let (ds, ss) = (x0 as usize * 4, (x0 - dx) as usize * 4);
        d[dst_at + ds..dst_at + ds + len].copy_from_slice(&s[src_at + ss..src_at + ss + len]);
    }
}

/// Copies an XRGB8888 agent buffer into a pixmap. `None` if it's somehow the wrong size.
fn copy_buffer(a: &Attached, scratch: &mut Vec<u8>) -> Option<Pixmap> {
    let mut px = Pixmap::new(a.width, a.height)?;
    a.buf.copy_tight(scratch);
    if scratch.len() != px.data().len() {
        return None;
    }
    for (o, s) in px
        .data_mut()
        .chunks_exact_mut(4)
        .zip(scratch.chunks_exact(4))
    {
        o.copy_from_slice(&[s[2], s[1], s[0], 255]);
    }
    Some(px)
}

fn rect_rows(dst: &Pixmap, r: Rect) -> (usize, usize, usize, usize) {
    let (w, h) = (dst.width(), dst.height());
    let x0 = r.x.min(w) as usize;
    let x1 = (r.right().min(u64::from(w))) as usize;
    let y0 = r.y.min(h) as usize;
    let y1 = (r.bottom().min(u64::from(h))) as usize;
    (x0, x1, y0, y1)
}

fn copy_rect(src: &Pixmap, dst: &mut Pixmap, r: Rect) {
    let (x0, x1, y0, y1) = rect_rows(dst, r);
    let row = dst.width() as usize * 4;
    let s = src.data();
    let d = dst.data_mut();
    for y in y0..y1 {
        let range = y * row + x0 * 4..y * row + x1 * 4;
        d[range.clone()].copy_from_slice(&s[range]);
    }
}

/// Generic pressed feedback when the agent sent no pressed image: lighten by 25%.
fn highlight_rect(dst: &mut Pixmap, r: Rect) {
    let (x0, x1, y0, y1) = rect_rows(dst, r);
    let row = dst.width() as usize * 4;
    let d = dst.data_mut();
    for y in y0..y1 {
        for px in d[y * row + x0 * 4..y * row + x1 * 4].chunks_exact_mut(4) {
            for c in &mut px[..3] {
                *c += (255 - *c) / 4;
            }
        }
    }
}
