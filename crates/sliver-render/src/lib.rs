//! Renders a row of Touch Bar items to pixels (ADR-0002).
//!
//! Pure Rust: [`tiny_skia`] rasterizes, [`resvg`] draws SVG icons and `cosmic-text` shapes text.
//! There are no system library dependencies, and an embedded font and icon set mean the safe layer
//! always renders (GOAL invariant 1).
//!
//! The pipeline:
//! 1. [`Renderer::layout`] measures content and [`solve`]s zones of `Auto`/`Fixed`/`Fill` items
//!    into rectangles. Use the same rects for touch regions ([`hit_test`]), so what you see is what
//!    you touch.
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
pub use geometry::{Rect, Sizing, Spacing, Width, hit_test, solve};
pub use icon::{BUILTIN_ICONS, Icon, IconError};
pub use render::Renderer;
pub use scene::{Content, Item, Scene, Zone};
pub use text::{EMBEDDED_FONT_FAMILY, Fonts};
pub use theme::{FontSpec, Theme};
pub use tiny_skia::Pixmap;
