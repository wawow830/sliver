//! Painting: boxes, labels, icons, sliders, graphs and bars into the pixmap.

use super::*;

impl Renderer {
    pub(super) fn draw_item(
        &mut self,
        item: &Item,
        rect: Rect,
        s: &Style,
        opacity: f32,
        spectrum: Option<Bars>,
        pixmap: &mut Pixmap,
    ) {
        if matches!(item.content, Content::Spacer) || opacity <= 0.0 {
            return;
        }
        draw_box(pixmap, rect, s, item.background, opacity);
        if let Some(b) = spectrum {
            draw_bars_split(pixmap, b.area, &item.spectrum, b.color, b.played);
        }
        let fg = fade(item.foreground.unwrap_or(s.color), opacity);
        let content = rect.inset(s.padding.map(|p| p + s.border_width));
        let avail = content.w.max(1.0);
        let icon_size = s
            .icon_size
            .min(content.h.max(rect.h * 0.5))
            .min(avail)
            .floor();
        let (cx, cy) = content.center();
        let clip = rect.inset([0.0, s.border_width, 0.0, s.border_width]);
        let fit = |icon: &Icon| {
            if s.icon_fit_content {
                icon.fit_content()
            } else {
                icon.clone()
            }
        };
        match &item.content {
            Content::Spacer => {}
            Content::Label { icon, text } => {
                let y = (cy - icon_size / 2.0).round();
                match (icon, text.is_empty()) {
                    (None, true) => {}
                    (None, false) => {
                        let label = self.fit_label(text, s, avail);
                        let x = (cx - label.width / 2.0).round();
                        self.draw_label(&label, x, cy, fg, clip, pixmap);
                    }
                    (Some(icon), true) => {
                        let x = (cx - icon_size / 2.0).round();
                        draw_icon(&fit(icon), x, y, icon_size, fg, opacity, pixmap);
                    }
                    (Some(icon), false) => {
                        let label = self.fit_label(text, s, avail - icon_size - s.gap);
                        let total = icon_size + s.gap + label.width;
                        let x = (cx - total / 2.0).round().max(content.x.floor());
                        draw_icon(&fit(icon), x, y, icon_size, fg, opacity, pixmap);
                        self.draw_label(&label, x + icon_size + s.gap, cy, fg, clip, pixmap);
                    }
                }
            }
            Content::Graph { values, capacity } => {
                // Some room above the highest value and below the area, inside any padding.
                let v = (content.h * 0.15).round();
                draw_graph(
                    pixmap,
                    content.inset([v, 0.0, v, 0.0]),
                    values,
                    *capacity,
                    fg,
                );
            }
            Content::Bars(levels) => draw_bars(pixmap, content, levels, fg),
            Content::Image(icon) => {
                let (x, y) = (content.x.round(), content.y.round());
                let (w, h) = (content.w.floor(), content.h.floor());
                // Clipped to the item's rounded box (inside its border), so a picture with no
                // padding (album art) keeps the button's corners.
                let inner = rect.inset([s.border_width; 4]);
                let mask = (s.radius > s.border_width)
                    .then(|| rounded_rect(inner, s.radius - s.border_width))
                    .flatten()
                    .and_then(|path| {
                        let mut m = tiny_skia::Mask::new(pixmap.width(), pixmap.height())?;
                        m.fill_path(&path, FillRule::Winding, true, Transform::identity());
                        Some(m)
                    });
                draw_icon_in(
                    icon,
                    Rect::new(x, y, w, h),
                    fg,
                    opacity,
                    mask.as_ref(),
                    pixmap,
                );
            }
            Content::Slider { icon, .. } => {
                if let Some(icon) = icon {
                    let y = (cy - icon_size / 2.0).round();
                    draw_icon(
                        &fit(icon),
                        content.x.round(),
                        y,
                        icon_size,
                        fg,
                        opacity,
                        pixmap,
                    );
                }
            }
        }
    }

    /// Shapes markup `text` at the style's size, shrinking it (down to half, at least 10 px) to
    /// fit `avail` pixels, then ellipsizing if it still doesn't fit.
    pub(super) fn fit_label(&mut self, text: &str, s: &Style, avail: f32) -> Arc<Label> {
        let family = self.fonts.pick_family(&s.font_family);
        let (size, weight) = (s.font_size, s.font_weight);
        let min_size = (size * 0.5).max(10.0).min(size);
        let mut shape = |t: &str, size: f32| text::shape(&mut self.fonts, t, &family, weight, size);
        let label = shape(text, size);
        if label.width <= avail {
            return label;
        }
        let size = (size * avail / label.width).floor().clamp(min_size, size);
        let label = shape(text, size);
        if label.width <= avail {
            return label;
        }
        // Longest prefix that fits with an ellipsis (binary search over char counts), cut on the
        // plain text so styles survive and tags are never split.
        let parsed = crate::markup::parse(text);
        let truncated = |n: usize| {
            let mut cut = parsed.truncated(n);
            if let Some(last) = cut.spans.last_mut() {
                last.text = last.text.trim_end().to_owned();
                last.text.push('…');
            }
            cut.to_markup()
        };
        let total = parsed.len_chars();
        let (mut lo, mut hi) = (0, total);
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
    pub(super) fn draw_label(
        &mut self,
        label: &Label,
        x: f32,
        cy: f32,
        color: Color,
        clip: Rect,
        pixmap: &mut Pixmap,
    ) {
        let baseline = (cy + label.cap_height / 2.0).round();
        let clip_x = (clip.x.floor() as i32, clip.right().ceil() as i32);
        for run in label.buffer.layout_runs() {
            draw_decorations(pixmap, &run, (x, baseline), color, clip_x);
            for glyph in run.glyphs {
                let phys = glyph.physical((x, baseline), 1.0);
                // COLRv1 emoji: swash can't draw them, so paint the paint graph ourselves.
                let fg = glyph
                    .color_opt
                    .map_or([color.r, color.g, color.b, 255], |c| {
                        [c.r(), c.g(), c.b(), 255]
                    });
                if let Some(img) = self.fonts.color_glyph(
                    glyph.font_id,
                    glyph.font_weight,
                    glyph.glyph_id,
                    glyph.font_size,
                    fg,
                ) {
                    blit_color(pixmap, &img, (phys.x, phys.y), color.a, clip_x);
                    continue;
                }
                let Fonts { system, cache, .. } = &mut self.fonts;
                let Some(image) = cache.get_image(system, phys.cache_key) else {
                    continue;
                };
                let p = image.placement;
                let origin = (phys.x + p.left, phys.y - p.top);
                let color = glyph
                    .color_opt
                    .map(|c| {
                        Color::rgba(
                            c.r(),
                            c.g(),
                            c.b(),
                            (c.a() as f32 * color.a as f32 / 255.0) as u8,
                        )
                    })
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

pub(super) fn fade(c: Color, opacity: f32) -> Color {
    Color::rgba(
        c.r,
        c.g,
        c.b,
        (c.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8,
    )
}

pub(super) fn rounded_rect(r: Rect, radius: f32) -> Option<Path> {
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
    pb.finish()
}

/// Natural width of a `graph` (content, without padding).
pub(super) const GRAPH_WIDTH: f32 = 60.0;

/// A sparkline in `r`: newest sample at the right edge, `capacity` samples across the width.
pub(super) fn draw_graph(
    pixmap: &mut Pixmap,
    r: Rect,
    values: &[f32],
    capacity: usize,
    color: Color,
) {
    if values.is_empty() || r.w < 2.0 || r.h < 2.0 || color.a == 0 {
        return;
    }
    let cap = capacity.max(values.len()).max(2);
    let step = r.w / (cap - 1) as f32;
    let first = cap - values.len();
    let line_w = 1.5;
    let (top, bottom) = (r.y + line_w / 2.0, r.bottom() - line_w / 2.0);
    let point = |i: usize, v: f32| {
        let x = r.x + (first + i) as f32 * step;
        (x, bottom - v.clamp(0.0, 1.0) * (bottom - top))
    };
    let mut line = PathBuilder::new();
    for (i, v) in values.iter().enumerate() {
        let (x, y) = point(i, *v);
        if i == 0 {
            line.move_to(x, y);
        } else {
            line.line_to(x, y);
        }
    }
    // One sample: a dot's worth of line at the right edge.
    if values.len() == 1 {
        let (x, y) = point(0, values[0]);
        line.line_to(x - step.min(2.0), y);
    }
    let Some(line) = line.finish() else {
        return;
    };
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    // The area under the line, then the line.
    let mut area = PathBuilder::new();
    let (x0, _) = point(0, 0.0);
    let (x1, _) = point(values.len() - 1, 0.0);
    area.move_to(x0, r.bottom());
    for (i, v) in values.iter().enumerate() {
        let (x, y) = point(i, *v);
        area.line_to(x, y);
    }
    area.line_to(x1, r.bottom());
    area.close();
    if let Some(area) = area.finish() {
        let mut fill = color;
        fill.a /= 4;
        paint.set_color(fill.to_skia());
        pixmap.fill_path(
            &area,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    paint.set_color(color.to_skia());
    let stroke = Stroke {
        width: line_w,
        line_join: tiny_skia::LineJoin::Round,
        line_cap: tiny_skia::LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(&line, &paint, &stroke, Transform::identity(), None);
}

/// Natural spacing of `Bars`, per level.
pub(super) const BAR_PITCH: f32 = 12.0;

/// One bar per level across `r`, each half its slot wide with round ends, centred on the middle
/// line and at least as tall as it is wide.
/// Where and how an item's spectrum is drawn: in `color`, except bars left of `played`'s x,
/// which take its colour (a slider's part up to its value).
#[derive(Clone, Copy)]
pub(super) struct Bars {
    pub(super) area: Rect,
    pub(super) color: Color,
    pub(super) played: Option<(f32, Color)>,
}

pub(super) fn draw_bars(pixmap: &mut Pixmap, r: Rect, levels: &[f32], color: Color) {
    draw_bars_split(pixmap, r, levels, color, None);
}

pub(super) fn draw_bars_split(
    pixmap: &mut Pixmap,
    r: Rect,
    levels: &[f32],
    color: Color,
    played: Option<(f32, Color)>,
) {
    if levels.is_empty() || r.w < 1.0 || r.h < 1.0 {
        return;
    }
    let pitch = r.w / levels.len() as f32;
    let w = (pitch / 2.0).min(r.h).max(1.0);
    let (mut rest, mut before) = (PathBuilder::new(), PathBuilder::new());
    for (i, v) in levels.iter().enumerate() {
        let h = w + v.clamp(0.0, 1.0) * (r.h - w);
        let cx = r.x + (i as f32 + 0.5) * pitch;
        let y = r.y + (r.h - h) / 2.0;
        let pb = match played {
            Some((x, _)) if cx < x => &mut before,
            _ => &mut rest,
        };
        if let Some(path) = rounded_rect(
            Rect {
                x: cx - w / 2.0,
                y,
                w,
                h,
            },
            w / 2.0,
        ) {
            pb.push_path(&path);
        }
    }
    let played_color = played.map_or(color, |(_, c)| c);
    for (pb, c) in [(rest, color), (before, played_color)] {
        if c.a == 0 {
            continue;
        }
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(c.to_skia());
        if let Some(path) = pb.finish() {
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

/// Background and border of a bar, zone or module.
pub(super) fn draw_box(
    pixmap: &mut Pixmap,
    r: Rect,
    s: &Style,
    bg_override: Option<Color>,
    opacity: f32,
) {
    if r.w <= 0.0 || r.h <= 0.0 {
        return;
    }
    let bg = fade(bg_override.unwrap_or(s.background), opacity);
    if bg.a > 0
        && let Some(path) = rounded_rect(r, s.radius)
    {
        let mut paint = Paint::default();
        paint.set_color(bg.to_skia());
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let bw = s.border_width.min(r.w / 2.0).min(r.h / 2.0);
    let border = fade(s.border_color(), opacity);
    if bw > 0.0
        && border.a > 0
        && let Some(path) = rounded_rect(r.inset([bw / 2.0; 4]), (s.radius - bw / 2.0).max(0.0))
    {
        let mut paint = Paint::default();
        paint.set_color(border.to_skia());
        paint.anti_alias = true;
        let stroke = Stroke {
            width: bw,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// Trough, then the filled highlight up to `value`, then the knob (if it has a size).
pub(super) fn draw_slider(
    pixmap: &mut Pixmap,
    track: Rect,
    value: f32,
    parts: &[Arc<Style>; 3],
    opacity: f32,
) {
    let [trough, hl, knob] = parts;
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    draw_box(pixmap, track, trough, None, opacity * trough.opacity);
    let fill = Rect::new(track.x, track.y, track.w * value, track.h);
    draw_box(
        pixmap,
        fill,
        hl,
        None,
        opacity * trough.opacity * hl.opacity,
    );
    let (kw, kh) = (knob.min_width, knob.min_height.max(knob.min_width));
    // The knob stays inside the trough's ends; a trough too short for it (mid-glide while a
    // slider opens) shows no knob yet.
    let (lo, hi) = (track.x + kw / 2.0, track.right() - kw / 2.0);
    if kw > 0.0 && lo <= hi {
        let cx = (track.x + track.w * value).clamp(lo, hi);
        let r = Rect::new(cx - kw / 2.0, track.center().1 - kh / 2.0, kw, kh);
        draw_box(
            pixmap,
            r,
            knob,
            None,
            opacity * trough.opacity * knob.opacity,
        );
    }
}

pub(super) fn draw_icon(
    icon: &Icon,
    x: f32,
    y: f32,
    size: f32,
    fg: Color,
    opacity: f32,
    pixmap: &mut Pixmap,
) {
    draw_icon_in(icon, Rect::new(x, y, size, size), fg, opacity, None, pixmap);
}

/// Draws `icon` fitted inside `area` (aspect ratio kept, centered).
pub(super) fn draw_icon_in(
    icon: &Icon,
    area: Rect,
    fg: Color,
    opacity: f32,
    mask: Option<&tiny_skia::Mask>,
    pixmap: &mut Pixmap,
) {
    let Rect { x, y, w: bw, h: bh } = area;
    let (w, h) = icon.size();
    if w <= 0.0 || h <= 0.0 || bw <= 0.0 || bh <= 0.0 {
        return;
    }
    let scale = (bw / w).min(bh / h);
    let (vx, vy) = icon.origin();
    let dx = (bw - w * scale) / 2.0 - vx * scale;
    let dy = (bh - h * scale) / 2.0 - vy * scale;
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
    if !icon.symbolic && opacity >= 1.0 && mask.is_none() {
        draw(pixmap, x, y);
        return;
    }
    // Draw offscreen, then either recolor (symbolic: keep only coverage, paint in `fg`) or fade.
    let Some(mut scratch) = Pixmap::new(bw.ceil() as u32, bh.ceil() as u32) else {
        return;
    };
    draw(&mut scratch, 0.0, 0.0);
    let mut paint = PixmapPaint::default();
    if icon.symbolic {
        for px in scratch.data_mut().chunks_exact_mut(4) {
            let a = px[3] as u32 * fg.a as u32 / 255;
            let premul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            px.copy_from_slice(&[premul(fg.r), premul(fg.g), premul(fg.b), a as u8]);
        }
    } else {
        paint.opacity = opacity.clamp(0.0, 1.0);
    }
    pixmap.draw_pixmap(
        x as i32,
        y as i32,
        scratch.as_ref(),
        &paint,
        Transform::identity(),
        mask,
    );
}

/// Source-over blends a glyph image into the premultiplied RGBA pixmap.
/// Source-over a premultiplied colour bitmap at `origin` (+ its own offset), scaled by `alpha`.
pub(super) fn blit_color(
    pixmap: &mut Pixmap,
    img: &crate::colr::ColorImage,
    origin: (i32, i32),
    alpha: u8,
    clip_x: (i32, i32),
) {
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let (x_lo, x_hi) = (clip_x.0.max(0), clip_x.1.min(pw));
    let (ox, oy) = (origin.0 + img.left, origin.1 + img.top);
    let (iw, ih) = (img.pixmap.width() as i32, img.pixmap.height() as i32);
    let src = img.pixmap.data();
    let dst = pixmap.data_mut();
    for sy in 0..ih {
        let y = oy + sy;
        if y < 0 || y >= ph {
            continue;
        }
        for sx in 0..iw {
            let x = ox + sx;
            if x < x_lo || x >= x_hi {
                continue;
            }
            let s = &src[((sy * iw + sx) * 4) as usize..][..4];
            let k = u32::from(alpha);
            let sa = u32::from(s[3]) * k / 255;
            if sa == 0 {
                continue;
            }
            let d = &mut dst[((y * pw + x) * 4) as usize..][..4];
            let inv = 255 - sa;
            for c in 0..3 {
                d[c] = ((u32::from(s[c]) * k + 127) / 255 + (u32::from(d[c]) * inv + 127) / 255)
                    .min(255) as u8;
            }
            d[3] = (sa + (u32::from(d[3]) * inv + 127) / 255).min(255) as u8;
        }
    }
}

/// Underline and strikethrough lines of a run (set by `<u>`, `<s>` in markup).
pub(super) fn draw_decorations(
    pixmap: &mut Pixmap,
    run: &cosmic_text::LayoutRun,
    origin: (f32, f32),
    color: Color,
    clip_x: (i32, i32),
) {
    for d in run.decorations {
        let td = &d.data.text_decoration;
        let Some(first) = run.glyphs.get(d.glyph_range.start) else {
            continue;
        };
        let Some(last) = run.glyphs.get(d.glyph_range.end.saturating_sub(1)) else {
            continue;
        };
        let (x0, x1) = (
            (origin.0 + first.x).round() as i32,
            (origin.0 + last.x + last.w).round() as i32,
        );
        let lines = [
            (
                td.underline != cosmic_text::UnderlineStyle::None,
                d.data.underline_metrics,
            ),
            (td.strikethrough, d.data.strikethrough_metrics),
        ];
        for (on, m) in lines {
            if !on {
                continue;
            }
            let thickness = (m.thickness * d.font_size).round().max(1.0) as u32;
            // Font metrics give the offset above the baseline in em units.
            let y = (origin.1 - m.offset * d.font_size).round() as i32;
            let w = (x1 - x0).max(0) as u32;
            let mask = vec![255u8; (w * thickness) as usize];
            let c = td
                .underline_color_opt
                .map(|c| Color::rgba(c.r(), c.g(), c.b(), c.a()))
                .unwrap_or(color);
            blend_glyph(
                pixmap,
                (x0, y),
                (w, thickness),
                SwashContent::Mask,
                &mask,
                c,
                clip_x,
            );
        }
    }
}

pub(super) fn blend_glyph(
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
                    (s[0], s[1], s[2], s[3] as u32 * color.a as u32 / 255)
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
