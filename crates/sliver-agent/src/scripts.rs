//! The runtime for user modules (`custom/*` and others with an `exec`), described in
//! sliver-config(5).
//!
//! A module is a shell command plus how often to run it ([`Spec`]). [`Runtime`] schedules them
//! (once, every N, as a continuous stream, or on a real-time signal), parses what they print
//! ([`Output`]: plain text or one JSON object per line) and sends an [`Update`] only when the
//! output changed. Threads sleep on channels, an inactive runtime arms no timers, restarts back
//! off, and every period has a floor.
//!
//! Commands run as `sh -c` in their own process group, never as root, with `no_new_privs`, a
//! timeout that kills the whole group, and bounded output. Built-in modules use the same
//! [`Output`].

mod output;
mod proc;
mod runtime;
mod signals;
mod spec;

pub use output::{Output, escape_markup, parse_json, parse_text};
pub use runtime::Runtime;
pub use signals::{forward_realtime_signals, forward_realtime_signals_to};
pub use spec::{DEFAULT_TIMEOUT, MAX_SIGNAL, MIN_RESTART, Mode, Spec};

/// A module produced new output.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// The module key (`custom/vpn`).
    pub module: String,
    pub output: Output,
}

/// What the commands see as `SLIVER_LAYER`, `SLIVER_LAYOUT`, `SLIVER_APP_ID`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub layer: String,
    pub layout: String,
    pub app_id: String,
}
