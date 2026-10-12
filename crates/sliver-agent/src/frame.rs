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
    /// Cells in item order. Region `id` is the cell index + 1.
    pub cells: Vec<Cell>,
    pub layout: Layout,
}

impl Frame {
    pub fn cell_for_region(&self, id: u32) -> Option<&Cell> {
        self.cells.get((id as usize).checked_sub(1)?)
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
                            .id(c.css_id.clone());
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
) -> Frame {
    let mut image = Pixmap::new(w, h).expect("non-zero panel");
    let layout = renderer.render(scene, sheet, &mut image);
    let mut all_pressed = scene.clone();
    all_pressed.items_mut().for_each(|i| i.pressed = true);
    let mut pressed = Pixmap::new(w, h).expect("non-zero panel");
    renderer.render(&all_pressed, sheet, &mut pressed);
    let regions = regions(&layout, &cells, w, h);
    Frame {
        image,
        pressed,
        regions,
        cells,
        layout,
    }
}

/// Full-height regions for touchable cells, widened into neighbouring gaps (split in the
/// middle, at most [`REGION_SLOP`]) so they never overlap.
fn regions(layout: &Layout, cells: &[Cell], w: u32, h: u32) -> Vec<Region> {
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
            id: i as u32 + 1,
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
