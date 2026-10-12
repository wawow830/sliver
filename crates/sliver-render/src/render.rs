use std::sync::Arc;

use cosmic_text::SwashContent;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

use crate::icon::Image;
use crate::text::{self, Label};
use crate::{Color, Content, Fonts, Icon, Item, Rect, Scene, Theme, layout_row};

/// Draws scenes. Holds font and glyph caches, so keep one around rather than making one per frame.
pub struct Renderer {
    fonts: Fonts,
}

impl Renderer {
    pub fn new(fonts: Fonts) -> Renderer {
        Renderer { fonts }
    }

    /// Item rectangles for `scene` on a `width` x `height` bar. Same geometry as [`Self::render`].
    pub fn layout(&self, scene: &Scene, theme: &Theme, width: u32, height: u32) -> Vec<Rect> {
        layout_row(
            width,
            height,
            &scene.stretches(),
            theme.spacing,
            theme.margin_y,
        )
    }

    /// Draws the whole scene into `pixmap`, a landscape bar (e.g. 2008x60).
    pub fn render(&mut self, scene: &Scene, theme: &Theme, pixmap: &mut Pixmap) {
        pixmap.fill(theme.background.to_skia());
        let rects = self.layout(scene, theme, pixmap.width(), pixmap.height());
        for (item, rect) in scene.items.iter().zip(rects) {
            self.draw_item(item, rect, theme, pixmap);
        }
    }

    /// Convenience: renders into a new `width` x `height` pixmap.
    pub fn render_new(&mut self, scene: &Scene, theme: &Theme, width: u32, height: u32) -> Pixmap {
        let mut pixmap = Pixmap::new(width, height).expect("non-zero bar size");
        self.render(scene, theme, &mut pixmap);
        pixmap
    }

    fn draw_item(&mut self, item: &Item, rect: Rect, theme: &Theme, pixmap: &mut Pixmap) {
        if matches!(item.content, Content::Spacer) {
            return;
        }
        let fill = match (item.pressed, item.background) {
            (true, Some(bg)) => Some(mix(bg, Color::WHITE, 0.25)),
            (true, None) => Some(theme.button_pressed),
            (false, Some(bg)) => Some(bg),
            (false, None) => theme.outlines.then_some(theme.button),
        };
        if let Some(fill) = fill {
            fill_rounded_rect(pixmap, rect, theme.radius, fill);
        }

        let fg = item.foreground.unwrap_or(theme.foreground);
        let avail = (rect.w - 2.0 * theme.padding_x).max(1.0);
        let icon_size = theme.icon_size.min(rect.h).min(avail).floor();
        let (cx, cy) = rect.center();
        match &item.content {
            Content::Spacer | Content::Blank => {}
            Content::Text(t) => {
                let label = self.fit_label(t, theme, avail);
                let x = (cx - label.width / 2.0).round();
                self.draw_label(label, x, cy, fg, rect, pixmap);
            }
            Content::Icon(icon) => {
                let x = (cx - icon_size / 2.0).round();
                let y = (cy - icon_size / 2.0).round();
                draw_icon(icon, x, y, icon_size, fg, pixmap);
            }
            Content::IconText(icon, t) => {
                let label = self.fit_label(t, theme, avail - icon_size - theme.icon_gap);
                let total = icon_size + theme.icon_gap + label.width;
                let x = (cx - total / 2.0).round();
                let y = (cy - icon_size / 2.0).round();
                draw_icon(icon, x, y, icon_size, fg, pixmap);
                let tx = x + icon_size + theme.icon_gap;
                self.draw_label(label, tx, cy, fg, rect, pixmap);
            }
        }
    }

    /// Shapes `text` at the theme's size, shrinking it (down to `min_size`) to fit `avail` pixels,
    /// then ellipsizing if it still doesn't fit.
    fn fit_label(&mut self, text: &str, theme: &Theme, avail: f32) -> Arc<Label> {
        let f = &theme.font;
        let mut shape =
            |t: &str, size: f32| text::shape(&mut self.fonts, t, &f.family, f.weight, size);
        let label = shape(text, f.size);
        if label.width <= avail {
            return label;
        }
        let size = (f.size * avail / label.width)
            .floor()
            .clamp(f.min_size, f.size);
        let label = shape(text, size);
        if label.width <= avail {
            return label;
        }
        // Longest prefix that fits with an ellipsis (binary search over char counts).
        let chars: Vec<char> = text.chars().collect();
        let truncated =
            |n: usize| chars[..n].iter().collect::<String>().trim_end().to_owned() + "…";
        let (mut lo, mut hi) = (0, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if shape(&truncated(mid), size).width <= avail {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        shape(&truncated(lo), size)
    }

    /// Draws a shaped label starting at `x`, optically centered on `cy`, clipped to `clip`.
    fn draw_label(
        &mut self,
        label: Arc<Label>,
        x: f32,
        cy: f32,
        color: Color,
        clip: Rect,
        pixmap: &mut Pixmap,
    ) {
        let baseline = (cy + label.cap_height / 2.0).round();
        let clip_x = (clip.x.floor() as i32, clip.right().ceil() as i32);
        let Fonts { system, cache, .. } = &mut self.fonts;
        for run in label.buffer.layout_runs() {
            for glyph in run.glyphs {
                let phys = glyph.physical((x, baseline), 1.0);
                let Some(image) = cache.get_image(system, phys.cache_key) else {
                    continue;
                };
                let p = image.placement;
                let origin = (phys.x + p.left, phys.y - p.top);
                let color = glyph
                    .color_opt
                    .map(|c| Color::rgba(c.r(), c.g(), c.b(), c.a()))
                    .unwrap_or(color);
                blend_glyph(
                    pixmap,
                    origin,
                    (p.width, p.height),
                    image.content,
                    &image.data,
                    color,
                    clip_x,
                );
            }
        }
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

fn fill_rounded_rect(pixmap: &mut Pixmap, r: Rect, radius: f32, color: Color) {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    // Cubic Bézier approximation of a quarter circle.
    let k = rad * (1.0 - 0.552_284_8);
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - k, y0, x1, y0 + k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - k, x1 - k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + k, y1, x0, y1 - k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + k, x0 + k, y0, x0 + rad, y0);
    pb.close();
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color(color.to_skia());
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn draw_icon(icon: &Icon, x: f32, y: f32, size: f32, fg: Color, pixmap: &mut Pixmap) {
    let (w, h) = icon.size();
    if w <= 0.0 || h <= 0.0 || size <= 0.0 {
        return;
    }
    // Fit inside size x size, keeping the aspect ratio, centered.
    let scale = (size / w).min(size / h);
    let dx = (size - w * scale) / 2.0;
    let dy = (size - h * scale) / 2.0;
    let draw = |target: &mut Pixmap, ox: f32, oy: f32| {
        let transform = Transform::from_row(scale, 0.0, 0.0, scale, ox + dx, oy + dy);
        match &icon.image {
            Image::Svg(tree) => resvg::render(tree, transform, &mut target.as_mut()),
            Image::Raster(src) => {
                let paint = PixmapPaint {
                    quality: tiny_skia::FilterQuality::Bicubic,
                    ..PixmapPaint::default()
                };
                target.draw_pixmap(0, 0, src.as_ref().as_ref(), &paint, transform, None);
            }
        }
    };
    if !icon.symbolic {
        draw(pixmap, x, y);
        return;
    }
    // Symbolic: draw the shape offscreen, keep only its coverage, and paint it in `fg`.
    let side = size.ceil() as u32;
    let Some(mut scratch) = Pixmap::new(side, side) else {
        return;
    };
    draw(&mut scratch, 0.0, 0.0);
    for px in scratch.data_mut().chunks_exact_mut(4) {
        let a = px[3] as u32 * fg.a as u32 / 255;
        let premul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        px.copy_from_slice(&[premul(fg.r), premul(fg.g), premul(fg.b), a as u8]);
    }
    pixmap.draw_pixmap(
        x as i32,
        y as i32,
        scratch.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

/// Source-over blends a glyph image into the premultiplied RGBA pixmap.
fn blend_glyph(
    pixmap: &mut Pixmap,
    origin: (i32, i32),
    size: (u32, u32),
    content: SwashContent,
    data: &[u8],
    color: Color,
    clip_x: (i32, i32),
) {
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let x_lo = clip_x.0.max(0);
    let x_hi = clip_x.1.min(pw);
    let dst = pixmap.data_mut();
    for gy in 0..size.1 as i32 {
        let y = origin.1 + gy;
        if y < 0 || y >= ph {
            continue;
        }
        for gx in 0..size.0 as i32 {
            let x = origin.0 + gx;
            if x < x_lo || x >= x_hi {
                continue;
            }
            let i = (gy * size.0 as i32 + gx) as usize;
            let (r, g, b, a) = match content {
                SwashContent::Mask => {
                    let a = data[i] as u32 * color.a as u32 / 255;
                    (color.r, color.g, color.b, a)
                }
                SwashContent::Color => {
                    let s = &data[i * 4..i * 4 + 4];
                    (s[0], s[1], s[2], s[3] as u32)
                }
                // We never ask swash for subpixel masks.
                SwashContent::SubpixelMask => continue,
            };
            if a == 0 {
                continue;
            }
            let d = &mut dst[((y * pw + x) * 4) as usize..][..4];
            let inv = 255 - a;
            let over = |s: u8, d: u8| ((s as u32 * a + d as u32 * inv + 127) / 255) as u8;
            d[0] = over(r, d[0]);
            d[1] = over(g, d[1]);
            d[2] = over(b, d[2]);
            d[3] = (a + (d[3] as u32 * inv + 127) / 255) as u8;
        }
    }
}
