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
    /// The main display's backlight changed (a udev `change` event on it, e.g. the user's
    /// brightness keys). For adaptive bar brightness without polling. At most one per drain.
    ScreenBacklight,
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
    entries
        .iter()
        .filter_map(|e| classify(e, udev_data))
        .collect()
}

/// Classifies one `/sys/class/input/eventN` entry. `None` = not ours to read.
pub fn classify(e: &Path, udev_data: &Path) -> Option<Seat0Device> {
    let read = |p: PathBuf| fs::read_to_string(p).unwrap_or_default();
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
}

enum Kind {
    Touch(Box<Decoder>),
    Seat0,
}

struct Dev {
    file: File,
    kind: Kind,
    name: String,
    node: PathBuf,
    /// Raw evdev events read so far (diagnostics).
    events: u64,
}

/// Token for the udev monitor socket in the epoll set (device tokens are their index).
const MONITOR: u64 = u64::MAX;
/// udev's netlink multicast group (processed events: permissions and ID_SEAT already applied).
const UDEV_GROUP: u32 = 2;

pub struct HwInput {
    epoll: OwnedFd,
    devs: Vec<Option<Dev>>,
    pending: Vec<HwEvent>,
    monitor: Option<OwnedFd>,
    touch_node: PathBuf,
    size: (u32, u32),
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

/// Parses one message from udev's netlink monitor (libudev wire format: a `libudev\0` header with
/// magic 0xfeedcafe big-endian, then NUL-separated `KEY=VALUE` properties). Returns `None` for
/// anything else (e.g. raw kernel uevents).
pub fn parse_udev_msg(buf: &[u8]) -> Option<Vec<(String, String)>> {
    if buf.len() < 40 || &buf[..8] != b"libudev\0" {
        return None;
    }
    if u32::from_be_bytes(buf[8..12].try_into().ok()?) != 0xfeed_cafe {
        return None;
    }
    let off = u32::from_ne_bytes(buf[16..20].try_into().ok()?) as usize;
    let len = u32::from_ne_bytes(buf[20..24].try_into().ok()?) as usize;
    let props = buf.get(off..off.checked_add(len)?)?;
    Some(
        props
            .split(|&b| b == 0)
            .filter_map(|kv| {
                let s = std::str::from_utf8(kv).ok()?;
                let (k, v) = s.split_once('=')?;
                Some((k.to_owned(), v.to_owned()))
            })
            .collect(),
    )
}

/// What a udev message means for us: `(add?, /dev/input/eventN)` for input event nodes only.
pub fn input_change(props: &[(String, String)]) -> Option<(bool, PathBuf)> {
    let get = |k: &str| {
        props
            .iter()
            .find(|(pk, _)| pk == k)
            .map(|(_, v)| v.as_str())
    };
    if get("SUBSYSTEM")? != "input" {
        return None;
    }
    let node = get("DEVNAME")?;
    let n = node.strip_prefix("/dev/input/event")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match get("ACTION")? {
        "add" => Some((true, PathBuf::from(node))),
        "remove" => Some((false, PathBuf::from(node))),
        _ => None,
    }
}

/// Whether a udev message is a brightness change on the main display's backlight (names as in
/// [`crate::discover::DISPLAY_BACKLIGHT_NAMES`]). The kernel sends one for every sysfs brightness
/// write (`SOURCE=sysfs`, measured on the Mac14,7). The bar's own backlight never matches, so our
/// writes don't come back to us.
pub fn screen_backlight_change(props: &[(String, String)]) -> bool {
    let get = |k: &str| {
        props
            .iter()
            .find(|(pk, _)| pk == k)
            .map(|(_, v)| v.as_str())
    };
    get("SUBSYSTEM") == Some("backlight")
        && get("ACTION") == Some("change")
        && get("DEVPATH")
            .and_then(|p| p.rsplit('/').next())
            .is_some_and(|name| {
                crate::discover::DISPLAY_BACKLIGHT_NAMES
                    .iter()
                    .any(|n| name.contains(n))
            })
}

/// A non-blocking socket on udev's netlink monitor group. Opens no devices (safe anywhere).
pub fn udev_monitor() -> io::Result<OwnedFd> {
    // SAFETY: plain syscalls on a socket we own.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_KOBJECT_UEVENT,
        );
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut sa: libc::sockaddr_nl = std::mem::zeroed();
        sa.nl_family = libc::AF_NETLINK as u16;
        sa.nl_groups = UDEV_GROUP;
        let r = libc::bind(
            fd.as_raw_fd(),
            &sa as *const libc::sockaddr_nl as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        );
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }
}

impl HwInput {
    /// Opens the bar digitizer at `touch_node` (mapped onto a `len` x `thickness` logical bar),
    /// every relevant seat0 device, and a udev monitor for hot-plug. Refuses outside the harness.
    pub fn open(touch_node: &Path, len: u32, thickness: u32) -> io::Result<HwInput> {
        crate::require_harness().map_err(io::Error::other)?;
        // SAFETY: plain syscall; we own the returned fd.
        let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if ep < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut me = HwInput {
            // SAFETY: ep is a fresh fd we own.
            epoll: unsafe { OwnedFd::from_raw_fd(ep) },
            devs: vec![],
            pending: vec![],
            monitor: None,
            touch_node: touch_node.to_owned(),
            size: (len, thickness),
        };
        me.open_touch()?;
        for d in seat0_devices(Path::new("/sys/class/input"), Path::new("/run/udev/data")) {
            let _ = me.open_seat0(d);
        }
        // Hot-plug is best effort: without it we only miss devices added later.
        if let Ok(m) = udev_monitor() {
            if me.watch(m.as_raw_fd(), MONITOR).is_ok() {
                me.monitor = Some(m);
            }
        }
        Ok(me)
    }

    fn open_touch(&mut self) -> io::Result<()> {
        let touch = open_evdev(&self.touch_node)?;
        let ax = evdev::abs_info(touch.as_raw_fd(), ABS_MT_POSITION_X)?;
        let ay = evdev::abs_info(touch.as_raw_fd(), ABS_MT_POSITION_Y)?;
        let (len, thickness) = self.size;
        let map = Mapping {
            x_min: ax.minimum,
            x_max: ax.maximum,
            y_min: ay.minimum,
            y_max: ay.maximum,
            len,
            thickness,
        };
        let mut dec = Decoder::new(map);
        // Seed with the current MT state: fingers already down must not fire (mt::Decoder::sync).
        let fd = touch.as_raw_fd();
        let n = crate::mt::MAX_SLOTS;
        if let (Ok(slot), Ok(ids)) = (
            evdev::abs_info(fd, evdev::ABS_MT_SLOT),
            evdev::mt_slots(fd, evdev::ABS_MT_TRACKING_ID, n),
        ) {
            let xs = evdev::mt_slots(fd, ABS_MT_POSITION_X, n).unwrap_or_default();
            let ys = evdev::mt_slots(fd, ABS_MT_POSITION_Y, n).unwrap_or_default();
            dec.sync(slot.value, &ids, &xs, &ys);
        }
        self.add(Dev {
            file: touch,
            kind: Kind::Touch(Box::new(dec)),
            name: self.touch_node.display().to_string(),
            node: self.touch_node.clone(),
            events: 0,
        })
    }

    fn open_seat0(&mut self, d: Seat0Device) -> io::Result<()> {
        let file = open_evdev(&d.node)?;
        if d.lid {
            let mut sw = [0u8; 8];
            // SAFETY: EVIOCGSW writes at most `len` bytes into the buffer.
            let r =
                unsafe { libc::ioctl(file.as_raw_fd(), eviocgsw(sw.len()) as _, sw.as_mut_ptr()) };
            if r >= 0 {
                self.pending.push(HwEvent::Lid {
                    closed: sw[0] & 1 == 1,
                });
            }
        }
        self.add(Dev {
            file,
            kind: Kind::Seat0,
            name: d.name,
            node: d.node,
            events: 0,
        })
    }

    fn watch(&self, fd: RawFd, token: u64) -> io::Result<()> {
        let mut ev = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: token,
        };
        // SAFETY: valid epoll fd, valid fd, valid event struct.
        let r =
            unsafe { libc::epoll_ctl(self.epoll.as_raw_fd(), libc::EPOLL_CTL_ADD, fd, &mut ev) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn add(&mut self, dev: Dev) -> io::Result<()> {
        self.watch(dev.file.as_raw_fd(), self.devs.len() as u64)?;
        self.devs.push(Some(dev));
        Ok(())
    }

    fn is_open(&self, node: &Path) -> bool {
        self.devs.iter().flatten().any(|d| d.node == node)
    }

    /// Closes device `i`. A touch device's live contacts are cancelled.
    fn remove(&mut self, i: usize, contacts: &mut Vec<Contact>) {
        if let Some(mut dev) = self.devs.get_mut(i).and_then(Option::take) {
            if let Kind::Touch(dec) = &mut dev.kind {
                dec.cancel_all(0, contacts);
            }
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

    /// Whether hot-plug monitoring is active.
    pub fn hotplug(&self) -> bool {
        self.monitor.is_some()
    }

    /// `(device name, raw events read)` for every open device (diagnostics).
    pub fn stats(&self) -> Vec<(String, u64)> {
        self.devs
            .iter()
            .flatten()
            .map(|d| (d.name.clone(), d.events))
            .collect()
    }

    /// Names of the devices being read (for logs).
    pub fn device_names(&self) -> Vec<String> {
        self.devs.iter().flatten().map(|d| d.name.clone()).collect()
    }

    fn handle_monitor(&mut self, contacts: &mut Vec<Contact>) {
        let mut buf = [0u8; 8192];
        loop {
            let Some(m) = &self.monitor else { return };
            // SAFETY: reading into our buffer from our socket.
            let n = unsafe { libc::recv(m.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return; // EAGAIN (drained) or error
            }
            let Some(props) = parse_udev_msg(&buf[..n as usize]) else {
                continue;
            };
            if screen_backlight_change(&props) {
                if !self.pending.contains(&HwEvent::ScreenBacklight) {
                    self.pending.push(HwEvent::ScreenBacklight);
                }
                continue;
            }
            let Some((add, node)) = input_change(&props) else {
                continue;
            };
            if add {
                if self.is_open(&node) {
                    continue;
                }
                if node == self.touch_node {
                    let _ = self.open_touch();
                    continue;
                }
                let ev = node
                    .file_name()
                    .map(|f| Path::new("/sys/class/input").join(f));
                if let Some(d) = ev.and_then(|e| classify(&e, Path::new("/run/udev/data"))) {
                    let _ = self.open_seat0(d);
                }
            } else if let Some(i) = self
                .devs
                .iter()
                .position(|d| d.as_ref().is_some_and(|d| d.node == node))
            {
                self.remove(i, contacts);
            }
        }
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
            if ev.u64 == MONITOR {
                self.handle_monitor(&mut contacts);
                continue;
            }
            let i = ev.u64 as usize;
            let gone = match self.devs.get_mut(i).and_then(Option::as_mut) {
                Some(dev) => read_dev(dev, &mut out, &mut contacts, &mut activity),
                None => false,
            };
            if gone {
                self.remove(i, &mut contacts);
            }
        }
        out.extend(contacts.into_iter().map(HwEvent::Touch));
        if activity {
            out.push(HwEvent::Activity);
        }
        out.extend(std::mem::take(&mut self.pending)); // e.g. lid state of a hot-plugged switch
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
                dev.events += (n / 24) as u64;
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

    fn udev_msg(props: &[&str]) -> Vec<u8> {
        let body: Vec<u8> = props.iter().flat_map(|p| p.bytes().chain([0])).collect();
        let mut m = b"libudev\0".to_vec();
        m.extend(0xfeed_cafe_u32.to_be_bytes());
        m.extend(40u32.to_ne_bytes()); // header_size
        m.extend(40u32.to_ne_bytes()); // properties_off
        m.extend((body.len() as u32).to_ne_bytes());
        m.extend([0u8; 16]); // filter hashes
        m.extend(body);
        m
    }

    #[test]
    fn screen_backlight_changes() {
        let p = |props: &[&str]| parse_udev_msg(&udev_msg(props)).unwrap();
        // Exactly what the Mac14,7 sends for a brightness key press.
        let main = "DEVPATH=/devices/platform/soc/231c00000.dcp/backlight/apple-panel-bl";
        let bar =
            "DEVPATH=/devices/platform/soc/228600000.dsi/228600000.dsi.0/backlight/228600000.dsi.0";
        let change = ["ACTION=change", "SUBSYSTEM=backlight", "SOURCE=sysfs"];
        assert!(screen_backlight_change(
            &p(&[&change[..], &[main]].concat())
        ));
        assert!(
            !screen_backlight_change(&p(&[&change[..], &[bar]].concat())),
            "our own bar writes must not echo"
        );
        assert!(!screen_backlight_change(&p(&[
            "ACTION=add",
            "SUBSYSTEM=backlight",
            main
        ])));
        assert!(!screen_backlight_change(&p(&[
            "ACTION=change",
            "SUBSYSTEM=input",
            main
        ])));
        assert!(input_change(&p(&[&change[..], &[main]].concat())).is_none());
    }

    #[test]
    fn udev_monitor_messages() {
        let m = udev_msg(&[
            "ACTION=add",
            "SUBSYSTEM=input",
            "DEVNAME=/dev/input/event9",
            "ID_SEAT=seat0",
        ]);
        let p = parse_udev_msg(&m).unwrap();
        assert_eq!(
            input_change(&p),
            Some((true, PathBuf::from("/dev/input/event9")))
        );
        let m = udev_msg(&[
            "ACTION=remove",
            "SUBSYSTEM=input",
            "DEVNAME=/dev/input/event9",
        ]);
        assert_eq!(
            input_change(&parse_udev_msg(&m).unwrap()),
            Some((false, PathBuf::from("/dev/input/event9")))
        );
        // not event nodes / not input / malformed
        for props in [
            &["ACTION=add", "SUBSYSTEM=input", "DEVNAME=/dev/input/mouse0"][..],
            &["ACTION=add", "SUBSYSTEM=drm", "DEVNAME=/dev/dri/card1"][..],
            &[
                "ACTION=add",
                "SUBSYSTEM=input",
                "DEVNAME=/dev/input/event../../x",
            ][..],
            &[
                "ACTION=change",
                "SUBSYSTEM=input",
                "DEVNAME=/dev/input/event3",
            ][..],
        ] {
            assert_eq!(
                input_change(&parse_udev_msg(&udev_msg(props)).unwrap()),
                None
            );
        }
        assert!(parse_udev_msg(b"add@/devices/foo\0ACTION=add\0").is_none()); // raw kernel uevent
        let mut bad = udev_msg(&["ACTION=add"]);
        bad[20..24].copy_from_slice(&9999u32.to_ne_bytes()); // properties_len past the end
        assert!(parse_udev_msg(&bad).is_none());
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
