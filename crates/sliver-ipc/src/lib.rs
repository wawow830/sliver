//! The narrow, versioned IPC between the privileged `sliverd` and the per-user `sliver-agent`.
//!
//! Stub: the protocol is designed in T5 (docs/api.md + ADR).

/// IPC protocol version. Peers must refuse to talk across incompatible major versions.
pub const PROTOCOL_VERSION: u32 = 0;
