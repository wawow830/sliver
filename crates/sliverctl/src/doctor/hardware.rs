//! The bar's hardware (T140): what sysfs shows without root, whether a safe-swap test run holds
//! the bar, and what sliverd itself found and opened (its read-only `report`, protocol 1.1).

use std::path::Path;
use std::time::Duration;

use sliver_ipc::proto::HwReport;
use sliver_ipc::report::Reply;

use super::{Check, Level, Sys, check};

/// What sysfs shows about the bar. Nothing is opened, so it works as any user and while sliverd
/// is down (which is when it matters most).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BarSysfs {
    /// The bar's DRM device, e.g. `/dev/dri/card1` (driver adp or appletbdrm).
    pub card: Option<String>,
    /// The bar backlight's sysfs dir.
    pub backlight: Option<String>,
    /// Its `brightness` is group `video` and group-writable (sliverd's udev rule). `None`: unknown.
    pub backlight_video_writable: Option<bool>,
    /// The digitizer, e.g. `/dev/input/event2`, and its name.
    pub touch: Option<(String, String)>,
    /// Intel T2 only: the bar's USB configuration (2 = display mode, 1 = plain keyboard).
    pub t2_usb_config: Option<u32>,
}

/// sliverd's answer to `report`: `Err` if it couldn't be asked (not running, hung).
pub type ReportOutcome = Result<Reply, String>;

pub fn checks(sys: &dyn Sys) -> Vec<Check> {
    let mut out = Vec::new();
    let bar = sys.bar_sysfs();
    match &bar.card {
        None => out.push(check(
            "bar",
            Level::Fail,
            "no Touch Bar display found (DRM driver adp or appletbdrm)",
            Some("Apple Silicon: an Asahi kernel with the adp driver; Intel T2: appletbdrm (a t2linux kernel, or Linux 6.15+)"),
        )),
        Some(card) => {
            let alias = sys.unit("dev-sliver_display.device", false);
            if alias.installed() && alias.active != "active" {
                out.push(check(
                    "bar",
                    Level::Warn,
                    format!("the bar display is {card}, but systemd doesn't see it as /dev/sliver_display, so sliverd.service can't start"),
                    Some("sudo udevadm trigger --action=change --subsystem-match=drm (sliverd's udev rule sets the alias), or reboot"),
                ));
            } else {
                out.push(check("bar", Level::Ok, format!("the bar display is {card}"), None));
            }
        }
    }
    match &bar.touch {
        None => out.push(check(
            "bar-touch",
            Level::Fail,
            "no Touch Bar digitizer found",
            Some("Apple Silicon: the kernel's Touch Bar input driver; Intel T2: hid-appletb-kbd and hid-multitouch"),
        )),
        Some((dev, name)) => out.push(check(
            "bar-touch",
            Level::Ok,
            format!("the digitizer is {dev} ({name})"),
            None,
        )),
    }
    match (&bar.backlight, bar.backlight_video_writable) {
        (None, _) => out.push(check(
            "bar-backlight",
            Level::Fail,
            "no Touch Bar backlight found: nothing can dim or turn off the bar",
            Some("Intel T2: the hid-appletb-bl module; Apple Silicon: the panel driver (panel-summit)"),
        )),
        (Some(bl), Some(false)) => out.push(check(
            "bar-backlight",
            Level::Fail,
            format!("{bl}/brightness isn't writable by group video, so sliverd can't dim the bar (it falls back to tiny-dfr)"),
            Some("sudo udevadm trigger --action=change --subsystem-match=backlight (sliverd's udev rule fixes the permissions)"),
        )),
        (Some(bl), _) => out.push(check("bar-backlight", Level::Ok, format!("the bar backlight is {bl}"), None)),
    }
    if bar.t2_usb_config == Some(1) {
        out.push(check(
            "bar-usb",
            Level::Fail,
            "the T2 Touch Bar is in keyboard mode (USB configuration 1), so it has no display",
            Some("reboot, or replug it: sliverd's udev rule switches it to configuration 2 when it appears"),
        ));
    }
    if sys.unit("sliver-swap.service", false).active == "active" {
        out.push(check(
            "bar-owner",
            Level::Warn,
            "a safe-swap test run is driving the bar right now; the previous owner comes back when it ends (5 min at most)",
            Some("wait, or: harness/safeswap/bar stop --any"),
        ));
    }
    match sys.hw_report() {
        Ok(Reply::Report(r)) => out.extend(from_report(&r)),
        Ok(Reply::TooOld(_)) => out.push(check(
            "sliverd-hw",
            Level::Warn,
            "sliverd is older than the hardware report (protocol 1.1), so what it opened can't be shown",
            Some("upgrade the sliverd package"),
        )),
        // Not running: the sliverd check already says so. Running but silent is worth a line.
        Err(e) if sys.unit("sliverd.service", false).active == "active" => out.push(check(
            "sliverd-hw",
            Level::Warn,
            format!("sliverd is running but didn't answer on its socket: {e}"),
            Some("journalctl -u sliverd -b"),
        )),
        Err(_) => {}
    }
    out
}

fn from_report(r: &HwReport) -> Vec<Check> {
    let mut out = Vec::new();
    let mut s = format!(
        "sliverd {} drives a {} bar ({}×{}, {}), allowed as: {}",
        r.version, r.model, r.width, r.height, r.card, r.gate
    );
    let display = r
        .display
        .map(|d| format!("{d:?}").to_lowercase())
        .unwrap_or_else(|| "?".into());
    s.push_str(&format!(
        "\n        showing {}, display {display}{}{}, up {}",
        r.view,
        r.brightness
            .map(|b| format!(" at {b}/{}", r.max_brightness))
            .unwrap_or_default(),
        if r.pixel_shift {
            ", pixel shift on"
        } else {
            ""
        },
        uptime(r.uptime_ms)
    ));
    if !r.inputs.is_empty() {
        s.push_str(&format!("\n        inputs: {}", r.inputs.join(", ")));
    }
    let (level, fix) = if r.backend != "real" {
        (
            Level::Warn,
            Some(
                "this sliverd runs on fake hardware (development mode); the installed one is sliverd.service",
            ),
        )
    } else {
        (Level::Ok, None)
    };
    out.push(check("sliverd-hw", level, s, fix));
    if r.backend == "real" && !r.backlight_writable {
        out.push(check(
            "sliverd-backlight",
            Level::Fail,
            "sliverd runs without a writable backlight: the bar never dims or turns off (OLED burn-in)",
            Some("sudo udevadm trigger --action=change --subsystem-match=backlight, then sudo systemctl restart sliverd"),
        ));
    }
    if r.backend == "real" && !r.hotplug {
        out.push(check(
            "sliverd-hotplug",
            Level::Warn,
            "sliverd can't watch for input devices coming back (udev netlink), so a digitizer that reappears isn't reopened",
            Some("restart sliverd: sudo systemctl restart sliverd"),
        ));
    }
    if r.hatch {
        out.push(check(
            "sliverd-hatch",
            Level::Ok,
            "the safe hatch is showing the built-in F-keys (Fn was pressed twice and held)",
            None,
        ));
    }
    out
}

fn uptime(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s / 60 % 60),
    }
}

// ---- the real machine -----------------------------------------------------------------------

pub fn real_bar_sysfs() -> BarSysfs {
    use sliver_hw::discover;
    let backlight = discover::bar_backlight(Path::new("/sys/class/backlight"));
    BarSysfs {
        card: discover::bar_card(Path::new("/sys/class/drm")).map(|p| p.display().to_string()),
        backlight_video_writable: backlight
            .as_ref()
            .and_then(|b| video_writable(&b.brightness_path())),
        backlight: backlight.map(|b| b.sysfs.display().to_string()),
        touch: discover::touch_device(Path::new("/sys/class/input"))
            .map(|(p, n)| (p.display().to_string(), n)),
        t2_usb_config: t2_usb_config(),
    }
}

/// Group `video` and `g+w`, what sliverd's udev rule sets (it runs as a DynamicUser in `video`).
fn video_writable(path: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    let name = c"video";
    // SAFETY: getgrnam with a valid C string; we only read gr_gid from the static result.
    let gid = unsafe {
        let g = libc::getgrnam(name.as_ptr());
        if g.is_null() {
            return None;
        }
        (*g).gr_gid
    };
    Some(m.gid() == gid && m.mode() & 0o020 != 0)
}

fn t2_usb_config() -> Option<u32> {
    std::fs::read_dir("/sys/bus/usb/devices")
        .ok()?
        .flatten()
        .find_map(|e| {
            let p = e.path();
            let read = |f: &str| {
                std::fs::read_to_string(p.join(f))
                    .ok()
                    .map(|s| s.trim().to_owned())
            };
            (read("idVendor")? == "05ac" && read("idProduct")? == "8302")
                .then(|| read("bConfigurationValue")?.parse().ok())
                .flatten()
        })
}

pub fn real_report() -> ReportOutcome {
    sliver_ipc::report::ask(
        sliver_ipc::proto::SOCKET_PATH,
        &format!("sliverctl {}", env!("CARGO_PKG_VERSION")),
        Duration::from_secs(2),
    )
    .map_err(|e| e.to_string())
}
