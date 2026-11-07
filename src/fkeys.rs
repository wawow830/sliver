use std::{
    collections::HashMap,
    error::Error,
    fs::File,
    io::Write,
    os::fd::{AsFd, FromRawFd, OwnedFd},
    os::unix::net::UnixStream,
};

use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_registry::WlRegistry,
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

use crate::{H, W};

pub const KEYS: [(&str, u32); 13] = [
    ("ESC", 1),
    ("F1", 59),
    ("F2", 60),
    ("F3", 61),
    ("F4", 62),
    ("F5", 63),
    ("F6", 64),
    ("F7", 65),
    ("F8", 66),
    ("F9", 67),
    ("F10", 68),
    ("F11", 87),
    ("F12", 88),
];

pub fn key_at(x: f64) -> u32 {
    let i = (x.max(0.0) as i32 * KEYS.len() as i32 / W).min(KEYS.len() as i32 - 1);
    KEYS[i as usize].1
}

pub fn draw() -> Vec<u32> {
    const CAP: u32 = 0x2a2a2a;
    const INK: u32 = 0xffffff;
    const GAP: i32 = 4;
    const SCALE: i32 = 3;
    let mut px = vec![0; (W * H) as usize];
    let n = KEYS.len() as i32;
    for (i, (label, _)) in (0..).zip(KEYS) {
        let (x0, x1) = (i * W / n + GAP / 2, (i + 1) * W / n - GAP / 2);
        for y in GAP..H - GAP {
            px[(y * W + x0) as usize..(y * W + x1) as usize].fill(CAP);
        }
        let text_w = label.len() as i32 * 6 * SCALE - SCALE;
        let (tx, ty) = ((x0 + x1 - text_w) / 2, (H - 7 * SCALE) / 2);
        for (c, ch) in (0..).zip(label.bytes()) {
            for (row, bits) in (0..).zip(glyph(ch)) {
                for col in 0..5 {
                    if bits >> (4 - col) & 1 == 1 {
                        for dy in 0..SCALE {
                            let (x, y) = (tx + (c * 6 + col) * SCALE, ty + row * SCALE + dy);
                            px[(y * W + x) as usize..(y * W + x + SCALE) as usize].fill(INK);
                        }
                    }
                }
            }
        }
    }
    px
}

fn glyph(c: u8) -> [u8; 7] {
    match c {
        b'E' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f],
        b'S' => [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e],
        b'C' => [0x0e, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0e],
        b'F' => [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x10],
        b'0' => [0x0e, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0e],
        b'1' => [0x04, 0x0c, 0x04, 0x04, 0x04, 0x04, 0x0e],
        b'2' => [0x0e, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1f],
        b'3' => [0x1f, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0e],
        b'4' => [0x02, 0x06, 0x0a, 0x12, 0x1f, 0x02, 0x02],
        b'5' => [0x1f, 0x10, 0x1e, 0x01, 0x01, 0x11, 0x0e],
        b'6' => [0x06, 0x08, 0x10, 0x1e, 0x11, 0x11, 0x0e],
        b'7' => [0x1f, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        b'8' => [0x0e, 0x11, 0x11, 0x0e, 0x11, 0x11, 0x0e],
        b'9' => [0x0e, 0x11, 0x11, 0x0f, 0x01, 0x02, 0x0c],
        _ => unreachable!("no glyph for {c}"),
    }
}

const KEYMAP: &[u8] =
    b"xkb_keymap { xkb_keycodes { include \"evdev\" }; xkb_types { include \"complete\" }; \
xkb_compat { include \"complete\" }; xkb_symbols { include \"pc+us\" }; };\0";

struct Keys {
    surface: WlSurface,
    buffer: WlBuffer,
    keyboard: ZwpVirtualKeyboardV1,
    held: HashMap<i32, u32>,
}

pub fn run(stream: UnixStream) {
    if let Err(e) = serve(stream) {
        eprintln!("sliverd: fkeys: {e}");
    }
}

fn serve(stream: UnixStream) -> Result<(), Box<dyn Error>> {
    let conn = Connection::from_socket(stream)?;
    let (globals, mut queue) = registry_queue_init::<Keys>(&conn)?;
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=4, ())?;
    let shm: WlShm = globals.bind(&qh, 1..=1, ())?;
    let base: XdgWmBase = globals.bind(&qh, 1..=1, ())?;
    let seat: WlSeat = globals.bind(&qh, 1..=1, ())?;
    let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ())?;

    let keymap = memfd(KEYMAP)?;
    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    keyboard.keymap(1, keymap.as_fd(), KEYMAP.len() as u32);

    let pixels: Vec<u8> = draw().iter().flat_map(|p| p.to_le_bytes()).collect();
    let pool = shm.create_pool(memfd(&pixels)?.as_fd(), pixels.len() as i32, &qh, ());
    let buffer = pool.create_buffer(0, W, H, W * 4, wl_shm::Format::Xrgb8888, &qh, ());
    pool.destroy();

    let surface = compositor.create_surface(&qh, ());
    let xdg = base.get_xdg_surface(&surface, &qh, ());
    xdg.get_toplevel(&qh, ());
    surface.commit();

    let mut keys = Keys {
        surface,
        buffer,
        keyboard,
        held: HashMap::new(),
    };
    loop {
        queue.blocking_dispatch(&mut keys)?;
    }
}

fn memfd(bytes: &[u8]) -> std::io::Result<File> {
    let fd = unsafe { libc::memfd_create(c"sliver".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.write_all(bytes)?;
    Ok(file)
}

impl Keys {
    fn press(&mut self, id: i32, key: u32) {
        self.held.insert(id, key);
        self.keyboard.key(0, key, 1);
    }

    fn release(&mut self, id: i32) {
        if let Some(key) = self.held.remove(&id) {
            self.keyboard.key(0, key, 0);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Keys {
    fn event(
        keys: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            keys.surface.attach(Some(&keys.buffer), 0, 0);
            keys.surface.damage_buffer(0, 0, W, H);
            keys.surface.commit();
        }
    }
}

impl Dispatch<WlSeat, ()> for Keys {
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

impl Dispatch<WlTouch, ()> for Keys {
    fn event(
        keys: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down { id, x, .. } => keys.press(id, key_at(x)),
            wl_touch::Event::Up { id, .. } => keys.release(id),
            wl_touch::Event::Cancel => {
                for id in keys.held.keys().copied().collect::<Vec<_>>() {
                    keys.release(id);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Keys {
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

impl Dispatch<WlRegistry, GlobalListContents> for Keys {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Keys: WlCompositor);
delegate_noop!(Keys: ignore WlShm);
delegate_noop!(Keys: WlShmPool);
delegate_noop!(Keys: ignore WlBuffer);
delegate_noop!(Keys: ignore WlSurface);
delegate_noop!(Keys: ignore XdgToplevel);
delegate_noop!(Keys: ZwpVirtualKeyboardManagerV1);
delegate_noop!(Keys: ZwpVirtualKeyboardV1);
