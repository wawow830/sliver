//! Sliver's wire contracts. Owner: cedar (ADR-0003). The spec is `docs/api.md`.
//!
//! - [`proto`]: the private, versioned IPC between the privileged `sliverd` and the per-user
//!   `sliver-agent` (one JSON object per `SOCK_SEQPACKET` packet, pixels in sealed memfds).
//! - [`validate`]: checks `sliverd` applies to everything the agent sends, including the
//!   advisory safe-key check. Pure functions, so they're unit-testable and fuzzable.
//! - [`keys`]: the evdev keycodes the protocol cares about.
//! - [`dbus`]: names and limits of the public session D-Bus API.
//! - [`report`]: ask sliverd for its hardware report (`sliverctl doctor`).
//! - [`transport`]: blocking/non-blocking `SOCK_SEQPACKET` connection with fd passing.
//! - [`shm`]: sealed-memfd frame buffers (agent writes, daemon imports and copies).
//! - [`router`]: the key-region touch semantics shared by sliverd and the fake.
//! - [`hatch`]: the Fn double-press-and-hold safe hatch (invariant 1).
//! - [`curve`]: the adaptive brightness curve math (sliverd applies it, settings plots it).
//! - Fake sliverd for headless tests: `sliverd::testing::TestDaemon` (feature `testing`), the real
//!   daemon core on fake hardware.

pub mod curve;
pub mod dbus;
pub mod hatch;
pub mod keys;
pub mod proto;
pub mod report;
pub mod router;
pub mod shm;
pub mod transport;
pub mod validate;

pub use proto::{AgentMsg, DaemonMsg, PROTO};
