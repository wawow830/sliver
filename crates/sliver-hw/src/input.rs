//! Real input: the bar digitizer (touch) + seat0 keyboards/pointers/lid (Fn, activity, lid),
//! read as raw evdev (never grabbed) behind ONE pollable fd (an epoll fd), so the daemon's loop
//! just adds [`HwInput::as_fd`] to its own epoll and calls [`HwInput::drain`] when it's readable.
//! Only open this under the safe-swap harness or the real sliverd unit ([`crate::require_harness`]).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::evdev::{self, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, EV_ABS, EV_KEY};
use crate::mt::{Contact, Decoder, Mapping};

pub const EV_REL: u16 = 0x02;
pub const EV_SW: u16 = 0x05;
pub const KEY_FN: u16 = 0x1d0;
pub const SW_LID: u16 = 0x00;
/// Name of the uinput device sliver creates. Never read our own keys back as activity.
pub const UINPUT_NAME: &str = "Sliver Touch Bar Keys";
/// Devices we must never treat as seat0 input: the bar itself, tiny-dfr's and our uinput devices.
const IGNORE_NAMES: &[&str] = &[
    " Touch Bar",
    "Dynamic Function Row Virtual Input Device",
    UINPUT_NAME,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HwEvent {
    Touch(Contact),
    /// `KEY_FN` pressed (true) / released (false) on a seat0 keyboard.
    Fn(bool),
    /// Other seat0 user input (keys, pointer). At most one per [`HwInput::drain`].
    Activity,
    Lid {
        closed: bool,
    },
}

/// A parsed sysfs capability bitmap (`device/capabilities/{ev,key,sw}`): hex words, most
/// significant first, each `long`-sized.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caps(Vec<u64>);

impl Caps {
    pub fn parse(s: &str) -> Caps {
        Caps(
            s.split_whitespace()
                .rev()
                .map(|w| u64::from_str_radix(w, 16).unwrap_or(0))
                .collect(),
        )
    }
    pub fn has(&self, bit: u16) -> bool {
        let (w, b) = (bit as usize / 64, bit % 64);
        self.0.get(w).is_some_and(|x| x >> b & 1 == 1)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seat0Device {
    pub node: PathBuf,
    pub name: String,
    pub fn_key: bool,
    pub lid: bool,
    pub activity: bool,
}

/// seat0 devices worth reading, from sysfs only. `sys_input` = `/sys/class/input`,
/// `udev_data` = `/run/udev/data` (used to skip devices udev put on another seat).
pub fn seat0_devices(sys_input: &Path, udev_data: &Path) -> Vec<Seat0Device> {
    let mut entries: Vec<PathBuf> = fs::read_dir(sys_input)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    let read = |p: PathBuf| fs::read_to_string(p).unwrap_or_default();
    entries
        .into_iter()
        .filter_map(|e| {
            let ev = e.file_name()?.to_str()?.to_owned();
            if !ev.starts_with("event") {
                return None;
            }
            let name = read(e.join("device/name")).trim().to_owned();
            if IGNORE_NAMES.iter().any(|n| name.contains(n)) {
                return None;
            }
            if let Some(dev) = read(e.join("dev")).trim().split_once(':') {
                let data = read(udev_data.join(format!("c{}:{}", dev.0, dev.1)));
                if data
                    .lines()
                    .any(|l| l.starts_with("E:ID_SEAT=") && l != "E:ID_SEAT=seat0")
                {
                    return None;
                }
            }
            let evs = Caps::parse(&read(e.join("device/capabilities/ev")));
            let keys = Caps::parse(&read(e.join("device/capabilities/key")));
            let sws = Caps::parse(&read(e.join("device/capabilities/sw")));
            let d = Seat0Device {
                node: PathBuf::from("/dev/input").join(&ev),
                fn_key: keys.has(KEY_FN),
                lid: evs.has(EV_SW) && sws.has(SW_LID),
                activity: evs.has(EV_KEY) || evs.has(EV_REL) || evs.has(EV_ABS),
                name,
            };
            (d.fn_key || d.lid || d.activity).then_some(d)
        })
        .collect()
}

enum Kind {
    Touch(Box<Decoder>),
    Seat0,
}

struct Dev {
    file: File,
    kind: Kind,
    name: String,
}

pub struct HwInput {
    epoll: OwnedFd,
    devs: Vec<Option<Dev>>,
    pending: Vec<HwEvent>,
}

fn ioctl_int(fd: RawFd, req: u64, v: libc::c_int) -> io::Result<()> {
    // SAFETY: _IOW(int) ioctls read one int from the pointer.
    let r = unsafe { libc::ioctl(fd, req as _, &v as *const libc::c_int) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `EVIOCSCLOCKID` = `_IOW('E', 0xa0, int)`.
const EVIOCSCLOCKID: u64 = (1 << 30) | (4 << 16) | ((b'E' as u64) << 8) | 0xa0;
/// `EVIOCGSW(len)` = `_IOC(_IOC_READ, 'E', 0x1b, len)`.
const fn eviocgsw(len: usize) -> u64 {
    (2 << 30) | ((len as u64) << 16) | ((b'E' as u64) << 8) | 0x1b
}

fn open_evdev(node: &Path) -> io::Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(node)?;
    // Monotonic timestamps, so touch times compare with the daemon's clock.
    let _ = ioctl_int(f.as_raw_fd(), EVIOCSCLOCKID, libc::CLOCK_MONOTONIC);
    Ok(f)
}

impl HwInput {
    /// Opens the bar digitizer at `touch_node` (mapped onto a `len` x `thickness` logical bar)
    /// and every relevant seat0 device. Refuses outside the harness.
    pub fn open(touch_node: &Path, len: u32, thickness: u32) -> io::Result<HwInput> {
        crate::require_harness().map_err(io::Error::other)?;
        // SAFETY: plain syscall; we own the returned fd.
        let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if ep < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: ep is a fresh fd we own.
        let mut me = HwInput {
            epoll: unsafe { OwnedFd::from_raw_fd(ep) },
            devs: vec![],
            pending: vec![],
        };

        let touch = open_evdev(touch_node)?;
        let ax = evdev::abs_info(touch.as_raw_fd(), ABS_MT_POSITION_X)?;
        let ay = evdev::abs_info(touch.as_raw_fd(), ABS_MT_POSITION_Y)?;
        let map = Mapping {
            x_min: ax.minimum,
            x_max: ax.maximum,
            y_min: ay.minimum,
            y_max: ay.maximum,
            len,
            thickness,
        };
        me.add(Dev {
            file: touch,
            kind: Kind::Touch(Box::new(Decoder::new(map))),
            name: touch_node.display().to_string(),
        })?;

        for d in seat0_devices(Path::new("/sys/class/input"), Path::new("/run/udev/data")) {
            let Ok(file) = open_evdev(&d.node) else {
                continue;
            };
            if d.lid {
                let mut sw = [0u8; 8];
                // SAFETY: EVIOCGSW writes at most `len` bytes into the buffer.
                let r = unsafe {
                    libc::ioctl(file.as_raw_fd(), eviocgsw(sw.len()) as _, sw.as_mut_ptr())
                };
                if r >= 0 {
                    me.pending.push(HwEvent::Lid {
                        closed: sw[0] & 1 == 1,
                    });
                }
            }
            me.add(Dev {
                file,
                kind: Kind::Seat0,
                name: d.name,
            })?;
        }
        Ok(me)
    }

    fn add(&mut self, dev: Dev) -> io::Result<()> {
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: self.devs.len() as u64,
        };
        // SAFETY: valid epoll fd, valid device fd, valid event struct.
        let r = unsafe {
            libc::epoll_ctl(
                self.epoll.as_raw_fd(),
                libc::EPOLL_CTL_ADD,
                dev.file.as_raw_fd(),
                &mut ev,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        self.devs.push(Some(dev));
        Ok(())
    }

    /// Names of the devices being read (for logs).
    pub fn device_names(&self) -> Vec<String> {
        self.devs.iter().flatten().map(|d| d.name.clone()).collect()
    }

    /// Everything that happened since the last call. Never blocks.
    pub fn drain(&mut self) -> Vec<HwEvent> {
        let mut out = std::mem::take(&mut self.pending);
        let mut ready = [libc::epoll_event { events: 0, u64: 0 }; 16];
        // SAFETY: valid epoll fd and buffer.
        let n = unsafe {
            libc::epoll_wait(
                self.epoll.as_raw_fd(),
                ready.as_mut_ptr(),
                ready.len() as i32,
                0,
            )
        };
        let mut activity = false;
        let mut contacts = Vec::new();
        for ev in ready.iter().take(n.max(0) as usize) {
            let i = ev.u64 as usize;
            let gone = match self.devs.get_mut(i).and_then(Option::as_mut) {
                Some(dev) => read_dev(dev, &mut out, &mut contacts, &mut activity),
                None => false,
            };
            if gone {
                if let Some(Some(Dev {
                    kind: Kind::Touch(dec),
                    ..
                })) = self.devs.get_mut(i)
                {
                    dec.cancel_all(0, &mut contacts);
                }
                if let Some(dev) = self.devs[i].take() {
                    // SAFETY: removing an fd we added.
                    unsafe {
                        libc::epoll_ctl(
                            self.epoll.as_raw_fd(),
                            libc::EPOLL_CTL_DEL,
                            dev.file.as_raw_fd(),
                            std::ptr::null_mut(),
                        )
                    };
                }
            }
        }
        out.extend(contacts.into_iter().map(HwEvent::Touch));
        if activity {
            out.push(HwEvent::Activity);
        }
        out
    }
}

/// Reads all queued events from one device. Returns true if the device went away.
fn read_dev(
    dev: &mut Dev,
    out: &mut Vec<HwEvent>,
    contacts: &mut Vec<Contact>,
    activity: &mut bool,
) -> bool {
    let mut buf = [0u8; 24 * 64];
    loop {
        match dev.file.read(&mut buf) {
            Ok(0) => return true,
            Ok(n) => {
                for chunk in buf[..n].chunks_exact(24) {
                    let (sec, usec, ty, code, value) =
                        evdev::decode_event(chunk.try_into().unwrap());
                    let time_us = (sec.max(0) as u64) * 1_000_000 + usec.max(0) as u64;
                    match &mut dev.kind {
                        Kind::Touch(dec) => dec.feed(ty, code, value, time_us, contacts),
                        Kind::Seat0 => seat0_event(ty, code, value, out, activity),
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return false,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return true, // ENODEV etc.
        }
    }
}

fn seat0_event(ty: u16, code: u16, value: i32, out: &mut Vec<HwEvent>, activity: &mut bool) {
    match (ty, code) {
        (EV_KEY, KEY_FN) if value != 2 => out.push(HwEvent::Fn(value == 1)),
        (EV_SW, SW_LID) => out.push(HwEvent::Lid { closed: value == 1 }),
        (EV_KEY, _) if value == 1 => *activity = true,
        (EV_REL, _) | (EV_ABS, _) => *activity = true,
        _ => {}
    }
}

impl AsFd for HwInput {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.epoll.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_parse_real_keyboard() {
        // "Apple MTP keyboard" on the Mac14,7 (docs/hardware.md): has KEY_FN (464) and KEY_ESC (1).
        let k = Caps::parse(
            "10000 0 0 0 101007b02011007 ff9f217ac14057ff ffbeffdfffefffff fffffffffffffffe",
        );
        assert!(k.has(KEY_FN));
        assert!(k.has(1));
        assert!(!k.has(0));
        assert!(!k.has(0x2ff));
        assert!(!Caps::parse("").has(1));
    }

    #[test]
    fn seat0_classification() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("input-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let mk = |ev: &str, name: &str, evc: &str, key: &str, sw: &str, dev: &str| {
            let c = d.join("class").join(ev);
            fs::create_dir_all(c.join("device/capabilities")).unwrap();
            fs::write(c.join("device/name"), format!("{name}\n")).unwrap();
            fs::write(c.join("device/capabilities/ev"), evc).unwrap();
            fs::write(c.join("device/capabilities/key"), key).unwrap();
            fs::write(c.join("device/capabilities/sw"), sw).unwrap();
            fs::write(c.join("dev"), dev).unwrap();
        };
        mk(
            "event1",
            "Apple MTP keyboard",
            "120013",
            "10000 0 0 0 0 0 0 fffffffffffffffe",
            "0",
            "13:65\n",
        );
        mk(
            "event2",
            "Mac14,7 Touch Bar",
            "b",
            "400 0 0 0 0 0",
            "0",
            "13:66\n",
        );
        mk(
            "event3",
            "Apple SMC power/lid events",
            "23",
            "10000000000000 0",
            "1",
            "13:67\n",
        );
        mk(
            "event5",
            "Dynamic Function Row Virtual Input Device",
            "3",
            "fffe",
            "0",
            "13:69\n",
        );
        mk(
            "event6",
            "Some seat1 mouse",
            "7",
            "1f0000 0 0 0 0",
            "0",
            "13:70\n",
        );
        mk("event7", "Headphone Jack", "21", "0", "4", "13:71\n");
        fs::create_dir_all(d.join("udev")).unwrap();
        fs::write(d.join("udev/c13:70"), "E:ID_SEAT=seat1\n").unwrap();
        let got = seat0_devices(&d.join("class"), &d.join("udev"));
        let names: Vec<_> = got
            .iter()
            .map(|x| (x.name.as_str(), x.fn_key, x.lid))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Apple MTP keyboard", true, false),
                // the jack only has a non-lid switch and no key/rel/abs, so it's skipped
                ("Apple SMC power/lid events", false, true),
            ]
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn seat0_events() {
        let mut out = vec![];
        let mut act = false;
        seat0_event(EV_KEY, KEY_FN, 1, &mut out, &mut act);
        seat0_event(EV_KEY, KEY_FN, 2, &mut out, &mut act); // autorepeat ignored
        seat0_event(EV_KEY, KEY_FN, 0, &mut out, &mut act);
        seat0_event(EV_SW, SW_LID, 1, &mut out, &mut act);
        assert_eq!(
            out,
            vec![
                HwEvent::Fn(true),
                HwEvent::Fn(false),
                HwEvent::Lid { closed: true }
            ]
        );
        assert!(!act);
        seat0_event(EV_KEY, 30, 1, &mut out, &mut act);
        assert!(act);
    }
}
