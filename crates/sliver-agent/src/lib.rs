//! `sliver-agent`: the per-user session agent. It resolves the user's config for the current
//! context, runs modules, renders frames with sliver-render, and presents them to sliverd over
//! the private IPC.
//!
//! ```text
//! feeds (desktop, session, user modules) ─► World
//! config ─resolve(layer, context)─► zones ─modules─► Cells ─frame─► image, pressed image, regions
//! driver: Inputs (touches, display, changes, time) ─► Agent ─► Outputs (present, keys, effects)
//! the binary: file descriptors ─► Inputs, Outputs ─► sliverd, the session, processes
//! ```
//!
//! [`agent::Agent`] is the sans-IO core; [`driver::Driver`] decides when to draw and what sliverd
//! must be told; the binary polls and carries out.
//!
//! - sliverd's `base` slot shows the top custom layer (or `base`) and its `fn` slot the config's
//!   `fn` layer; sliverd swaps them on Fn itself.
//! - A module whose only binding is "tap → key, fire on press" becomes a sliverd `key` region,
//!   which works without the agent. Other interactive cells are `agent` regions. Regions are full
//!   height and reach up to [`frame::REGION_SLOP`] into the gaps between cells.
//! - Nothing renders unless something changed, and no deadline runs while the display is off.
//!   Content that changes on its own is drawn at most [`pace::CONTENT_FPS`] times a second.
//! - Script output, published items and user stylesheets are untrusted, and systemd restarts a
//!   crashed agent into the same input: oversized raster icons are refused before decoding, and
//!   building a frame runs under `catch_unwind`, keeping that layer's last good frame.

use sliver_cli::{Cli, Opt};

/// sliver-agent's command line (`sliver-agent --help`).
pub const CLI: Cli = Cli {
    name: "sliver-agent",
    version: env!("CARGO_PKG_VERSION"),
    synopsis: "\
sliver-agent [--config FILE] [--socket PATH] [--replace]
sliver-agent --render OUT [--config FILE] [--size WxH]",
    options: &[
        Opt::value(
            &["--config"],
            "FILE",
            "this config, not the search path (~/.config/sliver/config.toml)",
        ),
        Opt::value(
            &["--socket"],
            "PATH",
            "sliverd's socket (default /run/sliver/sliverd.sock, or $SLIVER_SOCKET)",
        ),
        Opt::flag(
            &["--replace"],
            "take the D-Bus name from a running agent (it's handed back on exit)",
        ),
        Opt::value(
            &["--render"],
            "OUT",
            "headless, without sliverd: write OUT-base.png and OUT-fn.png",
        ),
        Opt::value(
            &["--size"],
            "WxH",
            "the bar's size for --render (default 2008x60)",
        ),
    ],
};

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
