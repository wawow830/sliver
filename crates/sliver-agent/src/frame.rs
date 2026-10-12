//! One layer's frame: resolved zones + cells → [`Scene`] → pixels (normal and all-pressed) and
//! touch regions for sliverd (api.md §6).

use sliver_config::{Align as CAlign, ResolvedZone, Width as CWidth};
use sliver_ipc::proto::{Rect as IpcRect, Region, RegionKind};
use sliver_render::{Align, Item, Layout, Pixmap, Renderer, Scene, Stylesheet, Width, Zone};

use crate::modules::{Cell, Touch};

/// Regions reach into the gap next to a button by at most this much, so near-misses still
/// count but empty bar space doesn't.
pub const REGION_SLOP: f32 = 12.0;

/// A rendered layer, ready to present.
#[derive(Debug, Clone)]
pub struct Frame {
    pub image: Pixmap,
    /// The same frame with every item drawn pressed (`:active`); sliverd blits from it.
    pub pressed: Pixmap,
    pub regions: Vec<Region>,
    /// Cells in item order. Region `id` is `id_base` + cell index + 1.
    pub cells: Vec<Cell>,
    pub layout: Layout,
    pub id_base: u32,
    /// What was drawn, so a press can re-render just the image ([`Frame::repress`]).
    pub scene: Scene,
}

impl Frame {
    /// The cell index for a region id of this frame.
    pub fn cell_index(&self, id: u32) -> Option<usize> {
        let i = id.checked_sub(self.id_base + 1)? as usize;
        (i < self.cells.len()).then_some(i)
    }
}

fn width(w: CWidth) -> Width {
    match w {
        CWidth::Auto => Width::Auto,
        CWidth::Fixed(px) => Width::Fixed(px as f32),
        CWidth::Fill(n) => Width::Fill(n),
    }
}

fn align(a: CAlign) -> Align {
    match a {
        CAlign::Start => Align::Start,
        CAlign::Center => Align::Center,
        CAlign::End => Align::End,
    }
}

/// The scene for `zones`, whose cells are `cells[i]` for zone `i`.
pub fn scene(zones: &[ResolvedZone], cells: &[Vec<Cell>], bar_classes: Vec<String>) -> Scene {
    Scene {
        classes: bar_classes,
        zones: zones
            .iter()
            .zip(cells)
            .map(|(z, cs)| Zone {
                name: Some(z.name.clone()),
                classes: z.class.clone(),
                width: width(z.width),
                align: align(z.align),
                items: cs
                    .iter()
                    .map(|c| {
                        let mut item = Item::new(c.content.clone())
                            .width(c.width)
                            .id(c.css_id.clone())
                            .pressed(c.pressed)
                            .checked(c.checked)
                            .disabled(c.disabled);
                        item.foreground = c.foreground;
                        item.background = c.background;
                        item.classes = c.classes.clone();
                        item
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Renders a layer at `w` x `h` and works out its touch regions.
pub fn render(
    renderer: &mut Renderer,
    sheet: &Stylesheet,
    scene: &Scene,
    cells: Vec<Cell>,
    w: u32,
    h: u32,
    id_base: u32,
) -> Frame {
    let mut image = Pixmap::new(w, h).expect("non-zero panel");
    let layout = renderer.render(scene, sheet, &mut image);
    let mut all_pressed = scene.clone();
    all_pressed.items_mut().for_each(|i| i.pressed = true);
    let mut pressed = Pixmap::new(w, h).expect("non-zero panel");
    // The pressed overlay must not animate (sliverd swaps it in instantly).
    renderer.render_still(&all_pressed, sheet, &mut pressed);
    let regions = regions(&layout, &cells, w, h, id_base);
    Frame {
        image,
        pressed,
        regions,
        cells,
        layout,
        id_base,
        scene: scene.clone(),
    }
}

/// A running layer change: the old image leaves while the new one arrives.
pub struct LayerAnim {
    pub from: Pixmap,
    pub start: std::time::Instant,
    pub t: sliver_render::LayerTransition,
    /// Slide direction: `1.0` forward (new from the right), `-1.0` back. Ignored for fades.
    pub dir: f32,
}

impl LayerAnim {
    /// Eased progress at `now`, `None` once it has finished.
    pub fn progress(&self, now: std::time::Instant) -> Option<f32> {
        let p =
            now.saturating_duration_since(self.start).as_secs_f32() / self.t.duration.as_secs_f32();
        (p < 1.0).then(|| self.t.timing.at(p))
    }

    /// Draws the in-between frame into `to` (which holds the new layer's image).
    pub fn composite(&self, to: &mut Pixmap, e: f32) {
        sliver_render::composite_layers(&self.from, to, self.t.effect, e, self.dir);
    }
}

impl Frame {
    /// Which layer and layout this frame shows (its `layer-*`/`layout-*` classes).
    pub fn identity(&self) -> (Option<&str>, Option<&str>) {
        let find = |p: &str| self.scene.classes.iter().find_map(|c| c.strip_prefix(p));
        (find("layer-"), find("layout-"))
    }

    /// Re-renders only the image with these cells pressed: the fast path for touch feedback on
    /// agent regions (regions, layout and the pressed twin can't change from a press). Returns
    /// whether anything changed.
    pub fn repress(
        &mut self,
        renderer: &mut Renderer,
        sheet: &Stylesheet,
        pressed: &dyn Fn(&Cell) -> bool,
    ) -> bool {
        let mut changed = false;
        for (item, cell) in self.scene.items_mut().zip(self.cells.iter_mut()) {
            let p = pressed(cell);
            if cell.pressed != p {
                cell.pressed = p;
                item.pressed = p;
                changed = true;
            }
        }
        if changed {
            renderer.render(&self.scene, sheet, &mut self.image);
        }
        changed
    }
}

/// Full-height regions for touchable cells, widened into neighbouring gaps (split in the
/// middle, at most [`REGION_SLOP`]) so they never overlap.
fn regions(layout: &Layout, cells: &[Cell], w: u32, h: u32, id_base: u32) -> Vec<Region> {
    let rects = &layout.items;
    let mut out = Vec::new();
    for (i, (r, c)) in rects.iter().zip(cells).enumerate() {
        let kind = match &c.touch {
            Touch::None => continue,
            Touch::Keys(keys) => RegionKind::Key { keys: keys.clone() },
            Touch::Agent => RegionKind::Agent,
        };
        if r.w <= 0.0 {
            continue;
        }
        let left = match i.checked_sub(1).map(|j| rects[j]) {
            Some(p) => {
                (r.x - ((r.x - p.right()) / 2.0).min(REGION_SLOP)).max((p.right() + r.x) / 2.0)
            }
            None => (r.x - REGION_SLOP).max(0.0),
        };
        let right = match rects.get(i + 1) {
            Some(n) => (r.right() + ((n.x - r.right()) / 2.0).min(REGION_SLOP))
                .min((r.right() + n.x) / 2.0),
            None => (r.right() + REGION_SLOP).min(w as f32),
        };
        let (x0, x1) = (left.ceil().max(0.0) as u32, (right.floor() as u32).min(w));
        if x1 <= x0 {
            continue;
        }
        out.push(Region {
            id: id_base + i as u32 + 1,
            rect: IpcRect {
                x: x0,
                y: 0,
                w: x1 - x0,
                h,
            },
            kind,
        });
    }
    out
}
