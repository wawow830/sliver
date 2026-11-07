use std::{
    io,
    os::unix::net::UnixStream,
    sync::{Arc, Mutex},
};

use smithay::{
    backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    delegate_compositor, delegate_output, delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::Window,
    input::{Seat, SeatHandler, SeatState},
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_protocols_misc::zwp_virtual_keyboard_v1::server::{
            zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
            zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
        },
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState, get_parent},
        output::OutputHandler,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};

use crate::{H, HZ, Keyboard, Sliver, TRANSFORM, W, panel};

pub struct Wl {
    pub dh: DisplayHandle,
    compositor: CompositorState,
    xdg: XdgShellState,
    shm: ShmState,
    seats: SeatState<Sliver>,
    pub seat: Seat<Sliver>,
    pub output: Output,
    pub windows: Vec<Window>,
}

impl Wl {
    pub fn new(dh: DisplayHandle) -> Wl {
        let mut seats = SeatState::new();
        let mut seat = seats.new_wl_seat(&dh, "seat0");
        seat.add_touch();
        let output = Output::new(
            "DSI-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Apple".into(),
                model: "Touch Bar".into(),
            },
        );
        let mode = Mode {
            size: panel(),
            refresh: HZ * 1000,
        };
        output.change_current_state(
            Some(mode),
            Some(TRANSFORM),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        output.create_global::<Sliver>(&dh);
        dh.create_global::<Sliver, ZwpVirtualKeyboardManagerV1, ()>(1, ());
        Wl {
            compositor: CompositorState::new::<Sliver>(&dh),
            xdg: XdgShellState::new::<Sliver>(&dh),
            shm: ShmState::new::<Sliver>(&dh, vec![]),
            seats,
            seat,
            output,
            windows: Vec::new(),
            dh,
        }
    }

    fn root(&self, surface: &WlSurface) -> WlSurface {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        root
    }

    pub fn connect(&mut self, stream: UnixStream) -> io::Result<Client> {
        self.dh
            .insert_client(stream, Arc::new(ClientState::default()))
    }

    fn toplevel(&self, surface: &WlSurface) -> Option<ToplevelSurface> {
        self.xdg
            .toplevel_surfaces()
            .iter()
            .find(|t| t.wl_surface() == surface)
            .cloned()
    }
}

#[derive(Default)]
struct ClientState(CompositorClientState);

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

impl CompositorHandler for Sliver {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.wl.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().0
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        let root = self.wl.root(surface);
        if let Some(toplevel) = self.wl.toplevel(&root) {
            if !toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
            let mapped =
                with_renderer_surface_state(&root, |s| s.buffer().is_some()).unwrap_or(false);
            let windows = &mut self.wl.windows;
            match windows.iter().position(|w| w.toplevel() == Some(&toplevel)) {
                Some(i) if !mapped => drop(windows.remove(i)),
                Some(i) => windows[i].on_commit(),
                None if mapped => {
                    let window = Window::new_wayland_window(toplevel);
                    window.on_commit();
                    windows.push(window);
                }
                None => {}
            }
            self.update();
        }
    }
}

impl XdgShellHandler for Sliver {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.wl.xdg
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Fullscreen);
            s.states.set(xdg_toplevel::State::Activated);
            s.size = Some((W, H).into());
        });
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.wl.windows.retain(|w| w.toplevel() != Some(&surface));
        self.update();
    }

    fn new_popup(&mut self, _: PopupSurface, _: PositionerState) {}

    fn reposition_request(&mut self, _: PopupSurface, _: PositionerState, _: u32) {}

    fn grab(&mut self, _: PopupSurface, _: WlSeat, _: Serial) {}
}

impl SeatHandler for Sliver {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.wl.seats
    }
}

impl ShmHandler for Sliver {
    fn shm_state(&self) -> &ShmState {
        &self.wl.shm
    }
}

impl BufferHandler for Sliver {
    fn buffer_destroyed(&mut self, _: &WlBuffer) {}
}

impl OutputHandler for Sliver {}

delegate_compositor!(Sliver);
delegate_xdg_shell!(Sliver);
delegate_seat!(Sliver);
delegate_shm!(Sliver);
delegate_output!(Sliver);

type Device = Mutex<Keyboard>;

impl GlobalDispatch<ZwpVirtualKeyboardManagerV1, ()> for Sliver {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        manager: New<ZwpVirtualKeyboardManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(manager, ());
    }
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for Sliver {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwpVirtualKeyboardManagerV1,
        request: zwp_virtual_keyboard_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { id, .. } = request
        {
            init.init(id, Mutex::new(state.bar.keyboard()));
        }
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, Device> for Sliver {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        device: &Device,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let zwp_virtual_keyboard_v1::Request::Key { key, state, .. } = request {
            device.lock().unwrap()(key, state == 1);
        }
    }
}
