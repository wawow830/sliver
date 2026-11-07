mod fkeys;
mod hw;
#[cfg(test)]
mod tests;
mod wayland;

use std::{
    collections::HashMap,
    io,
    os::unix::net::UnixStream,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        input::TouchSlot,
        renderer::{
            Bind, Offscreen,
            damage::OutputDamageTracker,
            element::{AsRenderElements, surface::WaylandSurfaceRenderElement},
            pixman::PixmanRenderer,
        },
    },
    desktop::{Window, WindowSurfaceType},
    input::touch::{DownEvent, MotionEvent, TouchTarget, UpEvent},
    reexports::{
        calloop::{
            Interest, LoopHandle, Mode, PostAction, RegistrationToken,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        pixman::Image,
        wayland_server::{
            Display, ListeningSocket, Resource, backend::ClientId, protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Physical, Point, SERIAL_COUNTER, Scale, Size, Transform},
};

use wayland::{ClientState, Wl, client_of};

pub const W: i32 = 2008;
pub const H: i32 = 60;
const TRANSFORM: Transform = Transform::_90;
const IDLE: Duration = Duration::from_secs(60);

pub fn panel<Kind>() -> Size<i32, Kind> {
    TRANSFORM.transform_size((W, H).into())
}

pub type Keyboard = Box<dyn FnMut(u32, bool) + Send>;

pub trait Bar {
    fn show(&mut self, frame: &[u32]) -> io::Result<()>;
    fn power(&mut self, on: bool);
    fn keyboard(&mut self) -> io::Result<Keyboard>;
}

type Target = (WlSurface, Point<f64, Logical>);

pub enum Touch {
    Down(u32, Point<f64, Logical>),
    Motion(u32, Point<f64, Logical>),
    Up(u32),
    Frame,
    Cancel,
}

pub struct Sliver {
    pub wl: Wl,
    bar: Box<dyn Bar>,
    fkeys: ClientId,
    renderer: PixmanRenderer,
    frame: Image<'static, 'static>,
    damage: OutputDamageTracker,
    age: usize,
    flipping: bool,
    dirty: bool,
    shown: Option<Window>,
    touches: HashMap<u32, Option<Target>>,
    touched: Vec<WlSurface>,
    on: bool,
    fn_held: bool,
    idle_at: Option<Instant>,
    timer: Option<RegistrationToken>,
    start: Instant,
    lh: LoopHandle<'static, Sliver>,
}

impl Sliver {
    pub fn new(lh: LoopHandle<'static, Sliver>, bar: Box<dyn Bar>) -> io::Result<Sliver> {
        let display = Display::<Sliver>::new().map_err(io::Error::other)?;
        let mut wl = Wl::new(display.handle());
        lh.insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, s| {
                unsafe { display.get_mut().dispatch_clients(s)? };
                Ok(PostAction::Continue)
            },
        )
        .map_err(io::Error::other)?;
        let (ours, theirs) = UnixStream::pair()?;
        let fkeys = wl
            .dh
            .insert_client(ours, Arc::new(ClientState::default()))?
            .id();
        std::thread::spawn(|| fkeys::run(theirs));
        let mut renderer = PixmanRenderer::new().map_err(io::Error::other)?;
        let size = panel::<Physical>();
        let frame = renderer
            .create_buffer(Fourcc::Xrgb8888, (size.w, size.h).into())
            .map_err(io::Error::other)?;
        let damage = OutputDamageTracker::from_output(&wl.output);
        let mut sliver = Sliver {
            wl,
            bar,
            fkeys,
            renderer,
            frame,
            damage,
            age: 0,
            flipping: false,
            dirty: false,
            shown: None,
            touches: HashMap::new(),
            touched: Vec::new(),
            on: false,
            fn_held: false,
            idle_at: None,
            timer: None,
            start: Instant::now(),
            lh,
        };
        sliver.activity();
        Ok(sliver)
    }

    pub fn listen(&self, path: &Path) -> io::Result<()> {
        let socket = ListeningSocket::bind_absolute(path.into()).map_err(io::Error::other)?;
        self.lh
            .insert_source(
                Generic::new(socket, Interest::READ, Mode::Level),
                |_, socket, s| {
                    while let Some(stream) = socket.accept()? {
                        if let Err(e) =
                            s.wl.dh
                                .insert_client(stream, Arc::new(ClientState::default()))
                        {
                            eprintln!("sliverd: client: {e}");
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(io::Error::other)?;
        Ok(())
    }

    fn update(&mut self) {
        let shown = match (self.on, self.fn_held) {
            (false, _) => None,
            (true, true) => self
                .wl
                .windows
                .iter()
                .find(|w| client_of(w) == Some(self.fkeys.clone())),
            (true, false) => self.wl.windows.last(),
        }
        .cloned();
        if shown != self.shown {
            self.shown = shown;
            self.cancel_touches();
            self.render();
        }
    }

    fn render(&mut self) {
        if !self.on {
            return;
        }
        if self.flipping {
            self.dirty = true;
            return;
        }
        let elements: Vec<WaylandSurfaceRenderElement<PixmanRenderer>> = match &self.shown {
            Some(w) => w.render_elements(
                &mut self.renderer,
                w.geometry().loc.upscale(-1).to_physical(1),
                Scale::from(1.0),
                1.0,
            ),
            None => Vec::new(),
        };
        let Ok(mut fb) = self.renderer.bind(&mut self.frame) else {
            return;
        };
        let damaged = match self.damage.render_output(
            &mut self.renderer,
            &mut fb,
            self.age,
            &elements,
            [0.0, 0.0, 0.0, 1.0],
        ) {
            Ok(r) => r.damage.is_some_and(|d| !d.is_empty()),
            Err(e) => {
                eprintln!("sliverd: render: {e:?}");
                false
            }
        };
        drop(fb);
        self.age = 1;
        if damaged {
            match self.bar.show(pixels(&self.frame)) {
                Ok(()) => self.flipping = true,
                Err(e) => {
                    eprintln!("sliverd: show: {e}");
                    self.send_frames();
                }
            }
        } else {
            self.send_frames();
        }
    }

    pub fn presented(&mut self) {
        self.flipping = false;
        self.send_frames();
        if std::mem::take(&mut self.dirty) {
            self.render();
        }
    }

    fn send_frames(&self) {
        if let Some(w) = &self.shown {
            w.send_frame(&self.wl.output, self.start.elapsed(), None, |_, _| None);
        }
    }

    pub fn touch(&mut self, touch: Touch) {
        let seat = self.wl.seat.clone();
        let time = self.start.elapsed().as_millis() as u32;
        let seq = SERIAL_COUNTER.next_serial();
        match touch {
            Touch::Down(id, at) => {
                let target = self.surface_at(at);
                if let Some((surface, origin)) = &target {
                    let event = DownEvent {
                        slot: slot(id),
                        location: at - *origin,
                        serial: seq,
                        time,
                    };
                    surface.down(&seat, self, &event, seq);
                    self.touched.push(surface.clone());
                }
                self.touches.insert(id, target);
            }
            Touch::Motion(id, at) => {
                if let Some(Some((surface, origin))) = self.touches.get(&id).cloned() {
                    let event = MotionEvent {
                        slot: slot(id),
                        location: at - origin,
                        time,
                    };
                    surface.motion(&seat, self, &event, seq);
                    self.touched.push(surface);
                }
            }
            Touch::Up(id) => {
                if let Some(Some((surface, _))) = self.touches.remove(&id) {
                    surface.up(
                        &seat,
                        self,
                        &UpEvent {
                            slot: slot(id),
                            serial: seq,
                            time,
                        },
                        seq,
                    );
                    self.touched.push(surface);
                }
            }
            Touch::Frame => {
                for surface in std::mem::take(&mut self.touched) {
                    surface.frame(&seat, self, seq);
                }
            }
            Touch::Cancel => {
                self.cancel_touches();
                self.touches.clear();
            }
        }
        self.activity();
    }

    fn surface_at(&self, at: Point<f64, Logical>) -> Option<Target> {
        let w = self.shown.as_ref()?;
        let offset = w.geometry().loc.to_f64();
        let (surface, loc) = w.surface_under(at + offset, WindowSurfaceType::ALL)?;
        Some((surface, loc.to_f64() - offset))
    }

    fn cancel_touches(&mut self) {
        let seat = self.wl.seat.clone();
        let seq = SERIAL_COUNTER.next_serial();
        let surfaces: Vec<_> = self.touches.values_mut().filter_map(Option::take).collect();
        self.touched.clear();
        for (surface, _) in surfaces {
            if surface.is_alive() {
                surface.cancel(&seat, self, seq);
            }
        }
    }

    pub fn fn_key(&mut self, held: bool) {
        self.fn_held = held;
        self.activity();
        self.update();
    }

    pub fn activity(&mut self) {
        if !self.on {
            self.on = true;
            self.age = 0;
            self.bar.power(true);
            self.update();
        }
        self.idle_at = (self.touches.is_empty() && !self.fn_held).then(|| Instant::now() + IDLE);
        if let (Some(at), None) = (self.idle_at, self.timer) {
            let timer = self.lh.insert_source(Timer::from_deadline(at), |_, _, s| {
                let action = s.tick(Instant::now());
                if let TimeoutAction::Drop = action {
                    s.timer = None;
                }
                action
            });
            self.timer = timer.ok();
        }
    }

    pub fn tick(&mut self, now: Instant) -> TimeoutAction {
        match self.idle_at {
            Some(at) if now >= at => {
                self.idle_at = None;
                self.on = false;
                self.bar.power(false);
                self.update();
                TimeoutAction::Drop
            }
            Some(at) => TimeoutAction::ToInstant(at),
            None => TimeoutAction::Drop,
        }
    }
}

fn slot(id: u32) -> TouchSlot {
    Some(id).into()
}

fn pixels<'a>(image: &'a Image<'static, 'static>) -> &'a [u32] {
    unsafe { std::slice::from_raw_parts(image.data(), image.stride() / 4 * image.height()) }
}

fn main() -> io::Result<()> {
    hw::run()
}
