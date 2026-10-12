use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::SwashContent;
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

use crate::icon::Image;
use crate::style::{Element, Style, Stylesheet};
use crate::text::{self, Label};
use crate::{Align, Color, Content, Fonts, Icon, Item, Rect, Scene, Sizing, Width, solve};

/// Items are never squeezed narrower than this on overflow unless they're naturally narrower
/// (ux.md §4: 60 px is the smallest usable touch target).
const OVERFLOW_MIN: f32 = 60.0;

/// Natural trough length when the stylesheet doesn't set `trough { min-width }`.
const SLIDER_MIN_TRACK: f32 = 160.0;

/// Slider trough: right of the icon (if any), the trough's `min-height` thick (default 6 px),
/// vertically centered, inset by its margins.
fn track_rect(item: Rect, s: &Style, trough: &Style, has_icon: bool) -> Rect {
    let content = item.inset(s.padding.map(|p| p + s.border_width));
    let icon = if has_icon {
        s.icon_size.min(content.h.max(item.h * 0.5)).floor() + s.gap
    } else {
        0.0
    };
    let h = if trough.min_height > 0.0 {
        trough.min_height
    } else {
        6.0
    };
    let r = Rect::new(
        content.x + icon,
        content.center().1 - h / 2.0,
        content.w - icon,
        h,
    );
    r.inset([0.0, trough.margin[1], 0.0, trough.margin[3]])
}

/// One in-between frame of a layer change: `to` holds the new layer's image and receives the
/// mix, `e` is the eased progress (0 = all old, 1 = all new), `dir` the slide direction (`1.0`:
/// the new layer comes from the right).
pub fn composite_layers(
    from: &Pixmap,
    to: &mut Pixmap,
    effect: crate::LayerEffect,
    e: f32,
    dir: f32,
) {
    use tiny_skia::{PixmapPaint, Transform};
    let new = to.clone();
    let id = Transform::identity();
    match effect {
        crate::LayerEffect::Slide => {
            let w = to.width() as f32;
            to.fill(tiny_skia::Color::BLACK);
            let old_x = (-dir * e * w).round() as i32;
            let new_x = (dir * (1.0 - e) * w).round() as i32;
            to.draw_pixmap(old_x, 0, from.as_ref(), &PixmapPaint::default(), id, None);
            to.draw_pixmap(new_x, 0, new.as_ref(), &PixmapPaint::default(), id, None);
        }
        crate::LayerEffect::Fade => {
            to.data_mut().copy_from_slice(from.data());
            let paint = PixmapPaint {
                opacity: e.clamp(0.0, 1.0),
                ..PixmapPaint::default()
            };
            to.draw_pixmap(0, 0, new.as_ref(), &paint, id, None);
        }
    }
}

/// Where everything in a scene goes. All rects are in bar pixels, rounded to whole pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub zones: Vec<Rect>,
    /// One per item, in [`Scene::items`] order. Use these for touch regions.
    pub items: Vec<Rect>,
    /// Per item: for sliders, the trough rect. Map a touch's x across it to the value.
    pub tracks: Vec<Option<Rect>>,
    /// The bar's `-sliver-layer-transition` (`None` with reduce-motion): what the caller
    /// should do when this scene replaces one of another layer.
    pub layer_transition: Option<crate::LayerTransition>,
}

/// Computed styles for one scene.
struct Styles {
    bar: Arc<Style>,
    zones: Vec<Arc<Style>>,
    items: Vec<Arc<Style>>,
    /// For sliders: trough, highlight, knob.
    parts: Vec<Option<[Arc<Style>; 3]>>,
}

/// Draws scenes. Holds font, glyph, label and style caches, so keep one around.
pub struct Renderer {
    anim: crate::anim::Animator,
    fonts: Fonts,
    styles: HashMap<String, Arc<Style>>,
    styles_generation: u64,
}

impl Renderer {
    pub fn new(fonts: Fonts) -> Renderer {
        Renderer {
            anim: Default::default(),
            fonts,
            styles: HashMap::new(),
            styles_generation: 0,
        }
    }

    fn compute_styles(&mut self, scene: &Scene, sheet: &Stylesheet) -> Styles {
        if self.styles_generation != sheet.generation() || self.styles.len() > 4096 {
            self.styles.clear();
            self.styles_generation = sheet.generation();
        }
        let bar_el = Element {
            tag: "bar",
            id: None,
            classes: &scene.classes,
            active: false,
            disabled: false,
            checked: false,
            first: true,
            last: true,
        };
        let mut key = format!("bar.{}", scene.classes.join("."));
        let bar = self.cached(&key, sheet, &[bar_el], None);
        let mut zones = Vec::new();
        let mut items = Vec::new();
        let mut parts = Vec::new();
        let nz = scene.zones.len();
        for (zi, zone) in scene.zones.iter().enumerate() {
            // A zone's name is a class (`.zone-main`, like waybar's `.modules-left`), so `#id`
            // always means a module.
            let mut zclasses = Vec::with_capacity(zone.classes.len() + 1);
            zclasses.extend(zone.name.iter().map(|n| format!("zone-{n}")));
            zclasses.extend(zone.classes.iter().cloned());
            let zone_el = Element {
                tag: "zone",
                id: None,
                classes: &zclasses,
                active: false,
                disabled: false,
                checked: false,
                first: zi == 0,
                last: zi + 1 == nz,
            };
            let zone_key = format!(
                "{key}>zone.{}:{}{}",
                zclasses.join("."),
                zone_el.first as u8,
                zone_el.last as u8
            );
            let zs = self.cached(&zone_key, sheet, &[bar_el, zone_el], Some(&bar));
            let ni = zone.items.len();
            for (ii, item) in zone.items.iter().enumerate() {
                // Waybar themes select every module with `.module`.
                let mut classes = Vec::with_capacity(item.classes.len() + 1);
                classes.push("module".to_string());
                classes.extend(item.classes.iter().cloned());
                let el = Element {
                    tag: "module",
                    id: item.id.as_deref(),
                    classes: &classes,
                    active: item.pressed,
                    disabled: item.disabled,
                    checked: item.checked,
                    first: ii == 0,
                    last: ii + 1 == ni,
                };
                key.clear();
                key.push_str(&zone_key);
                key.push_str(&format!(
                    ">module#{}.{}:{}{}{}{}{}",
                    item.id.as_deref().unwrap_or(""),
                    classes.join("."),
                    el.active as u8,
                    el.disabled as u8,
                    el.checked as u8,
                    el.first as u8,
                    el.last as u8
                ));
                let ms = self.cached(&key, sheet, &[bar_el, zone_el, el], Some(&zs));
                parts.push(if matches!(item.content, Content::Slider { .. }) {
                    let child = |tag| Element {
                        tag,
                        id: None,
                        classes: &[],
                        active: item.pressed,
                        disabled: item.disabled,
                        checked: item.checked,
                        first: true,
                        last: tag != "highlight",
                    };
                    let (trough, hl, knob) = (child("trough"), child("highlight"), child("slider"));
                    let tk = format!("{key}>trough");
                    let ts = self.cached(&tk, sheet, &[bar_el, zone_el, el, trough], Some(&ms));
                    let hs = self.cached(
                        &format!("{tk}>highlight"),
                        sheet,
                        &[bar_el, zone_el, el, trough, hl],
                        Some(&ts),
                    );
                    let ks = self.cached(
                        &format!("{tk}>slider"),
                        sheet,
                        &[bar_el, zone_el, el, trough, knob],
                        Some(&ts),
                    );
                    Some([ts, hs, ks])
                } else {
                    None
                });
                items.push(ms);
            }
            zones.push(zs);
            key = format!("bar.{}", scene.classes.join("."));
        }
        Styles {
            bar,
            zones,
            items,
            parts,
        }
    }

    fn cached(
        &mut self,
        key: &str,
        sheet: &Stylesheet,
        path: &[Element<'_>],
        parent: Option<&Style>,
    ) -> Arc<Style> {
        if let Some(s) = self.styles.get(key) {
            return s.clone();
        }
        let s = Arc::new(sheet.compute(path, parent));
        self.styles.insert(key.to_string(), s.clone());
        s
    }

    /// Where every zone and item goes on a `width` x `height` bar. Same geometry as
    /// [`Self::render`]. Takes `&mut self` because `Auto` widths measure text.
    pub fn layout(&mut self, scene: &Scene, sheet: &Stylesheet, width: u32, height: u32) -> Layout {
        let styles = self.compute_styles(scene, sheet);
        self.layout_styled(scene, &styles, width, height)
    }

    fn layout_styled(&mut self, scene: &Scene, st: &Styles, width: u32, height: u32) -> Layout {
        let bar = Rect::new(0.0, 0.0, width as f32, height as f32).inset(st.bar.padding);
        let mut item_sizes: Vec<Vec<Sizing>> = Vec::new();
        let mut zone_sizes = Vec::new();
        let mut styles = st.items.iter().zip(&st.parts);
        for (zone, zs) in scene.zones.iter().zip(&st.zones) {
            let sizes: Vec<Sizing> = zone
                .items
                .iter()
                .map(|item| {
                    let (s, parts) = styles.next().expect("one style per item");
                    let chrome = s.padding[1] + s.padding[3] + 2.0 * s.border_width;
                    let content = self.content_width(&item.content, s, parts.as_ref());
                    let natural = (content + chrome).max(s.min_width);
                    let margins = s.margin[1] + s.margin[3];
                    let fixed = |w: Width| match w {
                        Width::Fixed(px) => Width::Fixed(px + margins),
                        w => w,
                    };
                    Sizing {
                        width: fixed(item.width),
                        natural: natural + margins,
                        min: natural.min(OVERFLOW_MIN) + margins,
                    }
                })
                .collect();
            let gaps = zs.gap * sizes.len().saturating_sub(1) as f32;
            let chrome =
                zs.padding[1] + zs.padding[3] + 2.0 * zs.border_width + zs.margin[1] + zs.margin[3];
            zone_sizes.push(Sizing {
                width: match zone.width {
                    Width::Fixed(px) => Width::Fixed(px + zs.margin[1] + zs.margin[3]),
                    w => w,
                },
                natural: sizes.iter().map(|s| s.natural).sum::<f32>() + gaps + chrome,
                min: sizes.iter().map(|s| s.min).sum::<f32>() + gaps + chrome,
            });
            item_sizes.push(sizes);
        }
        let round = |r: Rect| {
            let (x0, x1) = (r.x.round(), r.right().round());
            let (y0, y1) = (r.y.round(), r.bottom().round());
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        };
        let mut zones = Vec::new();
        let mut items = Vec::new();
        let mut tracks = Vec::new();
        let mut styles = st.items.iter().zip(&st.parts).zip(scene.items());
        // Empty zones take no space and no gap (e.g. the `esc` zone on Macs with a physical Esc),
        // except `Fill` zones, which still take the leftover space and push later zones along.
        let takes_space =
            |z: &crate::Zone| !z.items.is_empty() || matches!(z.width, Width::Fill(_));
        let nonempty: Vec<Sizing> = zone_sizes
            .iter()
            .zip(&scene.zones)
            .filter(|(_, z)| takes_space(z))
            .map(|(s, _)| *s)
            .collect();
        let mut solved = solve(&nonempty, bar.w, st.bar.gap, Align::Start).into_iter();
        let mut last_end = 0.0;
        let zone_spans: Vec<(f32, f32)> = scene
            .zones
            .iter()
            .map(|z| {
                if !takes_space(z) {
                    (last_end, 0.0)
                } else {
                    let s = solved.next().unwrap_or((last_end, 0.0));
                    last_end = s.0 + s.1;
                    s
                }
            })
            .collect();
        for (((zone, zs), (zx, zw)), sizes) in scene
            .zones
            .iter()
            .zip(&st.zones)
            .zip(zone_spans)
            .zip(&item_sizes)
        {
            let outer = Rect::new(bar.x + zx, bar.y, zw, bar.h);
            let zrect = outer.inset([zs.margin[0], zs.margin[1], zs.margin[2], zs.margin[3]]);
            let inner = zrect.inset(zs.padding.map(|p| p + zs.border_width));
            for (ix, iw) in solve(sizes, inner.w, zs.gap, zone.align) {
                let ((s, parts), item) = styles.next().expect("one style per item");
                let r = round(Rect::new(inner.x + ix, inner.y, iw, inner.h).inset(s.margin));
                items.push(r);
                tracks.push(match (&item.content, parts) {
                    (Content::Slider { icon, .. }, Some([trough, ..])) => {
                        Some(track_rect(r, s, trough, icon.is_some()))
                    }
                    _ => None,
                });
            }
            zones.push(round(zrect));
        }
        Layout {
            zones,
            items,
            tracks,
            layer_transition: st.bar.layer_transition.filter(|_| !self.anim.reduce_motion),
        }
    }

    /// Width of an item's content (no padding) at its preferred size.
    fn content_width(
        &mut self,
        content: &Content,
        s: &Style,
        parts: Option<&[Arc<Style>; 3]>,
    ) -> f32 {
        let family = self.fonts.pick_family(&s.font_family);
        let mut label = |t: &str, markup: bool| {
            let shape = if markup {
                text::shape_markup
            } else {
                text::shape
            };
            shape(&mut self.fonts, t, &family, s.font_weight, s.font_size).width
        };
        match content {
            Content::Spacer | Content::Blank => 0.0,
            Content::Text(t) => label(t, false).ceil(),
            Content::Markup(t) => label(t, true).ceil(),
            Content::Icon(_) => s.icon_size,
            Content::IconText(_, t) => s.icon_size + s.gap + label(t, false).ceil(),
            Content::IconMarkup(_, t) => s.icon_size + s.gap + label(t, true).ceil(),
            Content::Slider { icon, .. } => {
                let trough =
                    parts.map_or(0.0, |p| p[0].min_width + p[0].margin[1] + p[0].margin[3]);
                let icon = if icon.is_some() {
                    s.icon_size + s.gap
                } else {
                    0.0
                };
                icon + trough.max(SLIDER_MIN_TRACK)
            }
        }
    }

    /// Draws the whole scene into `pixmap`, a landscape bar (e.g. 2008x60), and returns the
    /// layout it used.
    pub fn render(&mut self, scene: &Scene, sheet: &Stylesheet, pixmap: &mut Pixmap) -> Layout {
        self.render_at(scene, sheet, pixmap, std::time::Instant::now())
    }

    /// Forget transition state for every scene but these (the ones on screen now). Call it
    /// after drawing a set of scenes that replaced others (a layer change), or a fade left
    /// running on a scene that's no longer drawn would keep requesting frames.
    pub fn retain_scenes(&mut self, keep: &[&Scene]) {
        let ids: Vec<String> = keep.iter().map(|s| s.classes.join(".")).collect();
        self.anim.retain_scenes(&ids);
    }

    /// Honour the desktop's "reduce motion": every transition jumps to its end.
    pub fn set_reduce_motion(&mut self, on: bool) {
        self.anim.reduce_motion = on;
    }

    /// When the next frame is due for transitions that are still running, or `None` when
    /// everything is at rest (then there is nothing to redraw until something changes).
    pub fn animation_deadline(&self) -> Option<std::time::Instant> {
        self.anim.deadline()
    }

    /// Draws without transitions and without touching their state: for variants of a scene that
    /// must not animate (the "everything pressed" overlay sliverd swaps in).
    pub fn render_still(
        &mut self,
        scene: &Scene,
        sheet: &Stylesheet,
        pixmap: &mut Pixmap,
    ) -> Layout {
        let saved = std::mem::take(&mut self.anim);
        let layout = self.render_at(scene, sheet, pixmap, std::time::Instant::now());
        self.anim = saved;
        // (A fresh animator has no history, so everything draws at its target.)
        layout
    }

    /// [`Self::render`] at an explicit time, which is what `transition` interpolates by.
    /// Re-render at [`Self::animation_deadline`] until it is `None`.
    pub fn render_at(
        &mut self,
        scene: &Scene,
        sheet: &Stylesheet,
        pixmap: &mut Pixmap,
        now: std::time::Instant,
    ) -> Layout {
        let st = self.compute_styles(scene, sheet);
        let scene_id = scene.classes.join(".");
        self.anim.begin_frame(&scene_id);
        let layout = self.layout_styled(scene, &st, pixmap.width(), pixmap.height());
        // The panel has no alpha: whatever's transparent shows black.
        pixmap.fill(tiny_skia::Color::BLACK);
        let full = Rect::new(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32);
        let bar_opacity = st.bar.opacity;
        draw_box(pixmap, full, &st.bar, None, bar_opacity);
        let mut items = scene
            .items()
            .zip(&layout.items)
            .zip(&st.items)
            .zip(&st.parts)
            .zip(&layout.tracks);
        for ((zone, zs), zrect) in scene.zones.iter().zip(&st.zones).zip(&layout.zones) {
            let zone_name = zone.name.as_deref().unwrap_or("");
            let zone_opacity = bar_opacity * zs.opacity;
            let zrect = self.anim.geometry(
                &scene_id,
                &format!("{scene_id}|zone:{zone_name}"),
                *zrect,
                zs,
                now,
            );
            draw_box(pixmap, zrect, zs, None, zone_opacity);
            // Several items can share an id (an app's items, two clocks): number repeats so each
            // element keeps its own transition state.
            let mut seen_ids: HashMap<String, usize> = HashMap::new();
            for (ii, zone_item) in zone.items.iter().enumerate() {
                let ((((item, rect), s), parts), track) = items.next().expect("one rect per item");
                let id = zone_item.id.clone().unwrap_or_else(|| ii.to_string());
                let n = seen_ids.entry(id.clone()).or_insert(0);
                *n += 1;
                // Keyed per scene: the base and fn layers keep separate state.
                let key = format!("{scene_id}|{zone_name}/{id}#{n}");
                let animated = self.anim.apply(&scene_id, &key, s, now);
                let s = &animated;
                let opacity = zone_opacity * s.opacity;
                // A box gliding to its new place (`transition: width`): the trough keeps its
                // inset from the item's left edge and stretches with it.
                let drawn = self.anim.geometry(&scene_id, &key, *rect, s, now);
                self.draw_item(item, drawn, s, opacity, pixmap);
                if let (Content::Slider { value, .. }, Some(parts), Some(track)) =
                    (&item.content, parts, track)
                {
                    let track = Rect {
                        x: track.x + drawn.x - rect.x,
                        w: (track.w + drawn.w - rect.w).max(0.0),
                        ..*track
                    };
                    draw_slider(pixmap, track, *value, parts, opacity);
                }
            }
        }
        self.anim.end_frame(&scene_id);
        layout
    }

    /// Convenience: renders into a new `width` x `height` pixmap.
    pub fn render_new(
        &mut self,
        scene: &Scene,
        sheet: &Stylesheet,
        width: u32,
        height: u32,
    ) -> Pixmap {
        let mut pixmap = Pixmap::new(width, height).expect("non-zero bar size");
        self.render(scene, sheet, &mut pixmap);
        pixmap
    }

    fn draw_item(&mut self, item: &Item, rect: Rect, s: &Style, opacity: f32, pixmap: &mut Pixmap) {
        if matches!(item.content, Content::Spacer) || opacity <= 0.0 {
            return;
        }
        draw_box(pixmap, rect, s, item.background, opacity);
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
        match &item.content {
            Content::Spacer | Content::Blank => {}
            Content::Text(t) | Content::Markup(t) => {
                let markup = matches!(item.content, Content::Markup(_));
                let label = self.fit_label(t, markup, s, avail);
                let x = (cx - label.width / 2.0).round();
                self.draw_label(&label, x, cy, fg, clip, pixmap);
            }
            Content::Icon(icon) => {
                let x = (cx - icon_size / 2.0).round();
                let y = (cy - icon_size / 2.0).round();
                draw_icon(icon, x, y, icon_size, fg, opacity, pixmap);
            }
            Content::Slider { icon, .. } => {
                if let Some(icon) = icon {
                    let y = (cy - icon_size / 2.0).round();
                    draw_icon(icon, content.x.round(), y, icon_size, fg, opacity, pixmap);
                }
            }
            Content::IconText(icon, t) | Content::IconMarkup(icon, t) => {
                let markup = matches!(item.content, Content::IconMarkup(..));
                let label = self.fit_label(t, markup, s, avail - icon_size - s.gap);
                let total = icon_size + s.gap + label.width;
                let x = (cx - total / 2.0).round().max(content.x.floor());
                let y = (cy - icon_size / 2.0).round();
                draw_icon(icon, x, y, icon_size, fg, opacity, pixmap);
                self.draw_label(&label, x + icon_size + s.gap, cy, fg, clip, pixmap);
            }
        }
    }

    /// Shapes `text` at the style's size, shrinking it (down to half, at least 10 px) to fit
    /// `avail` pixels, then ellipsizing if it still doesn't fit.
    fn fit_label(&mut self, text: &str, markup: bool, s: &Style, avail: f32) -> Arc<Label> {
        let family = self.fonts.pick_family(&s.font_family);
        let (size, weight) = (s.font_size, s.font_weight);
        let min_size = (size * 0.5).max(10.0).min(size);
        let mut shape = |t: &str, size: f32| {
            let f = if markup {
                text::shape_markup
            } else {
                text::shape
            };
            f(&mut self.fonts, t, &family, weight, size)
        };
        let label = shape(text, size);
        if label.width <= avail {
            return label;
        }
        let size = (size * avail / label.width).floor().clamp(min_size, size);
        let label = shape(text, size);
        if label.width <= avail {
            return label;
        }
        // Longest prefix that fits with an ellipsis (binary search over char counts). Markup is
        // cut on its plain text so styles survive and tags are never split.
        let parsed = markup.then(|| crate::markup::parse(text));
        let chars: Vec<char> = text.chars().collect();
        let total = parsed.as_ref().map_or(chars.len(), |m| m.len_chars());
        let truncated = |n: usize| match &parsed {
            Some(m) => {
                let mut cut = m.truncated(n);
                if let Some(last) = cut.spans.last_mut() {
                    last.text = last.text.trim_end().to_owned();
                    last.text.push('…');
                }
                cut.to_markup()
            }
            None => chars[..n].iter().collect::<String>().trim_end().to_owned() + "…",
        };
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
    fn draw_label(
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

fn fade(c: Color, opacity: f32) -> Color {
    Color::rgba(
        c.r,
        c.g,
        c.b,
        (c.a as f32 * opacity.clamp(0.0, 1.0)).round() as u8,
    )
}

fn rounded_rect(r: Rect, radius: f32) -> Option<Path> {
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

/// Background and border of a bar, zone or module.
fn draw_box(pixmap: &mut Pixmap, r: Rect, s: &Style, bg_override: Option<Color>, opacity: f32) {
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
fn draw_slider(
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

fn draw_icon(icon: &Icon, x: f32, y: f32, size: f32, fg: Color, opacity: f32, pixmap: &mut Pixmap) {
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
    if !icon.symbolic && opacity >= 1.0 {
        draw(pixmap, x, y);
        return;
    }
    // Draw offscreen, then either recolor (symbolic: keep only coverage, paint in `fg`) or fade.
    let side = size.ceil() as u32;
    let Some(mut scratch) = Pixmap::new(side, side) else {
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
        None,
    );
}

/// Source-over blends a glyph image into the premultiplied RGBA pixmap.
/// Source-over a premultiplied colour bitmap at `origin` (+ its own offset), scaled by `alpha`.
fn blit_color(
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
fn draw_decorations(
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
