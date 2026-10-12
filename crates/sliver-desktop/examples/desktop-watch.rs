//! Prints the desktop state as one JSON line on every change, for whatever compositor the
//! environment points at (the same backend choice the agent makes). For the headless checks in
//! `integration/sway/test.sh`. Runs until killed.
//!
//!     SWAYSOCK=… cargo run -p sliver-desktop --example desktop-watch
use std::ops::ControlFlow;

fn main() {
    let r = sliver_desktop::watch_auto(
        |k| std::env::var(k).ok(),
        |s| {
            println!("{}", serde_json::to_string(s).unwrap());
            ControlFlow::Continue(())
        },
    );
    eprintln!("watch ended: {r:?}");
}
