//! Sliver's wire contracts: the private sliverd⇄agent IPC, and the public D-Bus API's names.

pub mod curve;
pub mod dbus;
pub mod keys;
pub mod proto;
pub mod report;
pub mod shm;
pub mod transport;
pub mod validate;

pub use proto::{AgentMsg, DaemonMsg, PROTO};
