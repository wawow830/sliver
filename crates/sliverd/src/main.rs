//! `sliverd`: the system daemon. Owns the Touch Bar panel, touch input, backlight and uinput keys,
//! and always keeps a built-in safe layer (Esc + F-keys) available.
//!
//! ```text
//! sliverd --headless OUT.png [--fn] [PANEL]
//!     Run the core once on fake hardware and write what the panel would show.
//! sliverd --fake-hardware [--socket PATH] [--uid N] [--frame-png PATH] [--config PATH] [PANEL]
//!     Serve the real IPC socket with fake hardware: develop and test the agent without a bar.
//!     The active uid defaults to your own. --frame-png rewrites a PNG on every panel flush.
//! PANEL: [--width 2008] [--model Mac14,7] [--max-brightness 255]
//! --backend real|fake (= --fake-hardware), --exit-after SECONDS.
//! Under the safe-swap harness (SLIVER_SAFESWAP=1) the socket defaults to $SLIVER_OUT_DIR/sliverd.sock
//! and sliverd exits 2 s before SLIVER_SAFESWAP_SECONDS.
//! ```
//! sliverd [--socket PATH] [--config PATH]
//!     Real hardware (needs SLIVER_SAFESWAP=1: only via the safe-swap harness for now).
//!
//! The IPC socket comes from systemd socket activation
//! (`FileDescriptorName=ipc`) or is bound at `--socket` (default `/run/sliver/sliverd.sock`).

use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sliver_ipc::proto::{PanelInfo, PixelFormat, SOCKET_PATH};
use sliver_ipc::transport::Listener;
use sliverd::fake::FakeHardware;
use sliverd::session::{FixedSession, LOGIND_SEATS, SeatWatch};
use sliverd::{Config, Daemon, Input, config, server::Server};

struct Args(Vec<String>);

impl Args {
    fn value(&mut self, flag: &str) -> Option<String> {
        let i = self.0.iter().position(|a| a == flag)?;
        self.0.remove(i);
        if i < self.0.len() {
            Some(self.0.remove(i))
        } else {
            die(&format!("{flag} needs a value"))
        }
    }
    fn flag(&mut self, flag: &str) -> bool {
        let found = self.0.iter().any(|a| a == flag);
        self.0.retain(|a| a != flag);
        found
    }
    fn parse<T: std::str::FromStr>(&mut self, flag: &str, default: T) -> T {
        self.value(flag).map_or(default, |v| {
            v.parse()
                .unwrap_or_else(|_| die(&format!("bad value for {flag}: {v}")))
        })
    }
}

fn die(msg: &str) -> ! {
    eprintln!("sliverd: {msg}");
    std::process::exit(2);
}

fn main() {
    let mut args = Args(std::env::args().skip(1).collect());
    if args.flag("--version") || args.flag("-V") {
        println!("sliverd {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let width = args.parse("--width", 2008u32);
    let max_brightness = args.parse("--max-brightness", 255u32);
    let model = args.value("--model").unwrap_or_else(|| "Mac14,7".into());
    let panel = PanelInfo {
        width,
        height: 60,
        format: PixelFormat::Xrgb8888,
        max_brightness,
        model,
    };
    let config_path = args
        .value("--config")
        .map_or_else(|| PathBuf::from(config::CONFIG_PATH), PathBuf::from);
    let (config, warning) = config::load(&config_path);
    if let Some(w) = warning {
        eprintln!("sliverd: {w}");
    }

    if let Some(out) = args.value("--headless") {
        return headless(panel, &config, &out, args.flag("--fn"));
    }
    let fake = match args.value("--backend").as_deref() {
        None | Some("real") => args.flag("--fake-hardware"),
        Some("fake") => {
            args.flag("--fake-hardware");
            true
        }
        Some(b) => die(&format!("unknown --backend {b} (real, fake)")),
    };
    let harness = std::env::var("SLIVER_SAFESWAP").is_ok_and(|v| v == "1");
    let out_dir = std::env::var_os("SLIVER_OUT_DIR").map(PathBuf::from);
    // Under the harness only $SLIVER_OUT_DIR is writable, and the run is cut off at
    // SLIVER_SAFESWAP_SECONDS: leave on our own a little before that.
    let secs = |v: String| -> u64 {
        v.parse()
            .unwrap_or_else(|_| die(&format!("bad seconds: {v}")))
    };
    let exit_after = match args.value("--exit-after") {
        Some(v) => Some(Duration::from_secs(secs(v))),
        None => harness
            .then(|| std::env::var("SLIVER_SAFESWAP_SECONDS").ok())
            .flatten()
            .map(|v| Duration::from_secs(secs(v).saturating_sub(2).max(1))),
    };
    let socket = args.value("--socket").or_else(|| {
        harness
            .then(|| {
                out_dir
                    .as_ref()
                    .map(|d| d.join("sliverd.sock").display().to_string())
            })
            .flatten()
    });
    let uid = args.parse("--uid", rustix::process::getuid().as_raw());
    let frame_png = args.value("--frame-png").map(PathBuf::from);
    if !args.0.is_empty() {
        die(&format!("unknown arguments: {:?}", args.0));
    }

    let listener = match (sliver_hw::listen_fd("ipc"), socket) {
        // SAFETY: systemd passed us this fd and nothing else owns it.
        (Some(fd), None) => Listener::from_fd(unsafe { OwnedFd::from_raw_fd(fd) }),
        (_, path) => {
            let path = path.unwrap_or_else(|| SOCKET_PATH.into());
            Listener::bind(&path).unwrap_or_else(|e| die(&format!("binding {path}: {e}")))
        }
    };
    let clock = Instant::now();
    let mut server = if fake {
        let hw = FakeHardware::new(panel.width, panel.height);
        hw.display.set_png_output(frame_png);
        let daemon = Daemon::new(hw.backends(), panel, &config, None, 0);
        let session = FixedSession::new(Some(uid)).unwrap_or_else(|e| die(&e.to_string()));
        eprintln!("sliverd: serving with fake hardware, active uid {uid}");
        Server::new(daemon, listener, Vec::new(), Box::new(session), clock)
    } else {
        // Real hardware: opening devices refuses unless SLIVER_SAFESWAP=1 (harness or unit).
        let hw = sliverd::real::open().unwrap_or_else(|e| die(&format!("hardware: {e}")));
        let panel = PanelInfo {
            width: hw.width,
            height: hw.height,
            max_brightness: hw.max_brightness,
            model: sliver_hw_model(),
            ..panel
        };
        eprintln!(
            "sliverd: {}x{} panel, model {}",
            panel.width, panel.height, panel.model
        );
        let daemon = Daemon::new(hw.backends, panel, &config, None, 0);
        let session = SeatWatch::new(LOGIND_SEATS, "seat0")
            .unwrap_or_else(|e| die(&format!("watching {LOGIND_SEATS}: {e}")));
        Server::new(
            daemon,
            listener,
            vec![Box::new(hw.input)],
            Box::new(session),
            clock,
        )
    }
    .unwrap_or_else(|e| die(&e.to_string()));
    let res = match exit_after {
        None => server.run(),
        Some(limit) => {
            let end = clock + limit;
            let mut res = Ok(());
            while res.is_ok() && Instant::now() < end {
                res = server.run_once(Some(end.saturating_duration_since(Instant::now())));
            }
            eprintln!("sliverd: --exit-after reached, exiting");
            res
        }
    };
    if let Err(e) = res {
        die(&format!("event loop: {e}"));
    }
}

fn headless(panel: PanelInfo, config: &Config, out: &str, fn_held: bool) {
    let hw = FakeHardware::new(panel.width, panel.height);
    let mut d = Daemon::new(hw.backends(), panel, config, None, 0);
    if fn_held {
        d.handle(0, Input::FnKey { held: true });
    }
    hw.display
        .frame()
        .save_png(out)
        .unwrap_or_else(|e| die(&format!("writing {out}: {e}")));
    eprintln!(
        "{out}: {:?}, backlight {:?}",
        d.shown_view(),
        hw.backlight.current()
    );
}

/// DMI product name (e.g. `Mac14,7`; device-tree model on Apple Silicon).
fn sliver_hw_model() -> String {
    [
        "/sys/class/dmi/id/product_name",
        "/proc/device-tree/compatible",
    ]
    .iter()
    .find_map(|p| std::fs::read(p).ok())
    .map(|b| {
        let s = String::from_utf8_lossy(&b);
        s.split('\0').next().unwrap_or("").trim().to_string()
    })
    .unwrap_or_default()
}
