use std::{
    fs::{self, File},
    io,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmSurface, PlaneConfig, PlaneState},
        input::{AbsolutePositionEvent, InputEvent, KeyState, KeyboardKeyEvent, TouchEvent},
        libinput::LibinputInputBackend,
    },
    reexports::{
        calloop::{EventLoop, LoopHandle},
        drm::{
            buffer::Buffer as _,
            control::{Device, connector, dumbbuffer::DumbBuffer, framebuffer},
        },
        input::{self, Libinput, LibinputInterface, event::DeviceEvent},
        udev,
    },
    utils::{DeviceFd, Logical, Rectangle, Transform},
};

use crate::{Bar, Keyboard, Sliver, TRANSFORM, Touch, fail, panel};

const SEAT: &str = "seat-touchbar";
const KEY_FN: u32 = 0x1d0;

const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const BUS_VIRTUAL: u16 = 6;

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct UinputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

fn uinput_keyboard() -> io::Result<Keyboard> {
    let file = File::options().write(true).open("/dev/uinput")?;
    let fd = file.as_raw_fd();
    let ioctl = |request, arg: libc::c_ulong| match unsafe { libc::ioctl(fd, request, arg) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    };
    ioctl(UI_SET_EVBIT, EV_KEY.into())?;
    for code in (1..0x100).chain(0x160..0x2c0) {
        ioctl(UI_SET_KEYBIT, code)?;
    }
    let mut setup = UinputSetup {
        bustype: BUS_VIRTUAL,
        vendor: 0,
        product: 0,
        version: 0,
        name: [0; 80],
        ff_effects_max: 0,
    };
    let name = b"Sliver Touch Bar";
    setup.name[..name.len()].copy_from_slice(name);
    ioctl(UI_DEV_SETUP, &setup as *const _ as libc::c_ulong)?;
    ioctl(UI_DEV_CREATE, 0)?;
    Ok(Box::new(move |code, pressed| {
        let event = |kind, code, value| UinputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            kind,
            code,
            value,
        };
        let events = [
            event(EV_KEY, code as u16, pressed.into()),
            event(EV_SYN, 0, 0),
        ];
        let bytes = unsafe {
            std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), size_of_val(&events))
        };
        io::Write::write_all(&mut &file, bytes).unwrap_or_else(|e| fail("uinput", e));
    }))
}

pub fn run() -> io::Result<()> {
    let mut lp = EventLoop::<Sliver>::try_new()?;
    let panel = Panel::open(&lp.handle())?;
    let mut sliver = Sliver::new(lp.handle(), Box::new(panel))?;
    sliver.listen(Path::new("/run/sliver"))?;
    libinput(&lp.handle(), SEAT, touch)?;
    libinput(&lp.handle(), "seat0", laptop)?;
    lp.run(None, &mut sliver, |s| {
        let _ = s.wl.dh.flush_clients();
    })
    .map_err(io::Error::other)
}

struct Panel {
    fd: DrmDeviceFd,
    _device: DrmDevice,
    surface: DrmSurface,
    buffers: [(DumbBuffer, framebuffer::Handle); 2],
    back: usize,
    started: bool,
    brightness: PathBuf,
    max: String,
}

impl Panel {
    fn open(lh: &LoopHandle<'static, Sliver>) -> io::Result<Panel> {
        let node = on_seat("drm")?
            .find_map(|d| d.devnode().map(Path::to_path_buf))
            .ok_or_else(|| io::Error::other(format!("no display on {SEAT}")))?;
        let backlight = on_seat("backlight")?
            .next()
            .map(|d| d.syspath().to_path_buf())
            .ok_or_else(|| io::Error::other(format!("no backlight on {SEAT}")))?;
        let file = File::options()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(&node)?;
        let fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
        let (mut device, notifier) = DrmDevice::new(fd.clone(), true).map_err(io::Error::other)?;
        let resources = fd.resource_handles()?;
        let connector = resources
            .connectors()
            .iter()
            .filter_map(|&c| fd.get_connector(c, false).ok())
            .find(|c| c.state() == connector::State::Connected)
            .ok_or_else(|| io::Error::other("no connected panel"))?;
        let mode = *connector
            .modes()
            .first()
            .ok_or_else(|| io::Error::other("panel has no mode"))?;
        let panel = panel::<Logical>();
        if mode.size() != (panel.w as u16, panel.h as u16) {
            return Err(io::Error::other(format!(
                "panel mode is {:?}, not {panel:?}",
                mode.size()
            )));
        }
        let crtc = connector
            .encoders()
            .iter()
            .filter_map(|&e| fd.get_encoder(e).ok())
            .flat_map(|e| resources.filter_crtcs(e.possible_crtcs()))
            .next()
            .ok_or_else(|| io::Error::other("no crtc for the panel"))?;
        let surface = device
            .create_surface(crtc, mode, &[connector.handle()])
            .map_err(io::Error::other)?;
        let buffer = || -> io::Result<_> {
            let buffer =
                fd.create_dumb_buffer((panel.w as u32, panel.h as u32), Fourcc::Xrgb8888, 32)?;
            let fb = fd.add_framebuffer(&buffer, 24, 32)?;
            Ok((buffer, fb))
        };
        let buffers = [buffer()?, buffer()?];
        lh.insert_source(notifier, |event, _, s| match event {
            DrmEvent::VBlank(_) => s.presented(),
            DrmEvent::Error(e) => fail("drm", e),
        })
        .map_err(|e| io::Error::other(e.error))?;
        let max = fs::read_to_string(backlight.join("max_brightness"))?;
        Ok(Panel {
            fd,
            _device: device,
            surface,
            buffers,
            back: 0,
            started: false,
            brightness: backlight.join("brightness"),
            max: max.trim().into(),
        })
    }

    fn present(&mut self, frame: &[u8]) -> io::Result<()> {
        let (buffer, fb) = &mut self.buffers[self.back];
        let (w, h) = buffer.size();
        let pitch = buffer.pitch() as usize;
        let mut map = self.fd.map_dumb_buffer(buffer)?;
        for (row, pixels) in map
            .chunks_exact_mut(pitch)
            .zip(frame.chunks_exact(w as usize * 4))
        {
            row[..pixels.len()].copy_from_slice(pixels);
        }
        let plane = PlaneState {
            handle: self.surface.plane(),
            config: Some(PlaneConfig {
                src: Rectangle::from_size((w as f64, h as f64).into()),
                dst: Rectangle::from_size((w as i32, h as i32).into()),
                transform: Transform::Normal,
                alpha: 1.0,
                damage_clips: None,
                fb: *fb,
                fence: None,
            }),
        };
        let flip = match self.started {
            true => self.surface.page_flip([plane], true),
            false => self.surface.commit([plane], true),
        };
        flip.map_err(io::Error::other)?;
        self.started = true;
        self.back ^= 1;
        Ok(())
    }
}

impl Bar for Panel {
    fn show(&mut self, frame: &[u8]) {
        self.present(frame).unwrap_or_else(|e| fail("panel", e));
    }

    fn power(&mut self, on: bool) {
        fs::write(&self.brightness, if on { &self.max } else { "0" })
            .unwrap_or_else(|e| fail("backlight", e));
    }

    fn keyboard(&mut self) -> Keyboard {
        uinput_keyboard().unwrap_or_else(|e| fail("uinput", e))
    }
}

fn on_seat(subsystem: &str) -> io::Result<impl Iterator<Item = udev::Device>> {
    let mut devices = udev::Enumerator::new()?;
    devices.match_subsystem(subsystem)?;
    devices.match_property("ID_SEAT", SEAT)?;
    Ok(devices.scan_devices()?.collect::<Vec<_>>().into_iter())
}

fn libinput(
    lh: &LoopHandle<'static, Sliver>,
    seat: &str,
    handle: fn(&mut Sliver, InputEvent<LibinputInputBackend>),
) -> io::Result<()> {
    let mut context = Libinput::new_with_udev(Open);
    context
        .udev_assign_seat(seat)
        .map_err(|()| io::Error::other(format!("libinput: no {seat}")))?;
    context.dispatch()?;
    if !context.any(|e| matches!(e, input::Event::Device(DeviceEvent::Added(_)))) {
        return Err(io::Error::other(format!("libinput: nothing on {seat}")));
    }
    lh.insert_source(LibinputInputBackend::new(context), move |event, _, s| {
        handle(s, event)
    })
    .map_err(|e| io::Error::other(e.error))?;
    Ok(())
}

fn touch(s: &mut Sliver, event: InputEvent<LibinputInputBackend>) {
    let panel = panel::<Logical>();
    let at = |e: &dyn AbsolutePositionEvent<LibinputInputBackend>| {
        TRANSFORM.transform_point_in(e.position_transformed(panel), &panel.to_f64())
    };
    let id = |e: &dyn TouchEvent<LibinputInputBackend>| i32::from(e.slot()) as u32;
    s.touch(match event {
        InputEvent::TouchDown { event } => Touch::Down(id(&event), at(&event)),
        InputEvent::TouchMotion { event } => Touch::Motion(id(&event), at(&event)),
        InputEvent::TouchUp { event } => Touch::Up(id(&event)),
        InputEvent::TouchFrame { .. } => Touch::Frame,
        InputEvent::TouchCancel { .. } => Touch::Cancel,
        InputEvent::DeviceRemoved { .. } => fail("touch", "the Touch Bar is gone"),
        _ => return,
    });
}

fn laptop(s: &mut Sliver, event: InputEvent<LibinputInputBackend>) {
    match event {
        InputEvent::Keyboard { event } if event.key_code().raw() == KEY_FN + 8 => {
            s.fn_key(event.state() == KeyState::Pressed)
        }
        InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {}
        _ => s.activity(),
    }
}

struct Open;

impl LibinputInterface for Open {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        File::options()
            .read(true)
            .write(flags & libc::O_ACCMODE != libc::O_RDONLY)
            .custom_flags(flags & !libc::O_ACCMODE)
            .open(path)
            .map(OwnedFd::from)
            .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
    }

    fn close_restricted(&mut self, _: OwnedFd) {}
}
