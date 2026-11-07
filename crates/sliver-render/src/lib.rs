//! Renders a row of Touch Bar items to pixels.
//!
//! Pure Rust: [`tiny_skia`] rasterizes, [`resvg`] draws SVG icons and `cosmic-text` shapes text.
//! No system library dependencies; an embedded font and icon set render without installed assets.
//!
//! The pipeline:
//! 1. A [`Stylesheet`] (waybar-style CSS) styles the `bar > zone > module` tree.
//! 2. [`Renderer::layout`] measures content and [`solve`]s zones and their `Auto`/`Fixed`/`Fill`
//!    items into rectangles. Touch regions use the same rects ([`hit_test`]).
//! 3. [`Renderer::render`] draws a [`Scene`] into a landscape [`Pixmap`] (e.g. 2008x60).
//!    Headless callers write it out with [`Pixmap::encode_png`].
//! 4. [`panel::damage`] finds what changed since the last frame, and [`panel::blit_xrgb8888`]
//!    rotates it into the panel's native framebuffer layout.

mod anim;
mod color;
mod colr;
mod fontscan;
mod geometry;
mod icon;
pub mod markup;
pub mod panel;
mod render;
mod scene;
pub mod style;
mod text;

pub use anim::FRAME;
pub use color::Color;
pub use geometry::{Align, Rect, Sizing, Width, hit_test, solve};
pub use icon::{BUILTIN_ICONS, Icon, IconError};
pub use render::{Layout, Renderer, composite_layers};
pub use scene::{Content, Item, Scene, Zone};
pub use style::{
    Diagnostic, LayerEffect, LayerTransition, Style, Stylesheet, Timing, Transition, TransitionProp,
};
pub use text::{EMBEDDED_FONT_FAMILY, Fonts};
pub use tiny_skia::Pixmap;
