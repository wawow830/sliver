//! `sliver-agent`: the per-user session agent. It resolves the user's config for the current
//! context, runs modules, renders frames with sliver-render, and presents them to sliverd over
//! the private IPC (api.md §6).
//!
//! ```text
//! feeds (desktop, session, user modules) ─► World
//! config ─resolve(layer, context)─► zones ─modules─► Cells ─frame─► image, pressed image, regions
//! driver: Inputs (touches, display, changes, time) ─► Agent ─► Outputs (present, keys, effects)
//! the binary: file descriptors ─► Inputs, Outputs ─► sliverd, the session, processes
//! ```
//!
//! [`agent::Agent`] is the sans-IO core: a world becomes frames, a touch becomes effects.
//! [`driver::Driver`] decides when to draw and what sliverd and cava must be told, and the binary
//! only polls and carries out. Tests drive the core against `sliverd::testing::TestDaemon`.
//!
//! - **Two IPC layers.** sliverd's `base` slot shows the top custom layer (or `base`) and its `fn`
//!   slot shows the config's `fn` layer; sliverd swaps them on Fn by itself, with no round trip.
//! - **Key modules are key regions.** A module whose only binding is "tap → key, fire on press"
//!   becomes a sliverd `key` region, so Esc and the F-keys never wait on the agent and keep
//!   working if it hangs. Every other interactive cell is an `agent` region for the gesture
//!   recognizer. Regions are full height and reach up to [`frame::REGION_SLOP`] into the gaps
//!   between cells, so near-misses count but empty bar space doesn't.
//! - **Power (invariant 3).** Nothing renders unless something changed. The only deadlines are a
//!   module's next change (clock ticks), animation frames, gesture timers and paced content, and
//!   none run while the display is off. Content that changes on its own is drawn at most
//!   [`pace::CONTENT_FPS`] times a second; touches, layer switches and animations draw at once.
//!   The link drops frames whose pixels and regions didn't change and sends damage spans.
//! - **Untrusted input.** Script output, published items and user stylesheets may be anything,
//!   and none may take the agent down, since systemd would restart it into the same input.
//!   Oversized raster icons are refused before decoding, and building a frame runs under
//!   `catch_unwind`, so a panicking module leaves that layer's last good frame up. Fixed-seed
//!   random inputs in `tests/robustness.rs` check that nothing panics.

pub mod act;
pub mod agent;
pub mod api;
pub mod context;
pub mod dbus_call;
pub mod display;
pub mod driver;
pub mod feeds;
pub mod frame;
pub mod gesture;
pub mod icons;
pub mod keymap;
pub mod link;
pub mod modules;
pub mod now;
pub mod ops;
pub mod pace;
pub mod privacy;
pub mod published;
pub mod reload;
pub mod scripts;
pub mod stats;
pub mod wait;

pub use agent::Agent;
