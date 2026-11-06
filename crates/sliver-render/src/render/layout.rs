//! Layout: where each item goes on the bar, and how wide it is.

use super::*;

impl Renderer {
    /// Where every zone and item goes on a `width` x `height` bar. Same geometry as
    /// [`Self::render`]. Takes `&mut self` because `Auto` widths measure text.
    pub fn layout(&mut self, scene: &Scene, sheet: &Stylesheet, width: u32, height: u32) -> Layout {
        let styles = self.compute_styles(scene, sheet);
        self.layout_styled(scene, &styles, width, height)
    }

    pub(super) fn layout_styled(
        &mut self,
        scene: &Scene,
        st: &Styles,
        width: u32,
        height: u32,
    ) -> Layout {
        let bar = Rect::new(0.0, 0.0, width as f32, height as f32)
            .inset(st.bar.padding)
            .inset(self.extra_inset);
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
                    let content_h = bar.h
                        - (zs.margin[0] + zs.margin[2] + zs.padding[0] + zs.padding[2])
                        - 2.0 * zs.border_width
                        - (s.margin[0] + s.margin[2] + s.padding[0] + s.padding[2])
                        - 2.0 * s.border_width;
                    let content = self.content_width(&item.content, s, parts.as_ref(), content_h);
                    let mut natural = (content + chrome).max(s.min_width);
                    let margins = s.margin[1] + s.margin[3];
                    let width = match item.width {
                        Width::Fixed(px) => Width::Fixed(px + margins),
                        Width::Between(lo, hi) => {
                            natural = natural.max(lo).min(hi.max(lo));
                            Width::Auto
                        }
                        w => w,
                    };
                    Sizing {
                        width,
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
                // A fixed-width item counts as its width (its `natural` is only its content).
                natural: sizes
                    .iter()
                    .map(|s| match s.width {
                        Width::Fixed(px) => px,
                        _ => s.natural,
                    })
                    .sum::<f32>()
                    + gaps
                    + chrome,
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
        let mut overflow = Vec::new();
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
            // What the items ask for: a fixed width counts as itself (its `natural` is only its
            // content), so squeezed fixed-width items are reported too.
            let wanted = sizes
                .iter()
                .map(|s| match s.width {
                    Width::Fixed(px) => px.max(s.min),
                    _ => s.natural,
                })
                .sum::<f32>()
                + zs.gap * sizes.len().saturating_sub(1) as f32;
            overflow.push(!sizes.is_empty() && wanted > inner.w + 0.5);
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
            overflow,
            layer_transition: st.bar.layer_transition.filter(|_| !self.anim.reduce_motion),
        }
    }

    /// Width of an item's content (no padding) at its preferred size.
    pub(super) fn content_width(
        &mut self,
        content: &Content,
        s: &Style,
        parts: Option<&[Arc<Style>; 3]>,
        content_h: f32,
    ) -> f32 {
        let family = self.fonts.pick_family(&s.font_family);
        let mut label =
            |t: &str| text::shape(&mut self.fonts, t, &family, s.font_weight, s.font_size).width;
        match content {
            Content::Spacer => 0.0,
            Content::Label { icon, text } => match (icon, text.is_empty()) {
                (None, true) => 0.0,
                (None, false) => label(text).ceil(),
                (Some(_), true) => s.icon_size,
                (Some(_), false) => s.icon_size + s.gap + label(text).ceil(),
            },
            // Wide enough to read a trend; `width`/`min-width` make it wider.
            Content::Graph { .. } => GRAPH_WIDTH,
            Content::Bars(levels) => levels.len() as f32 * BAR_PITCH,
            // As tall as the item's content box, as wide as its aspect ratio makes it.
            Content::Image(icon) => match icon.size() {
                (w, h) if w > 0.0 && h > 0.0 => (content_h.max(0.0) * w / h).ceil(),
                _ => 0.0,
            },
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
}
