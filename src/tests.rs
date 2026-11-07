use std::{
    fs::File,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    thread,
};

use smithay::{reexports::calloop::EventLoop, utils::Rectangle};
use wayland_client::protocol::wl_touch;

use super::{client::Client, *};

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
    fn show(&mut self, frame: &[u8]) -> io::Result<()> {
        let mut log = self.0.lock().unwrap();
        log.frame = frame
            .chunks_exact(4)
            .map(|px| u32::from_ne_bytes(px.try_into().unwrap()))
            .collect();
        log.shows += 1;
        Ok(())
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

    fn settle(&mut self) {
        for _ in 0..50 {
            self.pump();
        }
    }

    fn connect(&mut self) -> UnixStream {
        let (ours, theirs) = UnixStream::pair().unwrap();
        self.s.wl.connect(ours).unwrap();
        theirs
    }

    fn touch(&mut self, touches: impl IntoIterator<Item = Touch>) {
        for touch in touches {
            self.s.touch(touch);
        }
        self.s.touch(Touch::Frame);
    }

    fn picture(&self) -> Vec<u32> {
        let log = self.log.lock().unwrap();
        let panel = panel::<Logical>();
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

struct App {
    stream: UnixStream,
    events: Arc<Mutex<Vec<String>>>,
}

impl App {
    fn spawn(b: &mut Bench, color: u32) -> App {
        let stream = b.connect();
        let events = Arc::new(Mutex::new(Vec::new()));
        let (s, e) = (stream.try_clone().unwrap(), events.clone());
        thread::spawn(move || {
            let pixels = vec![color; (W * H) as usize];
            let _ = Client::connect(s).unwrap().show(&pixels, move |event| {
                let name = match event {
                    wl_touch::Event::Down { id, x, y, .. } => format!("down {id} {x} {y}"),
                    wl_touch::Event::Motion { id, x, y, .. } => format!("motion {id} {x} {y}"),
                    wl_touch::Event::Up { id, .. } => format!("up {id}"),
                    wl_touch::Event::Frame => "frame".into(),
                    wl_touch::Event::Cancel => "cancel".into(),
                    _ => return,
                };
                e.lock().unwrap().push(name);
            });
        });
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
    let theirs = stream.try_clone().unwrap();
    thread::spawn(move || {
        let client = Client::connect(theirs).unwrap();
        client.keyboard().unwrap().key(0, 30, 1);
        let _ = client.show(&vec![RED; (W * H) as usize], |_| {});
    });
    b.until("A down", |b| {
        b.log().keys.contains(&Device::Key(1, 30, true))
    });
    stream.shutdown(std::net::Shutdown::Both).unwrap();
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

#[test]
#[ignore]
fn quickshell_draws_and_types() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let build = root.join("target/bar");
    let cmake = |args: &[&str]| {
        assert!(
            std::process::Command::new("cmake")
                .args(args)
                .status()
                .unwrap()
                .success()
        )
    };
    cmake(&[
        "-S",
        root.join("bar/Sliver").to_str().unwrap(),
        "-B",
        build.to_str().unwrap(),
        "-G",
        "Ninja",
        "-Wno-dev",
    ]);
    cmake(&["--build", build.to_str().unwrap()]);

    let mut b = Bench::new();
    let socket = build.join("wayland");
    let _ = std::fs::remove_file(&socket);
    b.s.listen(&socket).unwrap();
    let mut qs = std::process::Command::new("qs")
        .args(["-p", root.join("bar").to_str().unwrap()])
        .env("WAYLAND_DISPLAY", &socket)
        .env("QT_QPA_PLATFORM", "wayland")
        .env("QML_IMPORT_PATH", build.join("qml"))
        .spawn()
        .unwrap();
    b.until("Quickshell's window", |b| {
        b.s.wl.windows.len() == 2 && b.picture().contains(&0x2a2a2a)
    });
    b.settle();
    b.png("quickshell");
    let shows = b.log().shows;
    for _ in 0..500 {
        b.pump();
    }
    assert_eq!(b.log().shows, shows, "an idle bar redraws");
    b.touch([Touch::Down(0, (100.0, 30.0).into())]);
    b.touch([Touch::Up(0)]);
    b.until("Esc", |b| b.log().keys.contains(&Device::Key(1, 1, false)));
    qs.kill().unwrap();
    qs.wait().unwrap();
    assert_eq!(
        b.log().keys[1..],
        [
            Device::Created(1),
            Device::Key(1, 1, true),
            Device::Key(1, 1, false)
        ]
    );
}
