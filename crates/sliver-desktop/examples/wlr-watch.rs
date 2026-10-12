//! Prints the focused window and the window/workspace lists every time the generic Wayland
//! backend sees a change (JSON, one line each). For `integration/wlroots/test.sh`:
//!
//! ```text
//! WAYLAND_DISPLAY=wayland-1 cargo run -p sliver-desktop --example wlr-watch [-- --focus-first-window]
//! ```
//!
//! With `--focus-first-window` it also sends a `FocusWindow` for the first window it sees that
//! is not focused, and `--close-focused` closes the focused one, to exercise the control path.

use sliver_desktop::control::{Action, run};
use std::ops::ControlFlow;

fn main() {
    let focus = std::env::args().any(|a| a == "--focus-first-window");
    let close = std::env::args().any(|a| a == "--close-focused");
    let mut done = false;
    let r = sliver_desktop::wlr::watch(|s| {
        println!("{}", serde_json::to_string(s).unwrap());
        if !done {
            if focus && let Some(w) = s.windows.iter().find(|w| !w.focused) {
                done = true;
                let addr = w.address.clone();
                std::thread::spawn(move || {
                    let r = run(
                        |k| std::env::var(k).ok(),
                        &Action::FocusWindow { address: addr },
                    );
                    eprintln!("focus: {r:?}");
                });
            }
            if close && let Some(w) = s.windows.iter().find(|w| w.focused) {
                done = true;
                let addr = w.address.clone();
                std::thread::spawn(move || {
                    let r = run(
                        |k| std::env::var(k).ok(),
                        &Action::CloseWindow { address: addr },
                    );
                    eprintln!("close: {r:?}");
                });
            }
        }
        ControlFlow::Continue(())
    });
    eprintln!("watch ended: {r:?}");
}
