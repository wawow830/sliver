//! Print the desktop state as one JSON line per change. Read-only; safe on a live session.
//! `cargo run -p sliver-desktop --example hypr-watch`

use sliver_desktop::hyprland::{Paths, watch};
use std::ops::ControlFlow;

fn main() {
    let Some(paths) = Paths::from_env(|k| std::env::var(k).ok()) else {
        eprintln!("HYPRLAND_INSTANCE_SIGNATURE is not set: not a Hyprland session");
        std::process::exit(1);
    };
    watch(&paths, |s| {
        println!("{}", serde_json::to_string(s).expect("serializable"));
        ControlFlow::Continue(())
    });
}
