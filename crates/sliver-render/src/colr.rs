//! COLRv1 colour glyphs (Noto Color Emoji on Fedora), painted with tiny-skia.
//!
//! swash, which cosmic-text uses for glyph images, draws bitmap emoji (CBDT/sbix) and COLRv0 but
//! not COLRv1. For a glyph that has a COLRv1 paint graph we walk it with `skrifa` and implement
//! [`ColorPainter`] on a small canvas: transforms, glyph and box clips (as masks), solid, linear,
//! radial and sweep fills, and layers with the COLR composite modes. The result is a bitmap
//! cached by (font, glyph, size, foreground), so a frame only blits.

use skrifa::color::{
    Brush, ColorGlyphFormat, ColorPainter, ColorStop, CompositeMode, Extend,
    Transform as FontTransform,
};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Pixmap,
    PixmapPaint, RadialGradient, Rect, Shader, SpreadMode, SweepGradient, Transform,
};

/// A painted glyph and where it goes.
pub(crate) struct ColorImage {
    pub pixmap: Pixmap,
    /// Offset of the bitmap's top-left from the glyph origin (left end of the baseline), in px.
    pub left: i32,
    pub top: i32,
}

/// Paint `gid` at `size` px. `fg` is what palette index 0xFFFF (the text colour) means.
pub(crate) fn render(
    data: &[u8],
    index: u32,
    gid: u16,
    size: f32,
    fg: [u8; 4],
) -> Option<ColorImage> {
    let font = FontRef::from_index(data, index).ok()?;
    let gid = GlyphId::new(u32::from(gid));
    let glyph = font
        .color_glyphs()
        .get_with_format(gid, ColorGlyphFormat::ColrV1)
        .or_else(|| font.color_glyphs().get(gid))?;
    let upem = font.head().ok()?.units_per_em() as f32;
    if upem <= 0.0 || size <= 0.0 {
        return None;
    }
    // Emoji sit in roughly a 1.25 em wide box; leave room for ascent, descent and bleed.
    let (w, h) = (
        (size * 1.7).ceil().max(1.0) as u32,
        (size * 1.6).ceil().max(1.0) as u32,
    );
    let (x0, y0) = ((size * 0.2).round(), (size * 1.2).round());
    let scale = size / upem;
    let mut canvas = Canvas::new(
        &font,
        w,
        h,
        fg,
        Transform::from_row(scale, 0.0, 0.0, -scale, x0, y0),
    )?;
    glyph.paint(LocationRef::default(), &mut canvas).ok()?;
    let pixmap = canvas.layers.into_iter().next()?.0;
    Some(ColorImage {
        pixmap,
        left: -(x0 as i32),
        top: -(y0 as i32),
    })
}

fn palette(font: &FontRef) -> Vec<[u8; 4]> {
    let Ok(cpal) = font.cpal() else {
        return Vec::new();
    };
    let records = cpal
        .color_records_array()
        .and_then(|r| r.ok())
        .unwrap_or(&[]);
    let start = cpal
        .color_record_indices()
        .first()
        .map(|i| i.get() as usize)
        .unwrap_or(0);
    let n = cpal.num_palette_entries() as usize;
    records
        .get(start..start + n)
        .unwrap_or(&[])
        .iter()
        .map(|c| [c.red, c.green, c.blue, c.alpha])
        .collect()
}

struct Canvas<'a> {
    font: &'a FontRef<'a>,
    palette: Vec<[u8; 4]>,
    fg: [u8; 4],
    w: u32,
    h: u32,
    /// Layers with the mode that merges each down; index 0 is the result.
    layers: Vec<(Pixmap, CompositeMode)>,
    /// Font units to canvas pixels.
    transforms: Vec<Transform>,
    clips: Vec<Option<Mask>>,
}

impl<'a> Canvas<'a> {
    fn new(font: &'a FontRef<'a>, w: u32, h: u32, fg: [u8; 4], base: Transform) -> Option<Self> {
        Some(Canvas {
            font,
            palette: palette(font),
            fg,
            w,
            h,
            layers: vec![(Pixmap::new(w, h)?, CompositeMode::SrcOver)],
            transforms: vec![base],
            clips: vec![None],
        })
    }

    fn ts(&self) -> Transform {
        *self.transforms.last().expect("base transform")
    }

    fn color(&self, index: u16, alpha: f32) -> Color {
        let [r, g, b, a] = if index == 0xFFFF {
            self.fg
        } else {
            self.palette
                .get(index as usize)
                .copied()
                .unwrap_or([0, 0, 0, 255])
        };
        Color::from_rgba8(
            r,
            g,
            b,
            ((f32::from(a) * alpha.clamp(0.0, 1.0)).round()) as u8,
        )
    }

    fn stops(&self, stops: &[ColorStop]) -> Vec<GradientStop> {
        stops
            .iter()
            .map(|s| {
                GradientStop::new(
                    s.offset.clamp(0.0, 1.0),
                    self.color(s.palette_index, s.alpha),
                )
            })
            .collect()
    }

    /// The path of glyph `gid` in font units.
    fn outline(&self, gid: GlyphId) -> Option<tiny_skia::Path> {
        struct Pen(PathBuilder);
        impl OutlinePen for Pen {
            fn move_to(&mut self, x: f32, y: f32) {
                self.0.move_to(x, y);
            }
            fn line_to(&mut self, x: f32, y: f32) {
                self.0.line_to(x, y);
            }
            fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
                self.0.quad_to(cx0, cy0, x, y);
            }
            fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
                self.0.cubic_to(cx0, cy0, cx1, cy1, x, y);
            }
            fn close(&mut self) {
                self.0.close();
            }
        }
        let mut pen = Pen(PathBuilder::new());
        self.font
            .outline_glyphs()
            .get(gid)?
            .draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut pen,
            )
            .ok()?;
        pen.0.finish()
    }

    fn push_clip_path(&mut self, path: Option<tiny_skia::Path>) {
        let ts = self.ts();
        let prev = self.clips.last().cloned().flatten();
        let next = match (path, prev) {
            // A glyph with no outline clips everything away.
            (None, _) => Mask::new(self.w, self.h),
            (Some(p), Some(mut m)) => {
                m.intersect_path(&p, FillRule::Winding, true, ts);
                Some(m)
            }
            (Some(p), None) => Mask::new(self.w, self.h).map(|mut m| {
                m.fill_path(&p, FillRule::Winding, true, ts);
                m
            }),
        };
        self.clips.push(next);
    }

    fn shader(&self, brush: &Brush<'_>) -> Option<Shader<'static>> {
        let ts = self.ts();
        match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => Some(Shader::SolidColor(self.color(*palette_index, *alpha))),
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => {
                if let [only] = color_stops {
                    return Some(Shader::SolidColor(
                        self.color(only.palette_index, only.alpha),
                    ));
                }
                let stops = self.stops(color_stops);
                LinearGradient::new(
                    tiny_skia::Point::from_xy(p0.x, p0.y),
                    tiny_skia::Point::from_xy(p1.x, p1.y),
                    stops,
                    spread(*extend),
                    ts,
                )
            }
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => {
                if let [only] = color_stops {
                    return Some(Shader::SolidColor(
                        self.color(only.palette_index, only.alpha),
                    ));
                }
                let stops = self.stops(color_stops);
                RadialGradient::new(
                    tiny_skia::Point::from_xy(c0.x, c0.y),
                    r0.max(0.0),
                    tiny_skia::Point::from_xy(c1.x, c1.y),
                    r1.max(0.0),
                    stops,
                    spread(*extend),
                    ts,
                )
            }
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => {
                if let [only] = color_stops {
                    return Some(Shader::SolidColor(
                        self.color(only.palette_index, only.alpha),
                    ));
                }
                let stops = self.stops(color_stops);
                SweepGradient::new(
                    tiny_skia::Point::from_xy(c0.x, c0.y),
                    *start_angle,
                    *end_angle,
                    stops,
                    spread(*extend),
                    ts,
                )
            }
        }
    }
}

fn spread(e: Extend) -> SpreadMode {
    match e {
        Extend::Pad => SpreadMode::Pad,
        Extend::Repeat => SpreadMode::Repeat,
        Extend::Reflect => SpreadMode::Reflect,
        _ => SpreadMode::Pad,
    }
}

fn blend(m: CompositeMode) -> BlendMode {
    match m {
        CompositeMode::Clear => BlendMode::Clear,
        CompositeMode::Src => BlendMode::Source,
        CompositeMode::Dest => BlendMode::Destination,
        CompositeMode::SrcOver => BlendMode::SourceOver,
        CompositeMode::DestOver => BlendMode::DestinationOver,
        CompositeMode::SrcIn => BlendMode::SourceIn,
        CompositeMode::DestIn => BlendMode::DestinationIn,
        CompositeMode::SrcOut => BlendMode::SourceOut,
        CompositeMode::DestOut => BlendMode::DestinationOut,
        CompositeMode::SrcAtop => BlendMode::SourceAtop,
        CompositeMode::DestAtop => BlendMode::DestinationAtop,
        CompositeMode::Xor => BlendMode::Xor,
        CompositeMode::Plus => BlendMode::Plus,
        CompositeMode::Screen => BlendMode::Screen,
        CompositeMode::Overlay => BlendMode::Overlay,
        CompositeMode::Darken => BlendMode::Darken,
        CompositeMode::Lighten => BlendMode::Lighten,
        CompositeMode::ColorDodge => BlendMode::ColorDodge,
        CompositeMode::ColorBurn => BlendMode::ColorBurn,
        CompositeMode::HardLight => BlendMode::HardLight,
        CompositeMode::SoftLight => BlendMode::SoftLight,
        CompositeMode::Difference => BlendMode::Difference,
        CompositeMode::Exclusion => BlendMode::Exclusion,
        CompositeMode::Multiply => BlendMode::Multiply,
        CompositeMode::HslHue => BlendMode::Hue,
        CompositeMode::HslSaturation => BlendMode::Saturation,
        CompositeMode::HslColor => BlendMode::Color,
        CompositeMode::HslLuminosity => BlendMode::Luminosity,
        _ => BlendMode::SourceOver,
    }
}

fn to_ts(t: FontTransform) -> Transform {
    Transform::from_row(t.xx, t.yx, t.xy, t.yy, t.dx, t.dy)
}

impl ColorPainter for Canvas<'_> {
    fn push_transform(&mut self, t: FontTransform) {
        let next = self.ts().pre_concat(to_ts(t));
        self.transforms.push(next);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }

    fn push_clip_glyph(&mut self, gid: GlyphId) {
        let path = self.outline(gid);
        self.push_clip_path(path);
    }

    fn push_clip_box(&mut self, b: skrifa::raw::types::BoundingBox<f32>) {
        let path = Rect::from_ltrb(b.x_min, b.y_min, b.x_max, b.y_max).map(PathBuilder::from_rect);
        self.push_clip_path(path);
    }

    fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    fn fill(&mut self, brush: Brush<'_>) {
        let Some(shader) = self.shader(&brush) else {
            return;
        };
        let paint = Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        };
        let mask = self.clips.last().and_then(|m| m.as_ref());
        // A fully clipped-away glyph (empty outline) paints nothing.
        let Some(whole) = Rect::from_xywh(0.0, 0.0, self.w as f32, self.h as f32) else {
            return;
        };
        if let Some((layer, _)) = self.layers.last_mut() {
            layer.fill_rect(whole, &paint, Transform::identity(), mask);
        }
    }

    fn push_layer(&mut self, mode: CompositeMode) {
        if let Some(p) = Pixmap::new(self.w, self.h) {
            self.layers.push((p, mode));
        }
    }

    fn pop_layer_with_mode(&mut self, _mode: CompositeMode) {
        if self.layers.len() < 2 {
            return;
        }
        let (layer, mode) = self.layers.pop().expect("checked");
        let paint = PixmapPaint {
            blend_mode: blend(mode),
            ..PixmapPaint::default()
        };
        if let Some((below, _)) = self.layers.last_mut() {
            below.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), None);
        }
    }
}
