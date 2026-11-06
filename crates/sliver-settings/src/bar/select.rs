//! Selecting on the preview: click, Shift/Ctrl multi-select, hover, and dragging modules
//! within the bar or in from the Library.

use super::*;

/// The bar under the preview for a Shift/Ctrl-click multi-selection, and the
/// function that redraws it and its rings from `ui.bar_multi` (dropping what no longer exists).
pub(super) fn multi_select(v: &View) -> (gtk::Box, Hook) {
    let label = gtk::Label::builder().xalign(0.0).hexpand(true).build();
    label.add_css_class("dim-label");
    let group = gtk::Button::builder()
        .label("Group…")
        .css_classes(["suggested-action"])
        .build();
    let clear = gtk::Button::with_label("Clear");
    let bar = gtk::Box::builder()
        .spacing(8)
        .margin_bottom(6)
        .visible(false)
        .build();
    bar.append(&label);
    bar.append(&clear);
    bar.append(&group);

    // Weak: the page's other closures already keep these alive for as long as it shows.
    let update: Hook = Rc::new({
        let (ui, preview, shown_layout, spans) = (
            v.ui.clone(),
            v.preview.clone(),
            v.shown_layout.clone(),
            v.marks.multi.clone(),
        );
        let (picture, bar, label, group) = (
            v.picture.clone(),
            bar.downgrade(),
            label.downgrade(),
            group.downgrade(),
        );
        move || {
            let state = ui.state();
            let zones = state
                .loaded
                .config
                .layout_zones(&shown_layout.borrow())
                .unwrap_or_default();
            let mut picked = ui.bar_multi.borrow_mut();
            picked.retain(|(zi, i)| zones.get(*zi).is_some_and(|z| *i < z.modules.len()));
            let ids = |zi: usize| -> Vec<String> {
                zones[zi]
                    .modules
                    .iter()
                    .map(|m| m.id().to_string())
                    .collect()
            };
            *spans.borrow_mut() = picked
                .iter()
                .filter_map(|(zi, i)| preview.borrow().module_span(*zi, &ids(*zi), *i))
                .collect();
            let n = picked.len();
            if let (Some(bar), Some(label), Some(group)) =
                (bar.upgrade(), label.upgrade(), group.upgrade())
            {
                bar.set_visible(n > 0);
                let zone = picked.first().map(|(zi, _)| zones[*zi].name.clone());
                label.set_text(&match (n, zone) {
                    (1, Some(z)) => format!(
                        "1 module of “{z}” selected. Shift- or Ctrl-click more to group them."
                    ),
                    (n, Some(z)) => format!("{n} modules of “{z}” selected"),
                    _ => String::new(),
                });
                group.set_sensitive(n >= 2);
            }
            if let Some(p) = picture.upgrade() {
                p.queue_draw();
            }
        }
    });
    {
        let (ui, update) = (v.ui.clone(), update.clone());
        clear.connect_clicked(move |_| {
            ui.bar_multi.borrow_mut().clear();
            update();
        });
    }
    {
        let v = v.clone();
        group.connect_clicked(move |_| {
            let picked = v.ui.bar_multi.borrow().clone();
            let Some((zone, ids)) = picked.first().and_then(|(zi, _)| v.zone(*zi)) else {
                return;
            };
            let checked: Vec<usize> = picked.iter().map(|(_, i)| *i).collect();
            make_group_dialog(&v.ui, &v.shown_layout.borrow(), &zone, &ids, &checked);
        });
    }
    (bar, update)
}

/// Click a module on the preview to open it in the inspector; with Shift or
/// Ctrl, add it to (or drop it from) the multi-selection instead.
pub(super) fn connect_select(v: &View, picture: &gtk::DrawingArea, update_multi: Hook) {
    let click = gtk::GestureClick::new();
    let view = v.clone();
    click.connect_released(move |g, _, x, _| {
        let v = &view;
        // The preview is drawn at half scale.
        let Some(hit) = v.preview.borrow().hit(x as f32 * 2.0) else {
            return;
        };
        let state = v.ui.state();
        let layout = v.shown_layout.borrow().clone();
        let Ok(zones) = state.loaded.config.layout_zones(&layout) else {
            return;
        };
        let Some(z) = zones.get(hit.zone) else { return };
        let ids: Vec<String> = z.modules.iter().map(|m| m.id().to_string()).collect();
        let Some(i) = hit.module_index(&ids) else {
            return;
        };
        let mods = g.current_event_state();
        if mods
            .intersects(gtk::gdk::ModifierType::SHIFT_MASK | gtk::gdk::ModifierType::CONTROL_MASK)
        {
            {
                let mut picked = v.ui.bar_multi.borrow_mut();
                // One zone at a time: a group takes modules from one zone.
                if picked.iter().any(|(zi, _)| *zi != hit.zone) {
                    picked.clear();
                }
                if let Some(k) = picked.iter().position(|p| *p == (hit.zone, i)) {
                    picked.remove(k);
                } else {
                    picked.push((hit.zone, i));
                }
            }
            update_multi();
            return;
        }
        if !v.ui.bar_multi.borrow().is_empty() {
            v.ui.bar_multi.borrow_mut().clear();
            update_multi();
        }
        let m = &z.modules[i];
        // A module inside a group: edit that member itself (its `[modules."id"]` block).
        if let Some((group, member)) = group_member(&ids[i], &hit.key)
            && let Ok(mid) = sliver_config::ModuleId::parse(&member)
        {
            inspector(
                &v.ui,
                Target {
                    member: Some((group.clone(), member.clone())),
                    ..Target::slot(&layout, &z.name, i)
                },
                &format!("{} › {member}", group.trim_start_matches("group/")),
                &state.loaded.config.module_block(&ModuleRef::Named(mid)),
                Some(hit.rect.1),
            );
            return;
        }
        inspector(
            &v.ui,
            Target::slot(&layout, &z.name, i),
            &m.id().to_string(),
            &state.loaded.config.module_block(m),
            Some(hit.rect.1),
        );
    });
    picture.add_controller(click);
    picture.set_cursor_from_name(Some("pointer"));
}

/// The width handle shows as a resize cursor on a module's right edge.
pub(super) fn connect_hover(v: &View, picture: &gtk::DrawingArea) {
    let hover = gtk::EventControllerMotion::new();
    let preview = v.preview.clone();
    hover.connect_motion(move |c, x, _| {
        let edge = preview.borrow().edge_at(x as f32 * 2.0).is_some();
        if let Some(w) = c.widget() {
            w.set_cursor_from_name(Some(if edge { "col-resize" } else { "pointer" }));
        }
    });
    picture.add_controller(hover);
}

/// Library items dropped on the preview go where the caret shows. Also returns the function that
/// puts a module there, which the page's own drag uses too.
pub(super) fn drop_target(v: &View) -> (gtk::DropTarget, DropFn) {
    let target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::COPY);
    {
        let v = v.clone();
        target.connect_motion(move |_, x, _| {
            v.marks.caret.set(v.insert_cue(x));
            v.redraw();
            gtk::gdk::DragAction::COPY
        });
    }
    {
        let v = v.clone();
        target.connect_leave(move |_| v.clear_cue());
    }
    // Puts Library module `module` where the caret is: at preview x-coordinate `x` (widget px).
    let drop_module: DropFn = {
        let v = v.clone();
        Rc::new(move |module: String, x: f64| {
            v.clear_cue();
            let Some((zone, left)) = v.preview.borrow().drop_target(x as f32 * 2.0) else {
                return false;
            };
            let Some((zname, ids)) = v.zone(zone) else {
                return false;
            };
            let at = left.and_then(|h| h.module_index(&ids)).map_or(0, |i| i + 1);
            let layout = v.shown_layout.borrow().clone();
            apply(&v.ui, move |ed, merged| {
                edit::insert_module(ed, merged, &layout, &zname, Some(at), &module)
            });
            true
        })
    };
    {
        let drop_module = drop_module.clone();
        target
            .connect_drop(move |_, val, x, _| val.get::<String>().is_ok_and(|m| drop_module(m, x)));
    }
    (target, drop_module)
}

/// Dragging (GOAL: WYSIWYG drag/drop/resize): a Library chip onto the bar; a module along the bar
/// to move it, also into another zone; a module off the bar (up or down) to remove it; a module's
/// right edge to set its width. One gesture on the page, which holds both the chips and the
/// preview, so it follows the pointer between them on every GDK backend (Broadway has no implicit
/// grab). It claims the pointer only once it has really moved, so plain clicks still reach the
/// chips and the preview.
pub(super) fn drag_gesture(v: &View, drop_module: DropFn) -> gtk::GestureDrag {
    let grab: Rc<RefCell<Option<Grab>>> = Rc::default();
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    // Page coordinates (the gesture's widget) -> preview (widget) coordinates.
    let to_bar = {
        let picture = v.picture.clone();
        move |g: &gtk::GestureDrag, x: f64, y: f64| {
            g.widget()?
                .compute_point(
                    &picture.upgrade()?,
                    &gtk::graphene::Point::new(x as f32, y as f32),
                )
                .map(|p| (p.x() as f64, p.y() as f64))
        }
    };
    // The preview x when the pointer is over (or just above or below) the preview.
    let over_bar = {
        let picture = v.picture.clone();
        move |(x, y): (f64, f64)| {
            let picture = picture.upgrade()?;
            let (w, h) = (picture.width() as f64, picture.height() as f64);
            (y > -12.0 && y < h + 12.0 && x >= 0.0 && x <= w).then_some(x)
        }
    };
    {
        let (v, grab, to_bar) = (v.clone(), grab.clone(), to_bar.clone());
        drag.connect_drag_begin(move |g, x, y| {
            let (Some(page), Some(picture)) = (g.widget(), v.picture.upgrade()) else {
                return;
            };
            let mut found = None;
            let mut w = page.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(cur) = w {
                if &cur == picture.upcast_ref::<gtk::Widget>() {
                    if let Some((bx, _)) = to_bar(g, x, y) {
                        let resize = v.preview.borrow().edge_at(bx as f32 * 2.0).is_some();
                        found = Some(Grab::Bar { x: bx, resize });
                    }
                    break;
                }
                if let Some(id) = cur.widget_name().strip_prefix("lib:") {
                    found = Some(Grab::Chip(id.to_owned(), cur.clone()));
                    break;
                }
                if cur == page {
                    break;
                }
                w = cur.parent();
            }
            *grab.borrow_mut() = found;
        });
    }
    {
        let (v, grab, to_bar, over_bar) =
            (v.clone(), grab.clone(), to_bar.clone(), over_bar.clone());
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(kind) = grab.borrow().clone() else {
                return;
            };
            if dx.abs() + dy.abs() < 8.0 {
                return;
            }
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(pos) = g.start_point().and_then(|(x, y)| to_bar(g, x + dx, y + dy)) else {
                return;
            };
            let cue = match kind {
                Grab::Chip(_, chip) => {
                    chip.set_opacity(0.5);
                    over_bar(pos).map_or(Cue::None, |x| v.insert_cue(x))
                }
                Grab::Bar { x, resize: true } => match v.preview.borrow().edge_at(x as f32 * 2.0) {
                    Some(h) => {
                        let left = h.rect.0 as f64 / 2.0;
                        Cue::Resize(left, pos.0.max(left + 8.0))
                    }
                    None => Cue::None,
                },
                Grab::Bar { x, resize: false } if dy.abs() > OFF_BAR => {
                    match v.preview.borrow().hit(x as f32 * 2.0) {
                        Some(h) => {
                            Cue::Remove(h.rect.0 as f64 / 2.0, (h.rect.0 + h.rect.1) as f64 / 2.0)
                        }
                        None => Cue::None,
                    }
                }
                Grab::Bar { .. } => v.insert_cue(pos.0),
            };
            v.marks.caret.set(cue);
            v.redraw();
        });
    }
    {
        let v = v.clone();
        drag.connect_drag_end(move |g, dx, dy| {
            v.clear_cue();
            let Some(kind) = grab.take() else { return };
            if let Grab::Chip(_, chip) = &kind {
                chip.set_opacity(1.0);
            }
            if dx.abs() + dy.abs() < 8.0 {
                return;
            }
            match kind {
                Grab::Chip(id, _) => {
                    let pos = g.start_point().and_then(|(x, y)| to_bar(g, x + dx, y + dy));
                    if let Some(x) = pos.and_then(&over_bar) {
                        drop_module(id, x);
                    }
                }
                Grab::Bar { x, resize } => {
                    finish_bar_drag(&v.ui, &v.preview, &v.shown_layout, x, dx, dy, resize)
                }
            }
        });
    }
    drag
}

pub(super) type DropFn = Rc<dyn Fn(String, f64) -> bool>;
/// A callback a page part gets from another.
pub(super) type Hook = Rc<dyn Fn()>;

/// What a drag on the Bar page started on.
#[derive(Clone)]
pub(super) enum Grab {
    /// A Library chip (module reference, the chip widget).
    Chip(String, gtk::Widget),
    /// A module on the preview at x (widget px); `resize` when it was the module's right edge.
    Bar { x: f64, resize: bool },
}

/// Applies a drag that started on the preview at `x0` (widget px) and moved by `dx`, `dy`: resize,
/// remove (dragged off the bar) or move.
pub(super) fn finish_bar_drag(
    ui: &Rc<Ui>,
    preview: &Rc<RefCell<Preview>>,
    shown_layout: &Rc<RefCell<String>>,
    x0: f64,
    dx: f64,
    dy: f64,
    resize: bool,
) {
    let ui = ui.clone();
    let p = preview.borrow();
    let state = ui.state();
    let layout = shown_layout.borrow().clone();
    let Ok(zones) = state.loaded.config.layout_zones(&layout) else {
        return;
    };
    let ids = |z: usize| -> Vec<String> {
        zones
            .get(z)
            .map(|z| z.modules.iter().map(|m| m.id().to_string()).collect())
            .unwrap_or_default()
    };
    if resize {
        let Some(h) = p.edge_at(x0 as f32 * 2.0) else {
            return;
        };
        let (Some(z), Some(i)) = (zones.get(h.zone), h.module_index(&ids(h.zone))) else {
            return;
        };
        if dx.abs() < 2.0 {
            return;
        }
        let px = (((x0 + dx) * 2.0) as f32 - h.rect.0).round().max(MIN_WIDTH) as i64;
        let (layout, z) = (layout.clone(), z.name.clone());
        drop(p);
        apply(&ui, move |ed, merged| {
            edit::set_module_key(
                ed,
                merged,
                &layout,
                &z,
                i,
                "width",
                Some(toml::Value::Integer(px)),
            )
        });
        return;
    }
    let Some(from) = p.hit(x0 as f32 * 2.0) else {
        return;
    };
    let (Some(fz), Some(fi)) = (zones.get(from.zone), from.module_index(&ids(from.zone))) else {
        return;
    };
    if dy.abs() > OFF_BAR {
        let (layout, fz) = (layout.clone(), fz.name.clone());
        drop(p);
        apply(&ui, move |ed, merged| {
            edit::remove_module(ed, merged, &layout, &fz, fi)
        });
        return;
    }
    if dx.abs() <= 8.0 {
        return;
    }
    let Some((to_zone, left)) = p.drop_target((x0 + dx) as f32 * 2.0) else {
        return;
    };
    drop(p);
    let Some(tz) = zones.get(to_zone) else { return };
    let ti = left
        .and_then(|h| h.module_index(&ids(to_zone)))
        .map_or(0, |i| i + 1);
    let (layout, fz, tz) = (layout.clone(), fz.name.clone(), tz.name.clone());
    apply(&ui, move |ed, merged| {
        edit::move_module_to(ed, merged, &layout, &fz, fi, &tz, ti)
    });
}
