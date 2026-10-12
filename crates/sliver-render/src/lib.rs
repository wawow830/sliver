//! Renders a row of Touch Bar items to pixels (ADR-0002).
//!
//! Pure Rust: [`tiny_skia`] rasterizes, [`resvg`] draws SVG icons and `cosmic-text` shapes text.
//! There are no system library dependencies, and an embedded font and icon set mean the safe layer
//! always renders (GOAL invariant 1).
//!
//! The pipeline:
//! 1. A [`Stylesheet`] (waybar-style CSS, ADR-0004) styles the `bar > zone > module` tree.
//! 2. [`Renderer::layout`] measures content and [`solve`]s zones and their `Auto`/`Fixed`/`Fill`
//!    items into rectangles. Use the same rects for touch regions ([`hit_test`]), so what you see
//!    is what you touch.
//! 3. [`Renderer::render`] draws a [`Scene`] into a landscape [`Pixmap`] (e.g. 2008x60).
//!    Headless callers write it out with [`Pixmap::encode_png`].
//! 4. [`panel::damage`] finds what changed since the last frame, and [`panel::blit`] rotates it into
//!    the panel's native framebuffer layout.

mod anim;
mod color;
mod colr;
mod geometry;
mod icon;
pub mod markup;
pub mod panel;
mod render;
mod scene;
pub mod style;
mod text;

pub use color::Color;
pub use geometry::{Align, Rect, Sizing, Width, hit_test, solve};
pub use icon::{BUILTIN_ICONS, Icon, IconError};
pub use render::{Layout, Renderer};
pub use scene::{Content, Item, Scene, Zone};
pub use style::{Diagnostic, Style, Stylesheet, Timing, Transition, TransitionProp};
pub use text::{EMBEDDED_FONT_FAMILY, Fonts};
pub use tiny_skia::Pixmap;
