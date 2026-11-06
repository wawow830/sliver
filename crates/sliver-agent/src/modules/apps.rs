//! What apps publish, on the bar: App controls and Status items placeholders, and items a
//! layout places itself (`app/<provider>#<item>`), with the module's config on top.

use super::*;

/// Just the user's `on-*` bindings: what a placeholder (or a configured published item) adds
/// to the app's own cells.
pub(super) fn user_bindings(m: &ModuleInstance) -> Vec<(Gesture, Do)> {
    let kind = m.id.kind.as_str();
    m.common
        .on
        .iter()
        .map(|(g, a)| (*g, Do::bind(a, kind, &[])))
        .collect()
}

/// The cells of a pinned `app/<provider>[#item]` module: the app's own cells (touches still go
/// to the app), with the module's classes, `on-*` bindings (`pass-through`), `width` and
/// `format` (over the item's values) applied on top. Nothing while it isn't published; an
/// outlined placeholder in the settings preview.
pub(super) fn pinned_app(m: &ModuleInstance, key: &str, b: &mut Build, out: &mut Vec<Cell>) {
    let (world, icons, icon_size) = (b.world, &mut *b.icons, b.icon_size);
    let Some(provider) = m.id.name.as_deref() else {
        return;
    };
    let item = m.id.instance.as_deref();
    let Some((ctx, class)) = world.published.find(provider) else {
        if world.preview {
            // Short: the item id (`timer`), else the provider's last segment (`Pomodoro`).
            let label = m.id.instance.clone().unwrap_or_else(|| {
                let p = m.id.name.as_deref().unwrap_or("app");
                p.rsplit('.').next().unwrap_or(p).to_owned()
            });
            out.push(placeholder(m, key, &label, "app"));
        }
        return;
    };
    let start = out.len();
    crate::published::pinned_cells(ctx, item, class, icons, icon_size, out);
    let cells: Vec<Cell> = out.drain(start..).collect();
    for cell in cells {
        out.extend(decorate_published(m, ctx, cell, icons, icon_size));
    }
}

/// A published item's cell with a module's config on top: classes, `on-*` bindings
/// (`pass-through`), `width`, and `format`/`format-icons`/`states` over the item's values
/// (sliders keep their track). `None` when the format hides it.
pub(super) fn decorate_published(
    m: &ModuleInstance,
    ctx: &crate::published::PubContext,
    mut cell: Cell,
    icons: &mut Icons,
    icon_size: u32,
) -> Option<Cell> {
    let c = &m.common;
    cell.look.classes.extend(m.classes.iter().cloned());
    if !c.on.is_empty() {
        cell.on = user_bindings(m);
        if let Some(p) = &mut cell.published {
            p.pass_through = c.pass_through;
        }
    }
    if c.width != sliver_config::Width::Auto {
        cell.look.width = width(c.width);
    }
    let it = cell.published.as_ref().and_then(|p| ctx.item(&p.item));
    if (c.format.is_some() || c.format_alt.is_some() || c.format_icons.is_some())
        && let Some(it) = it
        && !matches!(cell.look.content, Content::Slider { .. })
    {
        let (values, pct) = crate::published::item_values(it);
        let st = State {
            percentage: pct,
            ..State::default()
        };
        let f = present(c, "{text}", &st, &values, None, icons, icon_size)?;
        cell.look.classes.extend(f.classes);
        let own = match &cell.look.content {
            Content::Label { icon, .. } => icon.clone(),
            _ => None,
        };
        cell.look.content = Content::label(f.icon.or(own), f.text);
    }
    Some(cell)
}

/// An empty App controls / Status items placeholder, drawn only in previews. It takes the
/// module's `width`, so the settings editor's resize handle shows what it does.
pub(super) fn placeholder(m: &ModuleInstance, key: &str, label: &str, class: &str) -> Cell {
    let mut c = Cell::new(
        key,
        m.id.to_string(),
        m.id.kind.clone(),
        m.css_id.clone(),
        Content::text(label),
    );
    c.look.classes = vec!["placeholder".into(), class.into()];
    c.look.width = width(m.common.width);
    c
}

/// App controls / Status items: the items apps publish for it (the focused app's, or the status
/// contexts'), with the placeholder module's classes, bindings and `width` on them; an outlined
/// placeholder in previews while there are none.
pub(super) fn placeholder_cells(
    m: &ModuleInstance,
    key: &str,
    prefix: &str,
    b: &mut Build,
    out: &mut Vec<Cell>,
) {
    let (world, icons, icon_size) = (b.world, &mut *b.icons, b.icon_size);
    let start = out.len();
    let p = &world.published;
    if m.id.kind == "app-controls" {
        if let Some(ctx) = &p.app {
            let skip = |id: &str| p.is_pinned(&ctx.provider, id);
            crate::published::cells(ctx, "app", icons, icon_size, &skip, out);
        }
    } else {
        for ctx in &p.status {
            let skip = |id: &str| p.is_pinned(&ctx.provider, id);
            crate::published::cells(ctx, "status", icons, icon_size, &skip, out);
        }
    }
    let class = if m.id.kind == "app-controls" {
        "app"
    } else {
        "status"
    };
    let placeholder_shown = out.len() == start && world.preview;
    if placeholder_shown {
        let label = if class == "app" {
            "App controls"
        } else {
            "Status items"
        };
        out.push(placeholder(m, key, label, class));
    }
    // The placeholder's own classes and user bindings apply to every item in it
    // (a user `on-*` replaces the app's event unless `pass-through`).
    let on = user_bindings(m);
    for c in &mut out[start..] {
        c.look.classes.extend(m.classes.iter().cloned());
        c.on = on.clone();
        if let Some(p) = &mut c.published {
            p.pass_through = m.common.pass_through;
        }
        c.key = format!("{prefix}{}", c.key);
    }
    // `[modules."app/<provider>#<item>"]` applies here too.
    if !world.published.configured.is_empty() {
        let cells: Vec<Cell> = out.drain(start..).collect();
        for cell in cells {
            let conf = world.published.configured.get(&cell.id);
            let ctx = cell
                .id
                .strip_prefix("app/")
                .and_then(|r| r.split('#').next())
                .and_then(|p| world.published.find(p))
                .map(|(c, _)| c);
            match (conf, ctx) {
                (Some(conf), Some(ctx)) => {
                    out.extend(decorate_published(conf, ctx, cell, icons, icon_size))
                }
                _ => out.push(cell),
            }
        }
    }
    // A sized slot (`width` = px or fill): the items lay out inside it and a trailing
    // spacer reserves the rest. The agent measures a px slot and trims the spacer, or
    // drops the lowest-priority items if they don't fit (`Agent::fit_slots`).
    if !placeholder_shown && m.common.width != sliver_config::Width::Auto {
        let slot = key.to_owned();
        for c in &mut out[start..] {
            c.slot = Some(slot.clone());
        }
        let mut spacer = placeholder(m, &format!("{slot}#slot"), "", class);
        spacer.kind = "slot".into();
        spacer.look.classes = vec!["slot".into(), class.into()];
        spacer.look.content = Content::Spacer;
        spacer.slot = Some(slot);
        out.push(spacer);
    }
}
