use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::SwashContent;
use tiny_skia::{FillRule, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

use crate::icon::Image;
use crate::style::{Element, Style, Stylesheet};
use crate::text::{self, Label};
use crate::{Align, Color, Content, Fonts, Icon, Item, Rect, Scene, Sizing, Width, solve};

mod layout;
mod paint;
mod style;

use paint::*;

/// Items are never squeezed narrower than this on overflow unless they're naturally narrower
/// (60 px is the smallest usable touch target).
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
    /// Per zone: its items wanted more room than it got, so some were squeezed.
    pub overflow: Vec<bool>,
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
    /// For items with a spectrum: its `spectrum` child node.
    spectra: Vec<Option<Arc<Style>>>,
}

/// Draws scenes. Holds font, glyph, label and style caches, so keep one around.
pub struct Renderer {
    anim: crate::anim::Animator,
    fonts: Fonts,
    styles: HashMap<String, Arc<Style>>,
    styles_generation: u64,
    /// Extra `[top, right, bottom, left]` inset on top of the bar's padding (pixel shift keeps
    /// content clear of the edges it moves towards).
    extra_inset: [f32; 4],
}

impl Renderer {
    pub fn new(fonts: Fonts) -> Renderer {
        Renderer {
            anim: Default::default(),
            fonts,
            styles: HashMap::new(),
            styles_generation: 0,
            extra_inset: [0.0; 4],
        }
    }

    /// Lays everything out `[top, right, bottom, left]` px further in than the stylesheet says.
    /// Used while pixel shift is on, so the shifted content never runs off the panel.
    pub fn set_extra_inset(&mut self, inset: [f32; 4]) {
        self.extra_inset = inset;
    }

    pub fn extra_inset(&self) -> [f32; 4] {
        self.extra_inset
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
            .zip(&layout.tracks)
            .zip(&st.spectra);
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
                let (((((item, rect), s), parts), track), spectrum) =
                    items.next().expect("one rect per item");
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
                let track = track.map(|t| Rect {
                    x: t.x + drawn.x - rect.x,
                    w: (t.w + drawn.w - rect.w).max(0.0),
                    ..t
                });
                let spectrum = spectrum.as_ref().map(|ss| {
                    // Across the content height: along a slider's rail, else the content box.
                    let content = drawn.inset(s.padding.map(|p| p + s.border_width));
                    let area = match track {
                        Some(t) => Rect {
                            y: content.y,
                            h: content.h,
                            ..t
                        },
                        None => content,
                    };
                    // On a slider, the bars up to its value take the highlight's colour, so the
                    // spectrum can be the rail itself (with a trough of no height).
                    let played = match (&item.content, parts, track) {
                        (Content::Slider { value, .. }, Some([t, hl, _]), Some(tr)) => Some((
                            tr.x + tr.w * value.clamp(0.0, 1.0),
                            fade(hl.background, opacity * t.opacity * hl.opacity),
                        )),
                        _ => None,
                    };
                    // Its own padding narrows it: a band under a button's icon, say.
                    Bars {
                        area: area.inset(ss.padding),
                        color: fade(ss.color, opacity * ss.opacity),
                        played,
                    }
                });
                self.draw_item(item, drawn, s, opacity, spectrum, pixmap);
                if let (Content::Slider { value, .. }, Some(parts), Some(track)) =
                    (&item.content, parts, track)
                {
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
}
