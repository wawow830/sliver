//! What covers the bar for a while: a button's menu, a bar-mode drawer's items, a popover an
//! app showed, or a modal app context. At most one shows, on the base layer, in one zone that
//! replaces every zone of the layout but the first (a menu keeps the last one too when there
//! are more than two, so the volume beside it stays). A popover wins over a modal context,
//! which wins over a menu or drawer.

use sliver_api::registry::ContextId;
use sliver_config::{Align, ResolvedZone, Width};

use super::*;

/// An open overlay the user (or an app) asked for. A modal context isn't one: it shows while
/// the world says so.
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
    /// A published popover: its context, item id and the context's class (`app` / `status`).
    Popover {
        context: ContextId,
        item: String,
        class: String,
    },
}

/// The zone an overlay shows in, and its cells.
pub(super) struct Cover {
    pub zone: ResolvedZone,
    /// Keep the layout's last zone after it.
    pub keep_last: bool,
    pub modal: bool,
    pub cells: Vec<Cell>,
}

/// What covers the base layer now, if anything. A popover whose context or item went away
/// (the app closed, focus moved) is closed here.
pub(super) fn cover(
    overlay: &mut Option<Overlay>,
    zones: &[ResolvedZone],
    world: &World,
    placed: &modules::Placed,
    icons: &mut Icons,
    ui: &dyn Fn(&str) -> UiState,
) -> Option<Cover> {
    let zone = |name: &str, class: &[&str]| ResolvedZone {
        name: name.into(),
        width: Width::Fill(1),
        align: Align::Start,
        class: class.iter().map(|c| c.to_string()).collect(),
        modules: Vec::new(),
    };
    if let Some(Overlay::Popover {
        context,
        item,
        class,
    }) = overlay
    {
        let cells = world
            .published
            .contexts()
            .find(|c| c.id == *context)
            .and_then(|c| crate::published::popover_cells(c, item, class, icons, ICON_LOOKUP_SIZE));
        match cells {
            Some(cells) => {
                return Some(Cover {
                    zone: zone("popover", &["menu", "popover"]),
                    keep_last: false,
                    modal: false,
                    cells,
                });
            }
            None => *overlay = None,
        }
    }
    if world.published.modal
        && let Some(ctx) = &world.published.app
    {
        let mut cells = Vec::new();
        crate::published::cells(ctx, "app", icons, ICON_LOOKUP_SIZE, &|_| false, &mut cells);
        cells.push(crate::published::close_cell(ctx, icons, ICON_LOOKUP_SIZE));
        return Some(Cover {
            zone: zone("modal", &["modal"]),
            keep_last: false,
            modal: true,
            cells,
        });
    }
    let cells = match overlay.as_ref()? {
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
        Overlay::Popover { .. } => return None,
    };
    Some(Cover {
        // A menu's entries fit their labels, so the last zone can stay; a drawer's items take
        // the room.
        keep_last: matches!(overlay, Some(Overlay::Menu { .. })) && zones.len() > 2,
        zone: zone("menu", &["menu"]),
        modal: false,
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
