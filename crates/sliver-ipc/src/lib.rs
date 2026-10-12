//! Sliver's wire contracts. Owner: cedar (ADR-0003). The spec is `docs/api.md`.
//!
//! - [`proto`]: the private, versioned IPC between the privileged `sliverd` and the per-user
//!   `sliver-agent` (one JSON object per `SOCK_SEQPACKET` packet, pixels in sealed memfds).
//! - [`validate`]: checks `sliverd` applies to everything the agent sends, including the
//!   escape floor (invariant 1). Pure functions, so they're unit-testable and fuzzable.
//! - [`keys`]: the evdev keycodes the protocol cares about.
//! - [`dbus`]: names and limits of the public session D-Bus API.
//! - [`transport`]: blocking/non-blocking `SOCK_SEQPACKET` connection with fd passing.
//! - [`shm`]: sealed-memfd frame buffers (agent writes, daemon imports and copies).
//! - [`router`]: the key-region touch semantics shared by sliverd and the fake.
//! - `fake` (feature `fake`): an in-process fake sliverd for headless tests.

pub mod dbus;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod keys;
pub mod proto;
pub mod router;
pub mod shm;
pub mod transport;
pub mod validate;

pub use proto::{AgentMsg, DaemonMsg, PROTO};
