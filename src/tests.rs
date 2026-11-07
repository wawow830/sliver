use std::{
    fs::File,
    io::Write,
    os::fd::{AsFd, FromRawFd, OwnedFd},
    path::Path,
    sync::{Mutex, MutexGuard},
    thread,
};

use smithay::{reexports::calloop::EventLoop, utils::Rectangle};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
        wl_touch::{self, WlTouch},
    },
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

use super::*;

const RED: u32 = 0xff0000;
const BLUE: u32 = 0x0000ff;

#[derive(Debug, PartialEq)]
enum Device {
    Created(usize),
    Key(usize, u32, bool),
    Destroyed(usize),
}

#[derive(Default)]
struct Log {
    frame: Vec<u32>,
    shows: usize,
    on: bool,
    keyboards: usize,
    keys: Vec<Device>,
}

struct FakeBar(Arc<Mutex<Log>>);

struct FakeKeyboard(usize, Arc<Mutex<Log>>);

impl Drop for FakeKeyboard {
    fn drop(&mut self) {
        self.1.lock().unwrap().keys.push(Device::Destroyed(self.0));
    }
}

impl Bar for FakeBar {
    fn show(&mut self, frame: &[u32]) {
        let mut log = self.0.lock().unwrap();
        log.frame = frame.to_vec();
        log.shows += 1;
    }

    fn power(&mut self, on: bool) {
        self.0.lock().unwrap().on = on;
    }

    fn keyboard(&mut self) -> io::Result<Keyboard> {
        let mut log = self.0.lock().unwrap();
        let id = log.keyboards;
        log.keyboards += 1;
        log.keys.push(Device::Created(id));
        let device = FakeKeyboard(id, self.0.clone());
        Ok(Box::new(move |code, pressed| {
            device
                .1
                .lock()
                .unwrap()
                .keys
                .push(Device::Key(device.0, code, pressed))
        }))
    }
}

struct Bench {
    lp: EventLoop<'static, Sliver>,
    s: Sliver,
    log: Arc<Mutex<Log>>,
}

impl Bench {
    fn new() -> Bench {
        let lp = EventLoop::try_new().unwrap();
        let log = Arc::new(Mutex::new(Log::default()));
        let s = Sliver::new(lp.handle(), Box::new(FakeBar(log.clone()))).unwrap();
        let mut b = Bench { lp, s, log };
        b.until("the F-keys", Bench::shows_fkeys);
        b
    }

    fn pump(&mut self) {
        self.lp
            .dispatch(Some(Duration::from_millis(1)), &mut self.s)
            .unwrap();
        self.s.wl.dh.flush_clients().unwrap();
        if self.s.flipping {
            self.s.presented();
        }
    }

    fn until(&mut self, what: &str, done: impl Fn(&Bench) -> bool) {
        for _ in 0..3000 {
            if done(self) {
                return;
            }
            self.pump();
        }
        panic!("never saw {what}");
    }

    /// Long enough for every client to have been answered.
    fn settle(&mut self) {
        for _ in 0..50 {
            self.pump();
        }
    }

    fn connect(&mut self) -> UnixStream {
        let (ours, theirs) = UnixStream::pair().unwrap();
        self.s
            .wl
            .dh
            .insert_client(ours, Arc::new(ClientState::default()))
            .unwrap();
        theirs
    }

    fn touch(&mut self, touches: impl IntoIterator<Item = Touch>) {
        for touch in touches {
            self.s.touch(touch);
        }
        self.s.touch(Touch::Frame);
    }

    /// Landscape XRGB, as clients drew it.
    fn picture(&self) -> Vec<u32> {
        let log = self.log.lock().unwrap();
        let panel = TRANSFORM.transform_size(Size::<i32, Logical>::from((W, H)));
        let mut out = vec![0; (W * H) as usize];
        for y in 0..H {
            for x in 0..W {
                let p = TRANSFORM
                    .invert()
                    .transform_rect_in(
                        Rectangle::<i32, Logical>::new((x, y).into(), (1, 1).into()),
                        &Size::from((W, H)),
                    )
                    .loc;
                out[(y * W + x) as usize] = log
                    .frame
                    .get((p.y * panel.w + p.x) as usize)
                    .map_or(0, |px| px & 0xffffff);
            }
        }
        out
    }

    fn shows_fkeys(&self) -> bool {
        self.picture() == fkeys::draw()
    }

    fn shows(&self, color: u32) -> bool {
        self.picture().iter().all(|&px| px == color)
    }

    fn png(&self, name: &str) {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/target"));
        let file = File::create(dir.join(format!("{name}.png"))).unwrap();
        let mut png = png::Encoder::new(file, W as u32, H as u32);
        png.set_color(png::ColorType::Rgb);
        let rgb: Vec<u8> = self
            .picture()
            .iter()
            .flat_map(|px| [(px >> 16) as u8, (px >> 8) as u8, *px as u8])
            .collect();
        png.write_header().unwrap().write_image_data(&rgb).unwrap();
    }

    fn log(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap()
    }
}

/// A window filled with one colour that logs its touch events.
struct App {
    stream: UnixStream,
    events: Arc<Mutex<Vec<String>>>,
}

impl App {
    fn spawn(b: &mut Bench, color: u32) -> App {
        let stream = b.connect();
        let events = Arc::new(Mutex::new(Vec::new()));
        let (s, e) = (stream.try_clone().unwrap(), events.clone());
        thread::spawn(move || app(s, color, e));
        App { stream, events }
    }

    fn saw(&self, event: &str) -> bool {
        self.events.lock().unwrap().iter().any(|e| e == event)
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn kill(self) {
        self.stream.shutdown(std::net::Shutdown::Both).unwrap();
    }
}

struct AppState {
    surface: WlSurface,
    buffer: WlBuffer,
    events: Arc<Mutex<Vec<String>>>,
}

fn app(stream: UnixStream, color: u32, events: Arc<Mutex<Vec<String>>>) {
    let conn = Connection::from_socket(stream).unwrap();
    let (globals, mut queue) = registry_queue_init::<AppState>(&conn).unwrap();
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=4, ()).unwrap();
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).unwrap();
    let base: XdgWmBase = globals.bind(&qh, 1..=1, ()).unwrap();
    let _: WlSeat = globals.bind(&qh, 1..=7, ()).unwrap();
    let pixels: Vec<u8> = (0..W * H).flat_map(|_| color.to_le_bytes()).collect();
    let pool = shm.create_pool(memfd(&pixels).as_fd(), pixels.len() as i32, &qh, ());
    let buffer = pool.create_buffer(0, W, H, W * 4, wl_shm::Format::Xrgb8888, &qh, ());
    let surface = compositor.create_surface(&qh, ());
    let xdg = base.get_xdg_surface(&surface, &qh, ());
    xdg.get_toplevel(&qh, ());
    surface.commit();
    let mut state = AppState {
        surface,
        buffer,
        events,
    };
    while queue.blocking_dispatch(&mut state).is_ok() {}
}

fn memfd(bytes: &[u8]) -> File {
    let fd = unsafe { libc::memfd_create(c"test".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0);
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.write_all(bytes).unwrap();
    file
}

impl Dispatch<XdgSurface, ()> for AppState {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            app.surface.attach(Some(&app.buffer), 0, 0);
            app.surface.damage_buffer(0, 0, W, H);
            app.surface.commit();
        }
    }
}

impl Dispatch<WlSeat, ()> for AppState {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
            && caps.contains(wl_seat::Capability::Touch)
        {
            seat.get_touch(qh, ());
        }
    }
}

impl Dispatch<WlTouch, ()> for AppState {
    fn event(
        app: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let name = match event {
            wl_touch::Event::Down { id, x, y, .. } => format!("down {id} {x} {y}"),
            wl_touch::Event::Motion { id, x, y, .. } => format!("motion {id} {x} {y}"),
            wl_touch::Event::Up { id, .. } => format!("up {id}"),
            wl_touch::Event::Frame => "frame".into(),
            wl_touch::Event::Cancel => "cancel".into(),
            _ => return,
        };
        app.events.lock().unwrap().push(name);
    }
}

impl Dispatch<XdgWmBase, ()> for AppState {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for AppState {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(AppState: WlCompositor);
delegate_noop!(AppState: ignore WlShm);
delegate_noop!(AppState: WlShmPool);
delegate_noop!(AppState: ignore WlBuffer);
delegate_noop!(AppState: ignore WlSurface);
delegate_noop!(AppState: ignore XdgToplevel);
delegate_noop!(AppState: ZwpVirtualKeyboardManagerV1);
delegate_noop!(AppState: ZwpVirtualKeyboardV1);

/// Types `code` on a virtual keyboard of its own and disconnects.
fn type_and_quit(stream: UnixStream, code: u32) {
    let conn = Connection::from_socket(stream).unwrap();
    let (globals, queue) = registry_queue_init::<AppState>(&conn).unwrap();
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
    let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    keyboard.keymap(1, memfd(b"\0").as_fd(), 1);
    keyboard.key(0, code, 1);
    conn.roundtrip().unwrap();
}

fn center() -> Point<f64, Logical> {
    (W as f64 / 2.0, H as f64 / 2.0).into()
}

fn key_x(i: usize) -> f64 {
    (i as f64 + 0.5) * W as f64 / fkeys::KEYS.len() as f64
}

#[test]
fn nothing_open_shows_fkeys() {
    let b = Bench::new();
    b.png("fkeys");
}

#[test]
fn newest_window_is_shown() {
    let mut b = Bench::new();
    let red = App::spawn(&mut b, RED);
    b.until("red", |b| b.shows(RED));
    let blue = App::spawn(&mut b, BLUE);
    b.until("blue", |b| b.shows(BLUE));
    blue.kill();
    b.until("red again", |b| b.shows(RED));
    red.kill();
    b.until("the F-keys again", Bench::shows_fkeys);
}

#[test]
fn fn_shows_fkeys() {
    let mut b = Bench::new();
    let _red = App::spawn(&mut b, RED);
    b.until("red", |b| b.shows(RED));
    b.s.fn_key(true);
    b.until("the F-keys", Bench::shows_fkeys);
    b.s.fn_key(false);
    b.until("red", |b| b.shows(RED));
}

#[test]
fn fkeys_type() {
    let mut b = Bench::new();
    b.touch([Touch::Down(0, (key_x(5), 30.0).into())]);
    b.until("F5 down", |b| {
        b.log().keys.contains(&Device::Key(0, 63, true))
    });
    b.touch([Touch::Up(0)]);
    b.until("F5 up", |b| {
        b.log().keys.contains(&Device::Key(0, 63, false))
    });
}

#[test]
fn virtual_keyboard_is_its_own_device() {
    let mut b = Bench::new();
    let stream = b.connect();
    thread::spawn(move || type_and_quit(stream, 30));
    b.until("the device gone", |b| {
        b.log().keys.contains(&Device::Destroyed(1))
    });
    assert_eq!(
        b.log().keys[1..],
        [
            Device::Created(1),
            Device::Key(1, 30, true),
            Device::Destroyed(1)
        ]
    );
}

#[test]
fn touches_go_to_the_window_under_them() {
    let mut b = Bench::new();
    let red = App::spawn(&mut b, RED);
    b.until("red", |b| b.shows(RED));
    b.touch([Touch::Down(0, (100.0, 10.0).into())]);
    b.touch([Touch::Motion(0, (110.0, 12.0).into())]);
    b.touch([Touch::Up(0)]);
    b.until("up", |_| red.saw("up 0"));
    assert_eq!(
        red.events(),
        [
            "down 0 100 10",
            "frame",
            "motion 0 110 12",
            "frame",
            "up 0",
            "frame"
        ]
    );
}

#[test]
fn a_change_cancels_touches() {
    let mut b = Bench::new();
    let red = App::spawn(&mut b, RED);
    b.until("red", |b| b.shows(RED));
    b.touch([Touch::Down(0, center())]);
    b.until("down", |_| red.saw("frame"));
    b.s.fn_key(true);
    b.until("cancel", |_| red.saw("cancel"));
    b.touch([Touch::Motion(0, center()), Touch::Up(0)]);
    b.s.fn_key(false);
    b.touch([Touch::Down(1, center())]);
    b.until("the next touch", |_| red.events().len() > 3);
    assert_eq!(
        red.events(),
        [
            "down 0 1004 30",
            "frame",
            "cancel",
            "down 1 1004 30",
            "frame"
        ]
    );
}

#[test]
fn a_cancelled_fkey_is_released() {
    let mut b = Bench::new();
    let _red = App::spawn(&mut b, RED);
    b.until("red", |b| b.shows(RED));
    b.s.fn_key(true);
    b.until("the F-keys", Bench::shows_fkeys);
    b.touch([Touch::Down(0, (key_x(0), 30.0).into())]);
    b.until("Esc down", |b| {
        b.log().keys.contains(&Device::Key(0, 1, true))
    });
    b.s.fn_key(false);
    b.until("Esc up", |b| {
        b.log().keys.contains(&Device::Key(0, 1, false))
    });
}

#[test]
fn idle_bar_goes_off_and_a_touch_only_wakes_it() {
    let mut b = Bench::new();
    let red = App::spawn(&mut b, RED);
    b.until("red", |b| b.shows(RED));
    b.s.tick(Instant::now() + IDLE);
    assert!(!b.log().on);
    let shows = b.log().shows;
    b.touch([Touch::Down(0, center())]);
    assert!(b.log().on);
    b.until("red", |b| b.log().shows > shows && b.shows(RED));
    b.touch([Touch::Up(0)]);
    b.settle();
    assert_eq!(red.events(), Vec::<String>::new());
    b.touch([Touch::Down(1, center())]);
    b.until("the next touch", |_| red.saw("frame"));
}

#[test]
fn off_draws_nothing() {
    let mut b = Bench::new();
    b.s.tick(Instant::now() + IDLE);
    let shows = b.log().shows;
    let red = App::spawn(&mut b, RED);
    b.settle();
    assert_eq!(b.log().shows, shows);
    b.s.activity();
    b.until("red", |b| b.shows(RED));
    drop(red);
}

#[test]
fn held_keeps_the_bar_on() {
    let mut b = Bench::new();
    b.touch([Touch::Down(0, center())]);
    b.s.tick(Instant::now() + IDLE);
    assert!(b.log().on);
    b.touch([Touch::Up(0)]);
    b.s.fn_key(true);
    b.s.tick(Instant::now() + IDLE);
    assert!(b.log().on);
    b.s.fn_key(false);
    b.s.tick(Instant::now() + IDLE);
    assert!(!b.log().on);
}
