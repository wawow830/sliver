//! Print the desktop state as one JSON line per change, on whatever compositor this session
//! runs (Hyprland, sway/i3, niri). Read-only. `cargo run -p sliver-desktop --example desktop-watch`

use sliver_desktop::watch_auto;
use std::ops::ControlFlow;

fn main() {
    let r = watch_auto(
        |k| std::env::var(k).ok(),
        |s| {
            println!("{}", serde_json::to_string(s).expect("serializable"));
            ControlFlow::Continue(())
        },
    );
    if let Err(e) = r {
        eprintln!("desktop-watch: {e}");
        std::process::exit(1);
    }
}
