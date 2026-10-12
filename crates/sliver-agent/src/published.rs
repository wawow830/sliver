//! Items that apps publish over the public D-Bus API (api.md §3) as bar cells: the App controls
//! and Status items placeholders (customization.md §5) fill from here.

use sliver_api::item::{Item, ItemKind, SpacerSize};
use sliver_api::registry::ContextId;
use sliver_api::service::Api;
use sliver_render::{Color, Content, Icon, Width};

use crate::icons::Icons;
use crate::modules::{Cell, Touch};

/// One published context, copied out of the registry for this frame.
#[derive(Debug, Clone)]
pub struct PubContext {
    pub id: ContextId,
    pub provider: String,
    pub items: Vec<Item>,
}

/// Who fills the placeholders right now.
#[derive(Debug, Clone, Default)]
pub struct Published {
    pub app: Option<PubContext>,
    /// The app context may cover Main + Controls (api.md §4).
    pub modal: bool,
    pub status: Vec<PubContext>,
}

impl Published {
    /// What the API's registry says, for the current focus.
    pub fn snapshot(api: &Api) -> Published {
        let r = api.resolve();
        let st = api.state();
        let get = |id: ContextId| {
            st.reg.context(id).map(|c| PubContext {
                id,
                provider: c.provider.clone(),
                items: c.items.clone(),
            })
        };
        Published {
            app: r.app.and_then(get),
            modal: r.modal,
            status: r.status.into_iter().filter_map(get).collect(),
        }
    }

    pub fn contexts(&self) -> impl Iterator<Item = &PubContext> {
        self.app.iter().chain(&self.status)
    }
}

/// Which published item a cell shows, and how a touch on it reaches the app.
#[derive(Debug, Clone, PartialEq)]
pub struct PubRef {
    pub context: ContextId,
    pub item: String,
    pub kind: PubKind,
    /// Gestures beyond tap the item opted into.
    pub gestures: Vec<sliver_api::item::Gesture>,
    /// The item's `priority` (a group child has its group's): lower gives way first when its
    /// zone overflows (NSTouchBar visibilityPriority).
    pub priority: i32,
    /// What overflows as a unit: the top-level item's id (a group's children go together).
    pub unit: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PubKind {
    Button,
    Toggle,
    /// Slider range, to turn a 0–100 position back into the app's units.
    Slider {
        min: f64,
        max: f64,
        step: f64,
    },
    /// One segment of a segmented control.
    Segment(i32),
    /// One visible scrubber candidate: tap picks `index`; a swipe scrolls to `prev`/`next`
    /// (the offsets one page back/forward).
    Candidate {
        index: i32,
        prev: i32,
        next: i32,
    },
    /// A scrubber's ‹ / › arrow: scroll to this offset.
    ScrollTo(i32),
    /// A popover item: a tap opens its items full-width (NSPopoverTouchBarItem).
    Popover,
    /// The ✕ that closes an open popover (`item` is the popover's id).
    PopoverClose,
    /// The ✕ that closes a modal context.
    Close,
}

/// Widths of `spacer` items (`size = small / large`), in px.
const SPACER_SMALL: f32 = 16.0;
/// At most this many published items are dropped from one frame for overflow.
pub const MAX_OVERFLOW_DROPS: usize = 32;
const SPACER_LARGE: f32 = 64.0;

/// Which published item (zone index, context, unit) gives way next: in an overflowing zone, the
/// lowest-priority published item, if its priority is below another cell's there (built-in
/// modules count as 0). All-equal priorities never drop anything; they squeeze as before.
pub fn overflow_victim(
    zones: &[Vec<Cell>],
    overflow: &[bool],
) -> Option<(usize, ContextId, String)> {
    let prio = |c: &Cell| c.published.as_ref().map_or(0, |p| p.priority);
    for (zi, cells) in zones.iter().enumerate() {
        if !overflow.get(zi).copied().unwrap_or(false) {
            continue;
        }
        // ✕ cells are pinned (`i32::MAX`), not a reason to drop anything.
        let Some(top) = cells.iter().map(prio).filter(|p| *p != i32::MAX).max() else {
            continue;
        };
        let victim = cells
            .iter()
            .rev()
            .filter_map(|c| c.published.as_ref())
            .filter(|p| p.priority < top)
            .min_by_key(|p| p.priority);
        if let Some(p) = victim {
            return Some((zi, p.context, p.unit.clone()));
        }
    }
    None
}

fn color(s: &Option<String>) -> Option<Color> {
    let c = sliver_render::markup::color(s.as_deref()?)?;
    Some(Color::rgba(c[0], c[1], c[2], c[3]))
}

/// The ✕ at the end of a modal context (api.md §4).
pub fn close_cell(ctx: &PubContext, icons: &mut Icons, icon_size: u32) -> Cell {
    let icon = icons.get("window-close-symbolic", None, icon_size);
    Cell {
        key: format!("modal/{}#close", ctx.provider),
        id: format!("app/{}#close", ctx.provider),
        kind: "app".into(),
        mic: false,
        css_id: "modal-close".into(),
        classes: vec!["app".into(), "modal-close".into()],
        width: Width::Auto,
        content: icon.map_or_else(|| Content::Text("×".into()), Content::Icon),
        touch: Touch::Agent,
        on: Vec::new(),
        next_change: None,
        pressed: false,
        checked: false,
        disabled: false,
        drawer: None,
        foreground: None,
        background: None,
        published: Some(PubRef {
            context: ctx.id,
            item: String::new(),
            kind: PubKind::Close,
            gestures: Vec::new(),
            priority: i32::MAX,
            unit: String::new(),
        }),
        pass_through: false,
        target: None,
        timer_total: None,
        menu: Vec::new(),
    }
}

/// An open popover: its items as cells (`class` is the context's, `app` or `status`), then a ✕.
/// `None` when the popover is gone (the context or item was removed): the caller closes it.
pub fn popover_cells(
    ctx: &PubContext,
    popover: &str,
    class: &str,
    icons: &mut Icons,
    icon_size: u32,
) -> Option<Vec<Cell>> {
    let item = ctx.items.iter().find(|i| i.id == popover && i.visible)?;
    let ItemKind::Popover { items } = &item.kind else {
        return None;
    };
    let mut out = Vec::new();
    for child in items {
        push(
            ctx,
            child,
            None,
            class,
            class == "app",
            icons,
            icon_size,
            &mut out,
        );
    }
    for c in &mut out {
        c.classes.push("popover-item".into());
    }
    let mut close = close_cell(ctx, icons, icon_size);
    close.key = format!("popover/{}#{popover}.close", ctx.provider);
    close.id = format!("app/{}#{popover}.close", ctx.provider);
    close.css_id = "popover-close".into();
    close.classes = vec![class.to_owned(), "popover-close".into()];
    close.published = Some(PubRef {
        context: ctx.id,
        item: popover.to_owned(),
        kind: PubKind::PopoverClose,
        gestures: Vec::new(),
        priority: i32::MAX,
        unit: String::new(),
    });
    out.push(close);
    Some(out)
}

/// Cells for a context's items. `class` is `app` or `status`; `focused` adds `.focused-app`.
pub fn cells(
    ctx: &PubContext,
    class: &str,
    focused: bool,
    icons: &mut Icons,
    icon_size: u32,
    out: &mut Vec<Cell>,
) {
    for item in &ctx.items {
        push(ctx, item, None, class, focused, icons, icon_size, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn push(
    ctx: &PubContext,
    item: &Item,
    group: Option<&Item>,
    class: &str,
    focused: bool,
    icons: &mut Icons,
    icon_size: u32,
    out: &mut Vec<Cell>,
) {
    if !item.visible {
        return;
    }
    // `icon-data` is PNG or SVG bytes (api.md §3).
    let icon = item
        .icon_data
        .as_deref()
        .and_then(|data| Icon::from_png(data).or_else(|_| Icon::from_svg(data)).ok())
        .or_else(|| {
            item.icon
                .as_deref()
                .and_then(|n| icons.get(n, None, icon_size))
        });
    let label = item.label.clone().unwrap_or_default();
    let label_content = |icon: Option<Icon>, text: String| match (icon, text.is_empty()) {
        (Some(i), true) => Content::Icon(i),
        (Some(i), false) => Content::IconText(i, text),
        (None, false) => Content::Text(text),
        (None, true) => Content::Blank,
    };
    let mut classes = vec![class.to_owned(), item.kind.type_name().to_owned()];
    if focused {
        classes.push("focused-app".into());
    }
    classes.extend(item.classes.iter().cloned());
    let base = |content: Content, kind: Option<PubKind>, checked: bool, id: &str| Cell {
        key: format!("{class}/{}#{id}", ctx.provider),
        id: format!("app/{}#{id}", ctx.provider),
        kind: class.into(),
        mic: false,
        css_id: format!(
            "{class}-{}",
            ctx.provider.replace(['.', '/'], "-").to_ascii_lowercase()
        ),
        classes: classes.clone(),
        width: match (&item.kind, item.min_width, item.max_width) {
            (ItemKind::Spacer { size }, ..) => match size {
                SpacerSize::Flexible => Width::Fill(1),
                SpacerSize::Small => Width::Fixed(SPACER_SMALL),
                SpacerSize::Large => Width::Fixed(SPACER_LARGE),
            },
            (_, None, None) => Width::Auto,
            // `width = {min, max}`: fit the content within these bounds.
            (_, lo, hi) => Width::Between(
                lo.unwrap_or(0) as f32,
                hi.map_or(f32::INFINITY, |h| h as f32),
            ),
        },
        content,
        touch: if kind.is_some() && item.enabled {
            Touch::Agent
        } else {
            Touch::None
        },
        on: Vec::new(),
        next_change: None,
        pressed: false,
        checked,
        disabled: !item.enabled,
        drawer: None,
        foreground: color(&item.fg),
        background: color(&item.bg),
        published: kind.map(|kind| PubRef {
            context: ctx.id,
            item: item.id.clone(),
            kind,
            gestures: item.gestures.clone(),
            priority: group.map_or(item.priority, |g| g.priority),
            unit: group.map_or(&item.id, |g| &g.id).clone(),
        }),
        pass_through: false,
        target: None,
        timer_total: None,
        menu: Vec::new(),
    };
    match &item.kind {
        ItemKind::Button => out.push(base(
            label_content(icon, label),
            Some(PubKind::Button),
            false,
            &item.id,
        )),
        ItemKind::Popover { .. } => out.push(base(
            label_content(icon, label),
            Some(PubKind::Popover),
            false,
            &item.id,
        )),
        ItemKind::Toggle { state } => out.push(base(
            label_content(icon, label),
            Some(PubKind::Toggle),
            *state,
            &item.id,
        )),
        ItemKind::Label => out.push(base(label_content(icon, label), None, false, &item.id)),
        ItemKind::Slider {
            value,
            min,
            max,
            step,
        } => {
            let v = if max > min {
                ((value - min) / (max - min)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            out.push(base(
                Content::Slider {
                    icon,
                    value: v as f32,
                },
                Some(PubKind::Slider {
                    min: *min,
                    max: *max,
                    step: *step,
                }),
                false,
                &item.id,
            ));
        }
        ItemKind::Progress {
            value,
            indeterminate,
        } => {
            // No idle animations (power invariant): indeterminate is a full track with the
            // `indeterminate` class, which themes draw muted.
            let mut c = base(
                Content::Slider {
                    icon,
                    value: if *indeterminate {
                        1.0
                    } else {
                        value.clamp(0.0, 1.0) as f32
                    },
                },
                None,
                false,
                &item.id,
            );
            if *indeterminate {
                c.classes.push("indeterminate".into());
            }
            out.push(c);
        }
        ItemKind::Spacer { .. } => out.push(base(Content::Spacer, None, false, &item.id)),
        ItemKind::Segmented { segments, selected } => {
            for (i, seg) in segments.iter().enumerate() {
                let icon = seg
                    .icon
                    .as_deref()
                    .and_then(|n| icons.get(n, None, icon_size));
                let content = label_content(icon, seg.label.clone().unwrap_or_default());
                out.push(base(
                    content,
                    Some(PubKind::Segment(i as i32)),
                    *selected == i as i32,
                    &format!("{}.{i}", item.id),
                ));
            }
        }
        ItemKind::Scrubber {
            candidates,
            selected,
            offset,
            visible_count,
        } => {
            let page = (*visible_count).max(1) as i32;
            let max = sliver_api::item::scrubber_max_offset(candidates.len(), *visible_count);
            let offset = (*offset).clamp(0, max);
            let (prev, next) = ((offset - page).max(0), (offset + page).min(max));
            let arrow = |text: &str, to: i32, which: &str| {
                let mut c = base(
                    Content::Text(text.into()),
                    Some(PubKind::ScrollTo(to)),
                    false,
                    &format!("{}.{which}", item.id),
                );
                c.classes.push(format!("scrubber-{which}"));
                c
            };
            if offset > 0 {
                out.push(arrow("‹", prev, "prev"));
            }
            for (i, cand) in candidates
                .iter()
                .enumerate()
                .skip(offset as usize)
                .take(page as usize)
            {
                let icon = cand
                    .icon
                    .as_deref()
                    .and_then(|n| icons.get(n, None, icon_size));
                let mut c = base(
                    label_content(icon, cand.label.clone().unwrap_or_default()),
                    Some(PubKind::Candidate {
                        index: i as i32,
                        prev,
                        next,
                    }),
                    *selected == i as i32,
                    &format!("{}.{i}", item.id),
                );
                c.classes.push("scrubber-candidate".into());
                out.push(c);
            }
            if offset < max {
                out.push(arrow("›", next, "next"));
            }
        }
        ItemKind::Group { items } => {
            for child in items {
                push(
                    ctx,
                    child,
                    Some(item),
                    class,
                    focused,
                    icons,
                    icon_size,
                    out,
                );
            }
        }
    }
}
