//! `sliverd`: the system daemon. Owns the Touch Bar panel, touch input, backlight and uinput keys,
//! and always keeps a built-in safe layer (Esc + F-keys) available (invariant 1). It's a daemon of
//! its own because the bar's previous one, tiny-dfr, paints a crash screen and blocks while still
//! DRM master when its config has an error: no Esc, no F-keys, and `Restart=` never fires.
//!
//! The contract with `sliver-agent` is `sliver_ipc`. Layout:
//! - [`daemon`]: the sans-IO core. Agent frames + key regions, Fn layers, the safe hatch,
//!   liveness, sessions, display power. Driven by [`daemon::Input`] with simulated time in tests.
//! - [`builtin`]: the safe layer and built-in fn layer, rendered with sliver-render's embedded
//!   font and icons.
//! - [`backend`]: hardware traits. [`fake`] implements them for tests and headless runs. Real
//!   DRM/evdev/uinput/sysfs backends only run through the safe-swap harness.
//! - [`ipc`]: glue from `sliver_ipc::transport` packets/fds to [`daemon::Input`].
//! - [`profile`]: per-model quirks.
//! - [`server`]: the `poll(2)` event loop around the core. [`session`]: seat0's active uid.
//!   [`config`]: `/etc/sliver/sliverd.toml`.
//! - `testing` (feature): `TestDaemon`, the real core in-process for agent tests.
//! - [`router`]: which touches become keys and which go to the agent. [`hatch`]: the Fn safe hatch.

use sliver_cli::{Cli, Opt};

/// sliverd's command line (`sliverd --help`).
pub const CLI: Cli = Cli {
    name: "sliverd",
    version: env!("CARGO_PKG_VERSION"),
    synopsis: "\
sliverd [--socket PATH] [--config PATH]    (as sliverd.service, or under the safe-swap harness)
sliverd --fake-hardware [OPTION...]
sliverd --headless OUT.png [--fn] [OPTION...]",
    options: &[
        Opt::value(
            &["--socket"],
            "PATH",
            "the IPC socket if systemd passes none (default /run/sliver/sliverd.sock)",
        ),
        Opt::value(
            &["--config"],
            "PATH",
            "sliverd's own config (default /etc/sliver/sliverd.toml)",
        ),
        Opt::flag(
            &["--fake-hardware"],
            "fake panel, touch and keys: run the agent without a bar",
        ),
        Opt::value(
            &["--backend"],
            "real|fake",
            "the same, spelled out (fake is --fake-hardware)",
        ),
        Opt::value(
            &["--uid"],
            "N",
            "the active session's uid on fake hardware (default yours)",
        ),
        Opt::value(
            &["--frame-png"],
            "PATH",
            "rewrite this PNG on every panel flush (fake hardware)",
        ),
        Opt::value(
            &["--touch-socket"],
            "PATH",
            "take touches, Fn and lid as text lines (fake hardware)",
        ),
        Opt::value(
            &["--headless"],
            "OUT.png",
            "write what the panel shows, once, and exit",
        ),
        Opt::flag(&["--fn"], "with --headless: Fn held"),
        Opt::value(&["--width"], "N", "the fake panel's width (default 2008)"),
        Opt::value(
            &["--model"],
            "ID",
            "the fake panel's model (default Mac14,7)",
        ),
        Opt::value(
            &["--max-brightness"],
            "N",
            "the fake panel's top brightness (default 255)",
        ),
        Opt::value(
            &["--exit-after"],
            "SECONDS",
            "exit on its own after this long",
        ),
        Opt::flag(
            &["-v", "--verbose"],
            "log input, keys, state changes and agent traffic (or SLIVER_LOG=debug)",
        ),
    ],
};

pub mod backend;
pub mod builtin;
pub mod config;
pub mod daemon;
pub mod fake;
pub mod hatch;
pub mod idle;
pub mod inject;
pub mod ipc;
pub mod keystate;
pub mod log;
pub mod profile;
pub mod real;
pub mod router;
pub mod server;
pub mod session;
pub mod shift;
#[cfg(feature = "testing")]
pub mod testing;

pub use daemon::{ClientId, Config, Daemon, Input, Out, View};
