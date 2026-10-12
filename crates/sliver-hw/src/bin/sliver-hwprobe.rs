//! sliver-hwprobe: report what the Touch Bar hardware actually supports.
//!
//! Run it ONLY through the safe-swap harness (it refuses otherwise, except `--sysfs-only`):
//!   harness/safeswap/bar run target/debug/sliver-hwprobe --seconds 30 -- [--draw 5] [--touch 15] [--backlight N]
//!
//! Default: read-only. Opens the card (becomes DRM master automatically since tiny-dfr is
//! stopped) but never commits. Lists caps, connectors, CRTCs, planes and their properties,
//! plus the touch axis ranges. `--draw S` does a tiny-dfr-style atomic modeset and shows an
//! orientation test pattern for S seconds. `--touch S` logs raw touch events (no grab) for S
//! seconds from startup, in parallel with `--draw`.
//! `--backlight N` writes N to the harness-provided backlight fd.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd};
use std::path::Path;
use std::time::{Duration, Instant};

use drm::control::{
    AtomicCommitFlags, ClipRect, Device as ControlDevice, ResourceHandle, atomic, connector, crtc,
    dumbbuffer::DumbBuffer, framebuffer, plane, property,
};
use drm::{ClientCapability, Device, DriverCapability};
use sliver_hw::{discover, evdev, geom};

struct Card(File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Device for Card {}
impl ControlDevice for Card {}

#[derive(Default)]
struct Opts {
    sysfs_only: bool,
    draw: Option<u64>,
    touch: Option<u64>,
    backlight: Option<u32>,
    touchviz: Option<u64>,
    udev_listen: Option<u64>,
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut num = |what: &str| -> Result<u64, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{what} needs a number"))
        };
        match a.as_str() {
            "--sysfs-only" => o.sysfs_only = true,
            "--draw" => o.draw = Some(num("--draw")?.min(120)),
            "--touch" => o.touch = Some(num("--touch")?.min(120)),
            "--backlight" => o.backlight = Some(num("--backlight")? as u32),
            "--touchviz" => o.touchviz = Some(num("--touchviz")?.min(120)),
            "--udev-listen" => o.udev_listen = Some(num("--udev-listen")?.min(600)),
            "-h" | "--help" => {
                println!(
                    "usage: sliver-hwprobe [--sysfs-only] [--draw S] [--touch S] [--backlight N] [--touchviz S]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown arg {a}")),
        }
    }
    Ok(o)
}

fn main() {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("sliver-hwprobe: {e}");
            std::process::exit(2);
        }
    };
    println!("== sysfs");
    let card = discover::bar_card(Path::new("/sys/class/drm"));
    println!("card: {card:?}");
    if let Some(c) = &card {
        let name = c.file_name().unwrap().to_string_lossy();
        println!(
            "connectors: {:?}",
            discover::connector_modes(Path::new("/sys/class/drm"), &name)
        );
    }
    let bl = discover::bar_backlight(Path::new("/sys/class/backlight"));
    println!("bar backlight: {bl:?}");
    println!(
        "display backlight: {:?}",
        discover::display_backlight(Path::new("/sys/class/backlight"))
    );
    let touch = discover::touch_device(Path::new("/sys/class/input"));
    println!("touch: {touch:?}");
    if let Some(secs) = opts.udev_listen {
        // Opens no devices: just udev's netlink monitor. Checks parse_udev_msg on real traffic.
        udev_listen(secs);
        return;
    }
    if opts.sysfs_only {
        return;
    }
    if let Err(e) = sliver_hw::require_harness() {
        eprintln!("sliver-hwprobe: {e}");
        std::process::exit(3);
    }
    println!(
        "harness: seconds={:?} out={:?} backlight_fd={:?}",
        std::env::var("SLIVER_SAFESWAP_SECONDS").ok(),
        std::env::var("SLIVER_OUT_DIR").ok(),
        sliver_hw::listen_fd("backlight")
    );

    if let Some(secs) = opts.touchviz {
        // Exercises the real sliver-hw panel + input types together (what sliverd uses).
        let r = match (&card, &touch) {
            (Some(c), Some((t, _))) => touchviz(c, t, secs),
            _ => Err(std::io::Error::other("no card or touch device")),
        };
        if let Err(e) = r {
            println!("touchviz: ERROR {e}");
            std::process::exit(1);
        }
        println!("== done");
        return;
    }
    let mut failed = false;
    // Touch first, in a thread, so touches made while the test pattern is up get logged too.
    let touch_thread = touch.clone().map(|(dev, _)| {
        let secs = opts.touch;
        std::thread::spawn(move || probe_touch(&dev, secs))
    });
    std::thread::sleep(Duration::from_millis(200)); // let the touch header print first
    if let Some(c) = &card {
        if let Err(e) = probe_drm(c, opts.draw) {
            println!("drm: ERROR {e}");
            failed = true;
        }
    }
    if let Some(t) = touch_thread {
        match t.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                println!("touch: ERROR {e}");
                failed = true;
            }
            Err(_) => failed = true,
        }
    }
    if let Some(n) = opts.backlight {
        if let Err(e) = set_backlight(n, bl.as_ref().map(|b| b.max)) {
            println!("backlight: ERROR {e}");
            failed = true;
        }
    }
    println!("== done");
    std::process::exit(i32::from(failed));
}

fn prop_names<T: ResourceHandle>(card: &Card, h: T) -> std::io::Result<Vec<String>> {
    let props = card.get_properties(h)?;
    let (ids, vals) = props.as_props_and_values();
    let mut out = Vec::new();
    for (id, v) in ids.iter().zip(vals) {
        let info = card.get_property(*id)?;
        out.push(format!("{}={v}", info.name().to_string_lossy()));
    }
    Ok(out)
}

fn find_prop<T: ResourceHandle>(
    card: &Card,
    h: T,
    name: &str,
) -> std::io::Result<property::Handle> {
    let props = card.get_properties(h)?;
    for id in props.as_props_and_values().0 {
        if card.get_property(*id)?.name().to_bytes() == name.as_bytes() {
            return Ok(*id);
        }
    }
    Err(std::io::Error::other(format!("property {name} not found")))
}

fn probe_drm(path: &Path, draw: Option<u64>) -> std::io::Result<()> {
    println!("== drm {}", path.display());
    let card = Card(OpenOptions::new().read(true).write(true).open(path)?);
    let drv = card.get_driver()?;
    println!(
        "driver: {} ({})",
        drv.name().to_string_lossy(),
        drv.description().to_string_lossy()
    );
    for (name, cap) in [
        ("DUMB_BUFFER", DriverCapability::DumbBuffer),
        ("DUMB_PREFERRED_DEPTH", DriverCapability::DumbPreferredDepth),
        ("DUMB_PREFER_SHADOW", DriverCapability::DumbPreferShadow),
        ("PRIME", DriverCapability::Prime),
        ("ASYNC_PAGE_FLIP", DriverCapability::ASyncPageFlip),
        ("ADDFB2_MODIFIERS", DriverCapability::AddFB2Modifiers),
        ("CRTC_IN_VBLANK_EVENT", DriverCapability::CRTCInVBlankEvent),
    ] {
        println!(
            "cap {name}: {:?}",
            card.get_driver_capability(cap).map_err(|e| e.kind())
        );
    }
    println!(
        "client cap UNIVERSAL_PLANES: {:?}",
        card.set_client_capability(ClientCapability::UniversalPlanes, true)
            .map_err(|e| e.kind())
    );
    println!(
        "client cap ATOMIC: {:?}",
        card.set_client_capability(ClientCapability::Atomic, true)
            .map_err(|e| e.kind())
    );
    println!(
        "master: {:?}",
        card.acquire_master_lock().map_err(|e| e.kind())
    );

    let res = card.resource_handles()?;
    let mut target = None;
    for &c in res.connectors() {
        let info = card.get_connector(c, false)?;
        let modes: Vec<String> = info
            .modes()
            .iter()
            .map(|m| format!("{}x{}@{}", m.size().0, m.size().1, m.vrefresh()))
            .collect();
        println!(
            "connector {:?} {:?} state={:?} size_mm={:?} modes={modes:?} props={:?}",
            c,
            info.interface(),
            info.state(),
            info.size(),
            prop_names(&card, c)?
        );
        if info.state() == connector::State::Connected && target.is_none() {
            target = info.modes().first().map(|m| (c, *m));
        }
    }
    for &c in res.crtcs() {
        let info = card.get_crtc(c)?;
        println!(
            "crtc {:?} fb={:?} mode={:?} props={:?}",
            c,
            info.framebuffer(),
            info.mode().map(|m| m.size()),
            prop_names(&card, c)?
        );
    }
    let planes = card.plane_handles()?;
    for &p in &planes {
        let info = card.get_plane(p)?;
        let fmts: Vec<String> = info
            .formats()
            .iter()
            .map(|f| String::from_utf8_lossy(&f.to_le_bytes()).into_owned())
            .collect();
        println!(
            "plane {:?} crtc={:?} fb={:?} formats={fmts:?} props={:?}",
            p,
            info.crtc(),
            info.framebuffer(),
            prop_names(&card, p)?
        );
    }

    if let (Some(secs), Some((conn, mode))) = (draw, target) {
        let crtc = *res
            .crtcs()
            .first()
            .ok_or_else(|| std::io::Error::other("no crtc"))?;
        let plane = *planes
            .first()
            .ok_or_else(|| std::io::Error::other("no plane"))?;
        draw_pattern(&card, conn, crtc, plane, mode, secs)?;
    }
    Ok(())
}

fn draw_pattern(
    card: &Card,
    conn: connector::Handle,
    crtc: crtc::Handle,
    plane: plane::Handle,
    mode: drm::control::Mode,
    secs: u64,
) -> std::io::Result<()> {
    let (w, h) = mode.size(); // portrait: 60 x 2008
    println!("== draw: test pattern for {secs}s on {w}x{h}");
    let mut db: DumbBuffer =
        card.create_dumb_buffer((w.into(), h.into()), drm::buffer::DrmFourcc::Xrgb8888, 32)?;
    let fb: framebuffer::Handle = card.add_framebuffer(&db, 24, 32)?;
    let result = (|| -> std::io::Result<()> {
        let pitch = drm::buffer::Buffer::pitch(&db) as usize;
        {
            let mut map = card.map_dumb_buffer(&mut db)?;
            let len = u32::from(h);
            let vis_w = u32::from(w);
            geom::fill_portrait(map.as_mut(), pitch, vis_w, len, |lx, ly| {
                geom::test_pattern(lx, ly, len, vis_w)
            });
        }
        let blob = card.create_property_blob(&mode)?;
        let mut req = atomic::AtomicModeReq::new();
        req.add_property(
            conn,
            find_prop(card, conn, "CRTC_ID")?,
            property::Value::CRTC(Some(crtc)),
        );
        req.add_property(crtc, find_prop(card, crtc, "MODE_ID")?, blob);
        req.add_property(
            crtc,
            find_prop(card, crtc, "ACTIVE")?,
            property::Value::Boolean(true),
        );
        req.add_property(
            plane,
            find_prop(card, plane, "FB_ID")?,
            property::Value::Framebuffer(Some(fb)),
        );
        req.add_property(
            plane,
            find_prop(card, plane, "CRTC_ID")?,
            property::Value::CRTC(Some(crtc)),
        );
        for (n, v) in [
            ("SRC_X", 0u64),
            ("SRC_Y", 0),
            ("SRC_W", u64::from(w) << 16),
            ("SRC_H", u64::from(h) << 16),
        ] {
            req.add_property(
                plane,
                find_prop(card, plane, n)?,
                property::Value::UnsignedRange(v),
            );
        }
        req.add_property(
            plane,
            find_prop(card, plane, "CRTC_X")?,
            property::Value::SignedRange(0),
        );
        req.add_property(
            plane,
            find_prop(card, plane, "CRTC_Y")?,
            property::Value::SignedRange(0),
        );
        req.add_property(
            plane,
            find_prop(card, plane, "CRTC_W")?,
            property::Value::UnsignedRange(w.into()),
        );
        req.add_property(
            plane,
            find_prop(card, plane, "CRTC_H")?,
            property::Value::UnsignedRange(h.into()),
        );
        let t = Instant::now();
        card.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, req)?;
        println!("commit: ok in {:?}", t.elapsed());
        let t = Instant::now();
        println!(
            "dirtyfb: {:?} in {:?}",
            card.dirty_framebuffer(fb, &[ClipRect::new(0, 0, w, h)])
                .map_err(|e| e.kind()),
            t.elapsed()
        );
        std::thread::sleep(Duration::from_secs(secs));
        Ok(())
    })();
    let _ = card.destroy_framebuffer(fb);
    let _ = card.destroy_dumb_buffer(db);
    result
}

fn probe_touch(dev: &Path, secs: Option<u64>) -> std::io::Result<()> {
    println!("== touch {}", dev.display());
    let mut f = OpenOptions::new().read(true).open(dev)?; // no EVIOCGRAB, ever
    for &(abs, name) in evdev::PROBE_AXES {
        match evdev::abs_info(f.as_raw_fd(), abs) {
            Ok(i) => println!(
                "{name}: min={} max={} res={} fuzz={} flat={}",
                i.minimum, i.maximum, i.resolution, i.fuzz, i.flat
            ),
            Err(e) => println!("{name}: {:?}", e.kind()),
        }
    }
    let Some(secs) = secs else { return Ok(()) };
    println!("== touch events for {secs}s from now (touch the bar), concurrent with --draw");
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut buf = [0u8; 24];
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        let mut pfd = libc::pollfd {
            fd: f.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut pfd, 1, left) } <= 0 {
            continue;
        }
        f.read_exact(&mut buf)?;
        let (s, us, ty, code, v) = evdev::decode_event(&buf);
        if ty != evdev::EV_SYN || code == 0 {
            println!("{s}.{us:06} type={ty} code={code:#x} value={v}");
        }
    }
    Ok(())
}

fn set_backlight(n: u32, max: Option<u32>) -> std::io::Result<()> {
    let fd = sliver_hw::listen_fd("backlight")
        .ok_or_else(|| std::io::Error::other("no 'backlight' fd from the harness"))?;
    let n = max.map_or(n, |m| n.min(m));
    // SAFETY: systemd passed us this fd and nothing else owns it in this process.
    let mut f = unsafe { File::from_raw_fd(fd) };
    f.write_all(format!("{n}\n").as_bytes())?;
    println!("backlight: wrote {n}");
    Ok(())
}

/// Draws a white band under every finger (and a gray ruler every 100 px) using
/// `sliver_hw::panel::DrmPanel` + `sliver_hw::input::HwInput`, and logs Fn/lid/activity.
fn touchviz(card: &Path, touch: &Path, secs: u64) -> std::io::Result<()> {
    use sliver_hw::input::{HwEvent, HwInput};
    use sliver_hw::mt::Phase;
    use sliver_hw::panel::DrmPanel;
    let mut panel = DrmPanel::open(card)?;
    let (len, thick) = panel.logical_size();
    let mut input = HwInput::open(touch, len, thick)?;
    println!(
        "== touchviz {secs}s on {len}x{thick}, devices: {:?}",
        input.device_names()
    );
    let mut fingers: std::collections::HashMap<u8, f32> = std::collections::HashMap::new();
    let draw = |panel: &mut DrmPanel,
                fingers: &std::collections::HashMap<u8, f32>|
     -> std::io::Result<()> {
        let (len, thick) = panel.logical_size();
        let rotated = panel.rotated();
        panel.with_buffer(|buf, stride| {
            for lx in 0..len {
                let lit = fingers.values().any(|&x| (lx as f32 - x).abs() < 12.0);
                for ly in 0..thick {
                    let c: u32 = if lit {
                        0xffffff
                    } else if lx % 100 == 0 && ly > thick / 3 {
                        0x404040
                    } else {
                        0
                    };
                    let (x, y) = if rotated {
                        geom::landscape_to_fb(lx, ly, thick)
                    } else {
                        (lx, ly)
                    };
                    let o = y as usize * stride + x as usize * 4;
                    buf[o..o + 4].copy_from_slice(&c.to_le_bytes());
                }
            }
        })?;
        panel.dirty(&[(0, len)]);
        Ok(())
    };
    draw(&mut panel, &fingers)?;
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        let mut pfd = libc::pollfd {
            fd: input.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut pfd, 1, left) } <= 0 {
            continue;
        }
        let mut changed = false;
        for ev in input.drain() {
            match ev {
                HwEvent::Touch(c) => {
                    println!(
                        "touch slot={} {:?} x={:.1} y={:.1}",
                        c.slot, c.phase, c.x, c.y
                    );
                    match c.phase {
                        Phase::Down | Phase::Move => {
                            fingers.insert(c.slot, c.x);
                        }
                        Phase::Up | Phase::Cancel => {
                            fingers.remove(&c.slot);
                        }
                    }
                    changed = true;
                }
                other => println!("{other:?}"),
            }
        }
        if changed {
            let t = Instant::now();
            draw(&mut panel, &fingers)?;
            println!("frame in {:?}", t.elapsed());
        }
    }
    Ok(())
}

fn udev_listen(secs: u64) {
    let m = match sliver_hw::input::udev_monitor() {
        Ok(m) => m,
        Err(e) => {
            println!("udev monitor: ERROR {e}");
            std::process::exit(1);
        }
    };
    println!("== udev monitor for {secs}s");
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut buf = [0u8; 8192];
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        let mut pfd = libc::pollfd {
            fd: m.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut pfd, 1, left) } <= 0 {
            continue;
        }
        // SAFETY: reading into our buffer from our socket.
        let n = unsafe { libc::recv(m.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n <= 0 {
            continue;
        }
        match sliver_hw::input::parse_udev_msg(&buf[..n as usize]) {
            Some(p) => {
                let get = |k: &str| {
                    p.iter()
                        .find(|(a, _)| a == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default()
                };
                println!(
                    "udev {} {} {} input_change={:?} ({} props)",
                    get("ACTION"),
                    get("SUBSYSTEM"),
                    get("DEVPATH"),
                    sliver_hw::input::input_change(&p),
                    p.len()
                );
            }
            None => println!("unparsed message, {n} bytes"),
        }
    }
}
