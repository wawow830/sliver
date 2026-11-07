use std::{fs::File, io, os::fd::AsRawFd};

use crate::Keyboard;

// From linux/uinput.h and linux/input.h.
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
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

/// A uinput keyboard; the kernel releases its held keys when the file closes.
pub fn uinput_keyboard() -> io::Result<Keyboard> {
    let file = File::options().write(true).open("/dev/uinput")?;
    let fd = file.as_raw_fd();
    let ioctl = |request, arg: libc::c_ulong| {
        // SAFETY: each request takes an int or a pointer to a live UinputSetup.
        match unsafe { libc::ioctl(fd, request, arg) } {
            -1 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        }
    };
    ioctl(UI_SET_EVBIT, EV_KEY.into())?;
    // Keys only: anything in the BTN_ ranges would make it look like a mouse or joystick.
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
        let event = |kind, code, value| InputEvent {
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
        // SAFETY: InputEvent is plain old data.
        let bytes = unsafe {
            std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), size_of_val(&events))
        };
        if let Err(e) = io::Write::write_all(&mut &file, bytes) {
            eprintln!("sliverd: uinput: {e}");
        }
    }))
}

pub fn run() -> io::Result<()> {
    todo!()
}
