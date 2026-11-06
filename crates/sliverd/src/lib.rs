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

/// What `sliverd --help` prints, and the flags it accepts: [`accepts`] rejects any other.
pub const USAGE: &str = "\
usage: sliverd --headless OUT.png [--fn] [PANEL]
           Run the core once on fake hardware and write what the panel would show.
       sliverd --fake-hardware [--socket PATH] [--uid N] [--frame-png PATH] [--config PATH]
               [--touch-socket PATH] [PANEL]
           Serve the real IPC socket with fake hardware, to develop and test the agent without a
           bar. The active uid defaults to yours. --frame-png rewrites a PNG on every flush;
           --touch-socket takes injected touches, Fn and lid as text lines (sliverd::inject).
       sliverd [--socket PATH] [--config PATH]
           Real hardware: only under the safe-swap harness (SLIVER_SAFESWAP=1) or the packaged
           sliverd.service (SLIVER_SYSTEM_UNIT=1).
PANEL: [--width 2008] [--model Mac14,7] [--max-brightness 255]
Also: --backend real|fake (fake = --fake-hardware), --exit-after SECONDS,
      -v | --verbose (or SLIVER_LOG=debug), -V | --version, -h | --help.
The socket comes from systemd (FileDescriptorName=ipc) or --socket (default
/run/sliver/sliverd.sock); under the harness it defaults to $SLIVER_OUT_DIR/sliverd.sock and
sliverd exits 2 s before SLIVER_SAFESWAP_SECONDS.
";

/// Is `arg` acceptable on the command line: a value, or a flag [`USAGE`] names?
pub fn accepts(arg: &str) -> bool {
    let words = USAGE.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
    !arg.starts_with('-') || words.into_iter().any(|w| w == arg)
}

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
