//! Renders a row of Touch Bar items to pixels (ADR-0002).
//!
//! Pure Rust: [`tiny_skia`] rasterizes, [`resvg`] draws SVG icons and `cosmic-text` shapes text.
//! There are no system library dependencies, and an embedded font and icon set mean the safe layer
//! always renders (GOAL invariant 1).
//!
//! The pipeline:
//! 1. [`layout_row`] turns a list of stretches into item rectangles. The daemon uses the same
//!    function for hit-testing ([`hit_test`]), so what you see is what you touch.
//! 2. [`Renderer::render`] draws a [`Scene`] with a [`Theme`] into a landscape [`Pixmap`]
//!    (e.g. 2008x60). Headless callers write it out with [`Pixmap::encode_png`].
//! 3. [`panel::damage`] finds what changed since the last frame, and [`panel::blit`] rotates it into
//!    the panel's native framebuffer layout.

mod color;
mod geometry;
mod icon;
pub mod panel;
mod render;
mod scene;
mod text;
mod theme;

pub use color::Color;
pub use geometry::{Rect, hit_test, layout_row};
pub use icon::{BUILTIN_ICONS, Icon, IconError};
pub use render::Renderer;
pub use scene::{Content, Item, Scene};
pub use text::{EMBEDDED_FONT_FAMILY, Fonts};
pub use theme::{FontSpec, Theme};
pub use tiny_skia::Pixmap;
