//! `sliverd`: the system daemon. Owns the Touch Bar panel, touch input, backlight and uinput keys,
//! and always keeps a built-in safe layer (Esc + F-keys) available (GOAL invariant 1).
//!
//! The contract with `sliver-agent` is `docs/api.md` §6 and `sliver_ipc`. Layout:
//! - [`daemon`]: the sans-IO core. Agent frames + key regions, Fn layers, the safe hatch,
//!   liveness, sessions, display power. Driven by [`daemon::Input`] with simulated time in tests.
//! - [`builtin`]: the safe layer and built-in fn layer, rendered with sliver-render's embedded
//!   font and icons.
//! - [`backend`]: hardware traits. [`fake`] implements them for tests and headless runs. Real
//!   DRM/evdev/uinput/sysfs backends only run through the safe-swap harness.
//! - [`ipc`]: glue from `sliver_ipc::transport` packets/fds to [`daemon::Input`].
//! - [`profile`]: per-model quirks (docs/hardware.md).
//!
//! Touch semantics are `sliver_ipc::router::TouchRouter`, shared with cedar's `FakeDaemon`.

pub mod backend;
pub mod builtin;
pub mod daemon;
pub mod fake;
pub mod idle;
pub mod ipc;
pub mod keystate;
pub mod profile;
pub mod real;

pub use daemon::{ClientId, Config, Daemon, Input, Out, View};
