//! Sliver's wire contracts. Owner: cedar (ADR-0003). The spec is `docs/api.md`.
//!
//! - [`proto`]: the private, versioned IPC between the privileged `sliverd` and the per-user
//!   `sliver-agent` (one JSON object per `SOCK_SEQPACKET` packet, pixels in sealed memfds).
//! - [`validate`]: checks `sliverd` applies to everything the agent sends, including the
//!   escape floor (invariant 1). Pure functions, so they're unit-testable and fuzzable.
//! - [`keys`]: the evdev keycodes the protocol cares about.
//! - [`dbus`]: names and limits of the public session D-Bus API.
//!
//! This crate does no I/O. Socket and fd handling live in the daemon and the agent.

pub mod dbus;
pub mod keys;
pub mod proto;
pub mod validate;

pub use proto::{AgentMsg, DaemonMsg, PROTO};
