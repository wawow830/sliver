use std::{
    convert::Infallible,
    error::Error,
    fs::File,
    io::{self, Write},
    os::{
        fd::{AsFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
};

use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop,
    globals::{GlobalList, GlobalListContents, registry_queue_init},
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

use crate::{H, W};

const KEYMAP: &[u8] =
    b"xkb_keymap { xkb_keycodes { include \"evdev\" }; xkb_types { include \"complete\" }; \
xkb_compat { include \"complete\" }; xkb_symbols { include \"pc+us\" }; };\0";

pub struct Client {
    globals: GlobalList,
    queue: EventQueue<Window>,
    seat: WlSeat,
}

struct Window {
    surface: WlSurface,
    buffer: WlBuffer,
    touch: Box<dyn FnMut(wl_touch::Event)>,
}

impl Client {
    pub fn connect(stream: UnixStream) -> Result<Client, Box<dyn Error>> {
        let conn = Connection::from_socket(stream)?;
        let (globals, queue) = registry_queue_init(&conn)?;
        let seat = globals.bind(&queue.handle(), 1..=1, ())?;
        Ok(Client {
            globals,
            queue,
            seat,
        })
    }

    pub fn keyboard(&self) -> Result<ZwpVirtualKeyboardV1, Box<dyn Error>> {
        let qh = self.queue.handle();
        let manager: ZwpVirtualKeyboardManagerV1 = self.globals.bind(&qh, 1..=1, ())?;
        let keyboard = manager.create_virtual_keyboard(&self.seat, &qh, ());
        keyboard.keymap(1, memfd(KEYMAP)?.as_fd(), KEYMAP.len() as u32);
        Ok(keyboard)
    }

    pub fn show(
        mut self,
        pixels: &[u32],
        touch: impl FnMut(wl_touch::Event) + 'static,
    ) -> Result<Infallible, Box<dyn Error>> {
        let qh = self.queue.handle();
        let compositor: WlCompositor = self.globals.bind(&qh, 4..=4, ())?;
        let shm: WlShm = self.globals.bind(&qh, 1..=1, ())?;
        let base: XdgWmBase = self.globals.bind(&qh, 1..=1, ())?;
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
        let pool = shm.create_pool(memfd(&bytes)?.as_fd(), bytes.len() as i32, &qh, ());
        let buffer = pool.create_buffer(0, W, H, W * 4, wl_shm::Format::Xrgb8888, &qh, ());
        pool.destroy();
        let surface = compositor.create_surface(&qh, ());
        base.get_xdg_surface(&surface, &qh, ())
            .get_toplevel(&qh, ());
        surface.commit();
        let mut window = Window {
            surface,
            buffer,
            touch: Box::new(touch),
        };
        loop {
            self.queue.blocking_dispatch(&mut window)?;
        }
    }
}

fn memfd(bytes: &[u8]) -> io::Result<File> {
    let fd = unsafe { libc::memfd_create(c"sliver".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    file.write_all(bytes)?;
    Ok(file)
}

impl Dispatch<XdgSurface, ()> for Window {
    fn event(
        window: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            window.surface.attach(Some(&window.buffer), 0, 0);
            window.surface.damage_buffer(0, 0, W, H);
            window.surface.commit();
        }
    }
}

impl Dispatch<WlSeat, ()> for Window {
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

impl Dispatch<WlTouch, ()> for Window {
    fn event(
        window: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        (window.touch)(event);
    }
}

impl Dispatch<XdgWmBase, ()> for Window {
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

impl Dispatch<WlRegistry, GlobalListContents> for Window {
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

delegate_noop!(Window: WlCompositor);
delegate_noop!(Window: ignore WlShm);
delegate_noop!(Window: WlShmPool);
delegate_noop!(Window: ignore WlBuffer);
delegate_noop!(Window: ignore WlSurface);
delegate_noop!(Window: ignore XdgToplevel);
delegate_noop!(Window: ZwpVirtualKeyboardManagerV1);
delegate_noop!(Window: ZwpVirtualKeyboardV1);
