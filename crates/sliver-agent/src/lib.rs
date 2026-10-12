//! `sliver-agent`: the per-user session agent. It resolves the user's config for the current
//! context, runs modules, renders frames with sliver-render, and presents them to sliverd over
//! the private IPC (api.md §6). The core is sans-IO so tests drive it against
//! `sliver_ipc::fake::FakeDaemon`.

pub mod agent;
pub mod frame;
pub mod gesture;
pub mod icons;
pub mod link;
pub mod modules;

pub use agent::Agent;
