//! What covers the bar for a while: a button's menu or a bar-mode drawer's items. At most one
//! shows, on the base layer, in one zone that replaces every zone of the layout but the first (a
//! menu keeps the last one too when there are more than two, so the volume beside it stays).

use sliver_config::{Align, ResolvedZone, Width};

use super::*;

/// An open overlay.
#[derive(Debug, Clone)]
pub(super) enum Overlay {
    /// A `menu` sub-bar: its entries, and where the button that opened it sat among the zones
    /// the menu covers (offset from the menu's start, width), so the menu's ✕ takes its place.
    Menu {
        items: Vec<crate::act::MenuItem>,
        slot: Option<(f32, f32)>,
    },
    /// A bar-mode drawer's other modules: the group's key.
    Drawer(String),
}

/// The zone an overlay shows in, and its cells.
pub(super) struct Cover {
    pub zone: ResolvedZone,
    /// Keep the layout's last zone after it.
    pub keep_last: bool,
    pub cells: Vec<Cell>,
}

/// What `overlay` covers the base layer with, if anything.
pub(super) fn cover(
    overlay: Option<&Overlay>,
    zones: &[ResolvedZone],
    world: &World,
    placed: &modules::Placed,
    icons: &mut Icons,
    ui: &dyn Fn(&str) -> UiState,
) -> Option<Cover> {
    let cells = match overlay? {
        Overlay::Menu { items, slot } => modules::menu_cells(items, *slot, icons, ICON_LOOKUP_SIZE),
        // Looked up in the layout's zones: the drawer's group sits in one the overlay covers.
        Overlay::Drawer(key) => {
            let mut b = modules::Build {
                world,
                placed,
                icons,
                icon_size: ICON_LOOKUP_SIZE,
                ui,
            };
            modules::subbar_cells(zones, key, &mut b).unwrap_or_default()
        }
    };
    Some(Cover {
        // A menu's entries fit their labels, so the last zone can stay; a drawer's items take
        // the room.
        keep_last: matches!(overlay, Some(Overlay::Menu { .. })) && zones.len() > 2,
        zone: ResolvedZone {
            name: "menu".into(),
            width: Width::Fill(1),
            align: Align::Start,
            class: vec!["menu".into()],
            modules: Vec::new(),
        },
        cells,
    })
}

/// The zones the bar shows under `cover`, and where the cover's zone is among them.
pub(super) fn zones(
    layout: &[ResolvedZone],
    cover: Option<&Cover>,
) -> (Vec<ResolvedZone>, Option<usize>) {
    let Some(c) = cover.filter(|_| !layout.is_empty()) else {
        return (layout.to_vec(), None);
    };
    let mut out = vec![layout[0].clone(), c.zone.clone()];
    if c.keep_last {
        out.extend(layout.last().cloned());
    }
    (out, Some(1))
}
