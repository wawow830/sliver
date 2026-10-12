use crate::Color;
use crate::text::EMBEDDED_FONT_FAMILY;

/// Which font to draw labels with.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    /// A family name. Falls back to the embedded font if it isn't installed.
    pub family: String,
    /// CSS-style weight: 400 regular, 700 bold.
    pub weight: u16,
    /// Preferred size in pixels. Labels that don't fit shrink down to `min_size`.
    pub size: f32,
    pub min_size: f32,
}

/// Everything about how the bar looks that isn't content.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: Color,
    /// Button fill when idle. Ignored when `outlines` is false.
    pub button: Color,
    pub button_pressed: Color,
    pub foreground: Color,
    /// Draw idle buttons with a fill. When false, only pressed buttons show a fill.
    pub outlines: bool,
    pub radius: f32,
    /// Horizontal gap between slots, in pixels.
    pub spacing: f32,
    /// Gap between items and the top/bottom edge of the bar, in pixels.
    pub margin_y: f32,
    pub icon_size: f32,
    /// Gap between an icon and its label.
    pub icon_gap: f32,
    /// Minimum horizontal padding inside a button before labels shrink.
    pub padding_x: f32,
    pub font: FontSpec,
}

impl Theme {
    /// Pixel-compatible with tiny-dfr 0.3.7's look (gray 0.2/0.4 buttons, radius 8, 16 px gaps,
    /// 32 px bold labels, 48 px icons).
    pub fn tiny_dfr() -> Theme {
        Theme {
            background: Color::BLACK,
            button: Color::gray(0.2),
            button_pressed: Color::gray(0.4),
            foreground: Color::WHITE,
            outlines: true,
            radius: 8.0,
            spacing: 16.0,
            margin_y: 1.0,
            icon_size: 48.0,
            icon_gap: 8.0,
            padding_x: 8.0,
            font: FontSpec {
                family: EMBEDDED_FONT_FAMILY.to_string(),
                weight: 700,
                size: 32.0,
                min_size: 16.0,
            },
        }
    }
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::tiny_dfr()
    }
}
