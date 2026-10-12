//! `sliverd`: the system daemon. Owns the Touch Bar panel, touch input, backlight and uinput keys,
//! and always keeps a built-in safe layer (Esc + F-keys) available.
//!
//! ```text
//! sliverd --headless OUT.png [--fn] [--width 2008] [--model Mac14,7] [--max-brightness 255]
//! ```
//! `--headless` runs the daemon core against fake hardware and writes what the panel would show.

use sliver_ipc::proto::{PanelInfo, PixelFormat};
use sliverd::fake::FakeHardware;
use sliverd::{Config, Daemon, Input};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("sliverd {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let mut take = |flag: &str| -> Option<String> {
        let i = args.iter().position(|a| a == flag)?;
        args.remove(i);
        (i < args.len()).then(|| args.remove(i))
    };
    let Some(out) = take("--headless") else {
        eprintln!(
            "sliverd {}: real hardware backends are not implemented yet; try --headless OUT.png",
            env!("CARGO_PKG_VERSION")
        );
        std::process::exit(2);
    };
    let width = take("--width").map_or(2008, |w| w.parse().expect("--width N"));
    let model = take("--model").unwrap_or_else(|| "Mac14,7".into());
    let max_brightness = take("--max-brightness").map_or(255, |b| b.parse().expect("N"));
    let fn_held = args.iter().any(|a| a == "--fn");

    let hw = FakeHardware::new(width, 60);
    let panel = PanelInfo {
        width,
        height: 60,
        format: PixelFormat::Xrgb8888,
        max_brightness,
        model,
    };
    let mut d = Daemon::new(hw.backends(), panel, &Config::default(), None, 0);
    if fn_held {
        d.handle(0, Input::FnKey { held: true });
    }
    hw.display.frame().save_png(&out).expect("writing PNG");
    eprintln!(
        "{out}: {:?}, backlight {:?}",
        d.shown_view(),
        hw.backlight.current()
    );
}
