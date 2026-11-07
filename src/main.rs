mod client;
mod fkeys;
mod hw;
#[cfg(test)]
mod tests;
mod wayland;

use std::{
    collections::HashMap,
    fmt, io,
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        input::TouchSlot,
        renderer::{
            Bind, Offscreen,
            damage::{self, OutputDamageTracker},
            element::{AsRenderElements, surface::WaylandSurfaceRenderElement},
            pixman::PixmanRenderer,
        },
    },
    desktop::{Window, WindowSurfaceType, space::SpaceElement},
    input::touch::{DownEvent, MotionEvent, TouchTarget, UpEvent},
    reexports::{
        calloop::{
            Interest, LoopHandle, Mode, PostAction,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        pixman::Image,
        wayland_server::{Display, ListeningSocket, Resource, protocol::wl_surface::WlSurface},
    },
    utils::{Logical, Physical, Point, Rectangle, SERIAL_COUNTER, Scale, Size, Transform},
};

use wayland::Wl;

pub const W: i32 = 2008;
pub const H: i32 = 60;
const TRANSFORM: Transform = Transform::_270;
const IDLE: Duration = Duration::from_secs(60);
const HZ: i32 = 60;
const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

pub fn panel<Kind>() -> Size<i32, Kind> {
    TRANSFORM.transform_size((W, H).into())
}

pub type Keyboard = Box<dyn FnMut(u32, bool) + Send>;

pub trait Bar {
    fn show(&mut self, frame: &[u8]);
    fn power(&mut self, on: bool);
    fn keyboard(&mut self) -> Keyboard;
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
    renderer: PixmanRenderer,
    frame: Image<'static, 'static>,
    damage: OutputDamageTracker,
    dirty: bool,
    presenting: bool,
    shown: Option<Window>,
    touches: HashMap<u32, Option<Target>>,
    unframed: Option<WlSurface>,
    on: bool,
    fn_held: bool,
    last: Instant,
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
                s.dirty = true;
                Ok(PostAction::Continue)
            },
        )
        .map_err(io::Error::other)?;
        let (ours, theirs) = UnixStream::pair()?;
        wl.connect(ours)?;
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
            renderer,
            frame,
            damage,
            dirty: false,
            presenting: false,
            shown: None,
            touches: HashMap::new(),
            unframed: None,
            on: false,
            fn_held: false,
            last: Instant::now(),
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
                        if let Err(e) = s.wl.connect(stream) {
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
        let windows = &self.wl.windows;
        let shown = match (self.on, self.fn_held) {
            (false, _) => None,
            (true, true) => windows.first().cloned(),
            (true, false) => windows.last().cloned(),
        };
        if shown != self.shown {
            if let Some(w) = &self.shown {
                w.output_leave(&self.wl.output);
            }
            self.shown = shown;
            self.cancel_touches();
            self.dirty = true;
        }
    }

    pub fn refresh(&mut self) {
        if self.dirty && self.on && !self.presenting {
            self.draw();
        }
        let _ = self.wl.dh.flush_clients();
    }

    fn draw(&mut self) {
        self.dirty = false;
        self.presenting = true;
        let elements: Vec<WaylandSurfaceRenderElement<PixmanRenderer>> = match &self.shown {
            Some(w) => {
                let at = w.geometry().loc;
                w.output_enter(&self.wl.output, Rectangle::new(at, (W, H).into()));
                w.render_elements(
                    &mut self.renderer,
                    at.upscale(-1).to_physical(1),
                    Scale::from(1.0),
                    1.0,
                )
            }
            None => Vec::new(),
        };
        self.renderer
            .bind(&mut self.frame)
            .map_err(damage::Error::Rendering)
            .and_then(|mut fb| {
                self.damage
                    .render_output(&mut self.renderer, &mut fb, 1, &elements, BLACK)
                    .map(drop)
            })
            .unwrap_or_else(|e| fail("render", e));
        self.bar.show(pixels(&self.frame));
    }

    pub fn presented(&mut self) {
        self.presenting = false;
        if let Some(w) = &self.shown {
            let output = &self.wl.output;
            w.send_frame(output, self.start.elapsed(), None, |_, _| {
                Some(output.clone())
            });
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
                    self.unframed = Some(surface.clone());
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
                    self.unframed = Some(surface);
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
                    self.unframed = Some(surface);
                }
            }
            Touch::Frame => {
                if let Some(surface) = self.unframed.take() {
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
        self.unframed = None;
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
        self.last = Instant::now();
        if !self.on {
            self.power(true);
            let idle = Timer::from_duration(IDLE);
            let _ = self
                .lh
                .insert_source(idle, |_, _, s| s.tick(Instant::now()));
        }
    }

    pub fn tick(&mut self, now: Instant) -> TimeoutAction {
        let due = self.last + IDLE;
        if !self.touches.is_empty() || self.fn_held {
            TimeoutAction::ToDuration(IDLE)
        } else if now < due {
            TimeoutAction::ToInstant(due)
        } else {
            self.power(false);
            TimeoutAction::Drop
        }
    }

    fn power(&mut self, on: bool) {
        self.on = on;
        self.bar.power(on);
        self.update();
    }
}

fn slot(id: u32) -> TouchSlot {
    Some(id).into()
}

fn pixels<'a>(image: &'a Image<'static, 'static>) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(image.data().cast(), image.stride() * image.height()) }
}

pub fn fail(what: &str, e: impl fmt::Display) -> ! {
    eprintln!("sliverd: {what}: {e}");
    std::process::exit(1)
}

fn main() -> io::Result<()> {
    hw::run()
}
