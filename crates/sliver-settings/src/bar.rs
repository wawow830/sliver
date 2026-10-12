//! The Bar page (ux.md §4): live preview of any layer in a simulated context, and editing of the
//! layout it shows (add from the Library, reorder, remove, inspect), plus "Customize for app…".

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{Context, ModuleRef, Width, parse_cli_value};
use toml::Value;

use crate::pages::{group, row, surface, width_text};
use crate::preview::{Feedback, Preview, width_note};
use crate::{Ui, apply, edit};

/// Feedback drawn on the preview while dragging (widget x coordinates).
#[derive(Clone, Copy, Default)]
enum Cue {
    #[default]
    None,
    /// Where a moved or dropped module goes.
    Insert(f64),
    /// The module that leaves the bar when released here.
    Remove(f64, f64),
    /// The module's new extent while dragging its right edge.
    Resize(f64, f64),
}

/// Dragged this far (px) above or below the preview, a module is removed on release.
const OFF_BAR: f64 = 40.0;
/// The narrowest width the edge handle sets (bar px).
const MIN_WIDTH: f32 = 30.0;

/// What the Library offers (ux.md §4). Anything else can be typed as a module reference.
const LIBRARY: &[(&str, &str)] = &[
    ("clock", "Clock"),
    ("battery", "Battery"),
    ("mpris", "Now playing"),
    ("workspaces", "Workspaces"),
    ("backlight/slider", "Brightness slider"),
    ("audio/slider", "Volume slider"),
    ("kbd-backlight/slider", "Keyboard light slider"),
    ("key/Mute", "Mute key"),
    ("key/Esc", "Esc key"),
    ("idle-inhibitor", "Caffeine"),
    ("dnd", "Do not disturb"),
    ("dark-mode", "Dark mode"),
    ("screenshot", "Screenshot"),
    ("lock", "Lock screen"),
    ("power", "Power menu"),
    ("language", "Keyboard layout"),
    ("window", "Window title"),
    ("cpu", "CPU"),
    ("memory", "Memory"),
    ("network", "Network"),
    ("spacer", "Spacer"),
    ("app-controls", "App controls (focused app's buttons)"),
    ("status-items", "Status items (background apps)"),
];

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_margin_top(18);
    page.set_margin_bottom(18);
    page.set_margin_start(18);
    page.set_margin_end(18);

    let layers: Vec<String> = state.loaded.config.layers.keys().cloned().collect();
    let names: Vec<&str> = layers.iter().map(String::as_str).collect();
    let layer_dd = gtk::DropDown::from_strings(&names);
    let want = ui.layer.borrow().clone();
    layer_dd.set_selected(
        layers
            .iter()
            .position(|l| *l == want)
            .or_else(|| layers.iter().position(|l| l == "base"))
            .unwrap_or(0) as u32,
    );
    let app_entry = gtk::Entry::builder()
        .placeholder_text("app id, e.g. firefox")
        .width_chars(20)
        .text(ui.app.borrow().as_str())
        .build();
    let customize = gtk::Button::with_label("Customize for app…");
    customize.set_tooltip_text(Some("Give this app its own layout, based on the one shown"));
    let locked = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(ui.locked.get())
        .build();

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.append(&gtk::Label::new(Some("Layer")));
    controls.append(&layer_dd);
    controls.append(
        &gtk::Label::builder()
            .label("Preview as")
            .margin_start(16)
            .build(),
    );
    controls.append(&app_entry);
    controls.append(&customize);
    controls.append(
        &gtk::Label::builder()
            .label("Locked")
            .margin_start(16)
            .build(),
    );
    controls.append(&locked);
    let mirror = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(ui.mirror.get())
        .build();
    mirror.set_tooltip_text(Some(
        "Show the layout you're editing on the real Touch Bar while this window is focused",
    ));
    controls.append(
        &gtk::Label::builder()
            .label("Mirror")
            .margin_start(16)
            .build(),
    );
    controls.append(&mirror);
    page.append(&controls);

    // Drawn at half scale (1004x30 or 1085x30) so it stays legible; narrower windows scroll
    // sideways.
    let size = crate::preview::bar_size(state.hw);
    let frame: Rc<RefCell<Option<gtk::cairo::ImageSurface>>> = Rc::default();
    let caret: Rc<std::cell::Cell<Cue>> = Rc::default();
    // Overflow and small touch targets in the current render (ux.md §4), drawn over the preview.
    let sizes: Rc<RefCell<Feedback>> = Rc::default();
    // The keyboard-selected module's span (bar px), ringed while the preview has focus (T114).
    let frame_focus: Rc<std::cell::Cell<Option<(f32, f32)>>> = Rc::default();
    let picture = gtk::DrawingArea::builder()
        .content_width(size.0 as i32 / 2)
        .content_height(size.1 as i32 / 2)
        .halign(gtk::Align::Center)
        // Room for the overlay scrollbar, so it never sits on (and swallows clicks on) the bar.
        .margin_bottom(12)
        .build();
    {
        let frame = frame.clone();
        let caret = caret.clone();
        let sizes = sizes.clone();
        let frame_focus = frame_focus.clone();
        picture.set_draw_func(move |area, cr, _, h| {
            if let Some(s) = frame.borrow().as_ref() {
                cr.save().ok();
                cr.scale(0.5, 0.5);
                let _ = cr.set_source_surface(s, 0.0, 0.0);
                cr.source().set_filter(gtk::cairo::Filter::Best);
                let _ = cr.paint();
                cr.restore().ok();
            }
            // Bar px -> widget px (half scale). Overflowing zones: a red wash and a red right edge.
            let h = h as f64;
            let fb = sizes.borrow();
            for (_, x, w) in &fb.overflow {
                let (x, w) = (f64::from(*x) / 2.0, f64::from(*w) / 2.0);
                cr.set_source_rgba(0.88, 0.11, 0.14, 0.18);
                cr.rectangle(x, 0.0, w, h);
                let _ = cr.fill();
                cr.set_source_rgba(0.88, 0.11, 0.14, 0.9);
                cr.rectangle(x + w - 2.0, 0.0, 2.0, h);
                let _ = cr.fill();
            }
            // Small touch targets: a bar under the item, amber below 80 px, red below 60 px.
            for (_, (x, w), strong) in &fb.narrow {
                let (x, w) = (f64::from(*x) / 2.0, f64::from(*w) / 2.0);
                if *strong {
                    cr.set_source_rgba(0.88, 0.11, 0.14, 0.95);
                } else {
                    cr.set_source_rgba(0.96, 0.62, 0.0, 0.95);
                }
                cr.rectangle(x + 1.0, h - 3.0, (w - 2.0).max(1.0), 3.0);
                let _ = cr.fill();
            }
            drop(fb);
            if area.has_focus()
                && let Some((x, w)) = frame_focus.get()
            {
                let (x, w) = (f64::from(x) / 2.0, f64::from(w) / 2.0);
                cr.set_source_rgb(0.21, 0.52, 0.89);
                cr.set_line_width(2.0);
                cr.rectangle(x + 1.0, 1.0, (w - 2.0).max(1.0), h - 2.0);
                let _ = cr.stroke();
            }
            // Feedback while dragging (widget coordinates).
            match caret.get() {
                Cue::None => {}
                Cue::Insert(x) => {
                    cr.set_source_rgb(0.21, 0.52, 0.89);
                    cr.rectangle(x - 1.5, 0.0, 3.0, h);
                    let _ = cr.fill();
                }
                Cue::Remove(x0, x1) => {
                    cr.set_source_rgba(0.88, 0.11, 0.14, 0.45);
                    cr.rectangle(x0, 0.0, x1 - x0, h);
                    let _ = cr.fill();
                }
                Cue::Resize(x0, x1) => {
                    cr.set_source_rgba(0.21, 0.52, 0.89, 0.25);
                    cr.rectangle(x0, 0.0, x1 - x0, h);
                    let _ = cr.fill();
                    cr.set_source_rgb(0.21, 0.52, 0.89);
                    cr.rectangle(x1 - 1.5, 0.0, 3.0, h);
                    let _ = cr.fill();
                }
            }
        });
    }
    let bar_scroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&picture)
        .build();
    page.append(&bar_scroll);
    let shown_layout: Rc<RefCell<String>> = Rc::default();

    let why = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    why.add_css_class("dim-label");
    page.append(&why);
    let size_warning = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    size_warning.add_css_class("warning");
    page.append(&size_warning);

    let zones = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.append(
        &gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&zones)
            .build(),
    );

    let preview = Rc::new(RefCell::new(Preview::new(
        state.loaded.config.clone(),
        state.sheet.clone(),
        size,
    )));
    let refresh = {
        let (ui, preview, shown_layout) = (ui.clone(), preview.clone(), shown_layout.clone());
        let (sizes, size_warning) = (sizes.clone(), size_warning.clone());
        let (layer_dd, app_entry, locked, picture, why, zones, layers) = (
            layer_dd.clone(),
            app_entry.clone(),
            locked.clone(),
            picture.clone(),
            why.clone(),
            zones.clone(),
            layers.clone(),
        );
        Rc::new(move || {
            let state = ui.state();
            let layer = layers
                .get(layer_dd.selected() as usize)
                .cloned()
                .unwrap_or_else(|| "base".into());
            *ui.layer.borrow_mut() = layer.clone();
            *ui.app.borrow_mut() = app_entry.text().to_string();
            ui.locked.set(locked.is_active());
            let mut ctx = Context::new()
                .with("physical_esc", state.hw.physical_esc)
                .with("locked", locked.is_active());
            if !app_entry.text().is_empty() {
                ctx.set("app_id", app_entry.text().as_str());
            }
            while let Some(c) = zones.first_child() {
                zones.remove(&c);
            }
            let config = &state.loaded.config;
            let (layout, rule) = match config.select_layout(&layer, &ctx) {
                Ok((l, r)) => (l.to_owned(), r),
                Err(e) => {
                    why.set_text(&format!("Can't show this layer: {e}"));
                    return;
                }
            };
            *shown_layout.borrow_mut() = layout.clone();
            // Only base-layer layouts are mirrored: the agent's SetLayout replaces the base layer.
            if layer == "base" {
                ui.mirror_layout(&layout);
            } else {
                ui.stop_mirror();
            }
            match preview.borrow_mut().render(&layer, ctx.clone()) {
                Ok(p) => *frame.borrow_mut() = pages_surface(&p),
                Err(e) => {
                    *frame.borrow_mut() = None;
                    zones.append(&row("Can't render", &e));
                }
            }
            let fb = preview.borrow().feedback();
            size_warning.set_visible(fb.summary().is_some());
            size_warning.set_text(&fb.summary().unwrap_or_default());
            // Overflow is red, small targets amber (libadwaita style classes).
            size_warning.set_css_classes(if fb.overflow.is_empty() {
                &["warning"]
            } else {
                &["error"]
            });
            *sizes.borrow_mut() = fb;
            picture.queue_draw();
            let profile = sliver_config::profile::active();
            let in_profile = if profile == sliver_config::profile::DEFAULT {
                String::new()
            } else {
                format!("Profile “{profile}”: ")
            };
            why.set_text(&format!("{in_profile}{}", match rule {
                Some(i) => format!(
                    "Editing layout “{layout}”, picked by rules[{i}]{}. Click a module to edit it; changes apply instantly.",
                    config.rules[i].name.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
                ),
                None => format!("Editing layout “{layout}”, the {layer} layer's default. Click a module to edit it; changes apply instantly."),
            }));
            zone_groups(&ui, &zones, &layout);
        })
    };
    for w in [layer_dd.upcast_ref::<gtk::Widget>(), locked.upcast_ref()] {
        let r = refresh.clone();
        if let Some(dd) = w.downcast_ref::<gtk::DropDown>() {
            dd.connect_selected_notify(move |_| r());
        } else if let Some(sw) = w.downcast_ref::<gtk::Switch>() {
            sw.connect_active_notify(move |_| r());
        }
    }
    {
        let r = refresh.clone();
        app_entry.connect_changed(move |_| r());
    }
    {
        let (ui, app_entry, shown_layout) = (ui.clone(), app_entry.clone(), shown_layout.clone());
        customize.connect_clicked(move |_| {
            let app = app_entry.text().to_string();
            let base = shown_layout.borrow().clone();
            *ui.layer.borrow_mut() = "base".into();
            apply(&ui, |ed, merged| {
                edit::customize_for_app(ed, merged, &app, &base).map(|(_, msg)| msg)
            });
        });
    }
    // Click a module on the preview to open it in the inspector (ux.md §4 "Select").
    let click = gtk::GestureClick::new();
    {
        let (ui, preview, shown_layout) = (ui.clone(), preview.clone(), shown_layout.clone());
        click.connect_released(move |_, _, x, _| {
            // The preview is drawn at half scale.
            let Some(hit) = preview.borrow().hit(x as f32 * 2.0) else {
                return;
            };
            let state = ui.state();
            let layout = shown_layout.borrow().clone();
            let Ok(zones) = state.loaded.config.layout_zones(&layout) else {
                return;
            };
            let Some(z) = zones.get(hit.zone) else { return };
            let ids: Vec<String> = z.modules.iter().map(|m| m.id().to_string()).collect();
            let Some(i) = hit.module_index(&ids) else {
                return;
            };
            let m = &z.modules[i];
            inspector(
                &ui,
                &layout,
                &z.name,
                i,
                &m.id().to_string(),
                &state.loaded.config.module_block(m),
                Some(hit.rect.1),
            );
        });
    }
    picture.add_controller(click);
    picture.set_cursor_from_name(Some("pointer"));
    // The width handle shows as a resize cursor on a module's right edge.
    let hover = gtk::EventControllerMotion::new();
    {
        let (preview, picture2) = (preview.clone(), picture.clone());
        hover.connect_motion(move |_, x, _| {
            let edge = preview.borrow().edge_at(x as f32 * 2.0).is_some();
            picture2.set_cursor_from_name(Some(if edge { "col-resize" } else { "pointer" }));
        });
    }
    picture.add_controller(hover);

    // Library items dropped on the preview go where the caret shows.
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gtk::gdk::DragAction::COPY);
    {
        let (caret, picture2) = (caret.clone(), picture.clone());
        let preview = preview.clone();
        drop_target.connect_motion(move |_, x, _| {
            caret.set(match preview.borrow().insertion_x(x as f32 * 2.0) {
                Some(bx) => Cue::Insert(bx as f64 / 2.0),
                None => Cue::None,
            });
            picture2.queue_draw();
            gtk::gdk::DragAction::COPY
        });
    }
    {
        let (caret, picture2) = (caret.clone(), picture.clone());
        drop_target.connect_leave(move |_| {
            caret.set(Cue::None);
            picture2.queue_draw();
        });
    }
    // Puts Library module `m` where the caret is: at preview x-coordinate `x` (widget px).
    let drop_module: DropFn = {
        let (ui, preview, shown_layout, caret, picture2) = (
            ui.clone(),
            preview.clone(),
            shown_layout.clone(),
            caret.clone(),
            picture.clone(),
        );
        Rc::new(move |module: String, x: f64| {
            caret.set(Cue::None);
            picture2.queue_draw();
            let Some((zone, left)) = preview.borrow().drop_target(x as f32 * 2.0) else {
                return false;
            };
            let state = ui.state();
            let layout = shown_layout.borrow().clone();
            let Ok(zones) = state.loaded.config.layout_zones(&layout) else {
                return false;
            };
            let Some(z) = zones.get(zone) else {
                return false;
            };
            let ids: Vec<String> = z.modules.iter().map(|m| m.id().to_string()).collect();
            let at = left.and_then(|h| h.module_index(&ids)).map_or(0, |i| i + 1);
            let zname = z.name.clone();
            apply(&ui, move |ed, merged| {
                edit::insert_module(ed, merged, &layout, &zname, Some(at), &module)
            });
            true
        })
    };
    {
        let drop_module = drop_module.clone();
        drop_target
            .connect_drop(move |_, v, x, _| v.get::<String>().is_ok_and(|m| drop_module(m, x)));
    }
    let lib = palette(ui, &shown_layout);
    page.insert_child_after(&lib, Some(&bar_scroll));

    // Dragging (GOAL: WYSIWYG drag/drop/resize): a Library chip onto the bar; a module along the
    // bar to move it, also into another zone; a module off the bar (up or down) to remove it; a
    // module's right edge to set its width. One gesture on the page, which holds both the chips
    // and the preview, so it follows the pointer between them on every GDK backend (Broadway has
    // no implicit grab). It claims the pointer only once it has really moved, so plain clicks
    // still reach the chips and the preview.
    let grab: Rc<RefCell<Option<Grab>>> = Rc::default();
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let to_bar = {
        let (page, picture) = (page.clone(), picture.clone());
        move |x: f64, y: f64| {
            page.compute_point(&picture, &gtk::graphene::Point::new(x as f32, y as f32))
                .map(|p| (p.x() as f64, p.y() as f64))
        }
    };
    let over_bar = {
        let picture = picture.clone();
        move |(x, y): (f64, f64)| {
            let (w, h) = (picture.width() as f64, picture.height() as f64);
            (y > -12.0 && y < h + 12.0 && x >= 0.0 && x <= w).then_some(x)
        }
    };
    {
        let (grab, page2, picture2, preview, to_bar) = (
            grab.clone(),
            page.clone(),
            picture.clone(),
            preview.clone(),
            to_bar.clone(),
        );
        drag.connect_drag_begin(move |_, x, y| {
            let mut found = None;
            let mut w = page2.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(cur) = w {
                if &cur == picture2.upcast_ref::<gtk::Widget>() {
                    if let Some((bx, _)) = to_bar(x, y) {
                        let resize = preview.borrow().edge_at(bx as f32 * 2.0).is_some();
                        found = Some(Grab::Bar { x: bx, resize });
                    }
                    break;
                }
                if let Some(id) = cur.widget_name().strip_prefix("lib:") {
                    found = Some(Grab::Chip(id.to_owned(), cur.clone()));
                    break;
                }
                if &cur == page2.upcast_ref::<gtk::Widget>() {
                    break;
                }
                w = cur.parent();
            }
            *grab.borrow_mut() = found;
        });
    }
    {
        let (grab, caret, picture2, preview, to_bar, over_bar) = (
            grab.clone(),
            caret.clone(),
            picture.clone(),
            preview.clone(),
            to_bar.clone(),
            over_bar.clone(),
        );
        // The caret where the module will really land (after a whole group, not inside it).
        let snap = {
            let preview = preview.clone();
            move |x: f64| match preview.borrow().insertion_x(x as f32 * 2.0) {
                Some(bx) => Cue::Insert(bx as f64 / 2.0),
                None => Cue::None,
            }
        };
        drag.connect_drag_update(move |g, dx, dy| {
            let Some(kind) = grab.borrow().clone() else {
                return;
            };
            if dx.abs() + dy.abs() < 8.0 {
                return;
            }
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(pos) = g.start_point().and_then(|(x, y)| to_bar(x + dx, y + dy)) else {
                return;
            };
            let cue = match kind {
                Grab::Chip(_, chip) => {
                    chip.set_opacity(0.5);
                    over_bar(pos).map_or(Cue::None, &snap)
                }
                Grab::Bar { x, resize: true } => match preview.borrow().edge_at(x as f32 * 2.0) {
                    Some(h) => {
                        let left = h.rect.0 as f64 / 2.0;
                        Cue::Resize(left, pos.0.max(left + 8.0))
                    }
                    None => Cue::None,
                },
                Grab::Bar { x, resize: false } if dy.abs() > OFF_BAR => {
                    match preview.borrow().hit(x as f32 * 2.0) {
                        Some(h) => {
                            Cue::Remove(h.rect.0 as f64 / 2.0, (h.rect.0 + h.rect.1) as f64 / 2.0)
                        }
                        None => Cue::None,
                    }
                }
                Grab::Bar { .. } => snap(pos.0),
            };
            caret.set(cue);
            picture2.queue_draw();
        });
    }
    {
        let (ui, grab, caret, picture2, preview, shown_layout, drop_module) = (
            ui.clone(),
            grab.clone(),
            caret.clone(),
            picture.clone(),
            preview.clone(),
            shown_layout.clone(),
            drop_module.clone(),
        );
        drag.connect_drag_end(move |g, dx, dy| {
            caret.set(Cue::None);
            picture2.queue_draw();
            let Some(kind) = grab.take() else { return };
            if let Grab::Chip(_, chip) = &kind {
                chip.set_opacity(1.0);
            }
            if dx.abs() + dy.abs() < 8.0 {
                return;
            }
            match kind {
                Grab::Chip(id, _) => {
                    let pos = g.start_point().and_then(|(x, y)| to_bar(x + dx, y + dy));
                    if let Some(x) = pos.and_then(&over_bar) {
                        drop_module(id, x);
                    }
                }
                Grab::Bar { x, resize } => {
                    finish_bar_drag(&ui, &preview, &shown_layout, x, dx, dy, resize)
                }
            }
        });
    }
    page.add_controller(drag);
    picture.add_controller(drop_target);
    {
        let (ui, shown_layout) = (ui.clone(), shown_layout.clone());
        mirror.connect_active_notify(move |m| {
            ui.mirror.set(m.is_active());
            if m.is_active() {
                ui.mirror_layout(&shown_layout.borrow());
            } else {
                ui.stop_mirror();
            }
        });
    }
    keyboard_access(ui, &picture, &preview, &shown_layout, &frame_focus);
    refresh();
    page.upcast()
}

fn pages_surface(p: &sliver_render::Pixmap) -> Option<gtk::cairo::ImageSurface> {
    surface(p)
}

/// One preferences group per zone, with the zone's modules as editable rows.
fn zone_groups(ui: &Rc<Ui>, container: &gtk::Box, layout: &str) {
    let state = ui.state();
    let config = &state.loaded.config;
    let Ok(zones) = config.layout_zones(layout) else {
        return;
    };
    let own: Vec<String> = config
        .layouts
        .get(layout)
        .map(|l| l.zones.iter().map(|z| z.name.clone()).collect())
        .unwrap_or_default();
    let count = zones.len();
    for (zi, z) in zones.into_iter().enumerate() {
        let origin = if own.contains(&z.name) {
            String::new()
        } else {
            " · inherited (editing makes an override)".into()
        };
        let g = group(
            &format!("Zone “{}”", z.name),
            Some(&format!("width {}{origin}", width_text(z.width))),
        );
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.append(&library_button(ui, layout, &z.name));
        if z.modules.len() > 1 {
            let b = gtk::Button::from_icon_name("folder-new-symbolic");
            b.set_tooltip_text(Some("Group modules…"));
            b.add_css_class("flat");
            b.set_valign(gtk::Align::Center);
            let ids: Vec<String> = z.modules.iter().map(|m| m.id().to_string()).collect();
            let (ui, layout, zone) = (ui.clone(), layout.to_owned(), z.name.clone());
            b.connect_clicked(move |_| make_group_dialog(&ui, &layout, &zone, &ids));
            header.append(&b);
        }
        header.append(&zone_button(ui, layout, &z, origin.is_empty(), (zi, count)));
        g.set_header_suffix(Some(&header));
        if z.modules.is_empty() {
            g.add(&row("(empty)", "Add modules with +"));
        }
        let n = z.modules.len();
        for (i, m) in z.modules.iter().enumerate() {
            let block = config.module_block(m);
            let kind = &m.id().kind;
            let inline = if matches!(m, ModuleRef::Inline(..)) {
                " · inline"
            } else {
                ""
            };
            let w = block
                .get("width")
                .and_then(|v| Width::parse(v).ok())
                .or(z.module_width)
                .unwrap_or_default();
            let r = row(
                &m.id().to_string(),
                &format!("{kind} · {}{inline}", width_text(w)),
            );
            r.set_activatable(true);
            {
                let (ui, layout, zone, title) = (
                    ui.clone(),
                    layout.to_owned(),
                    z.name.clone(),
                    m.id().to_string(),
                );
                let block = block.clone();
                // A fixed px width is known without the preview (auto/fill ones are checked there).
                let px = block
                    .get("width")
                    .and_then(|w| w.as_integer())
                    .map(|w| w as f32);
                r.connect_activated(move |_| inspector(&ui, &layout, &zone, i, &title, &block, px));
            }
            for (icon, tip, delta) in [
                ("go-up-symbolic", "Move left", -1isize),
                ("go-down-symbolic", "Move right", 1),
            ] {
                let b = gtk::Button::from_icon_name(icon);
                b.set_tooltip_text(Some(tip));
                b.add_css_class("flat");
                b.set_valign(gtk::Align::Center);
                b.set_sensitive(if delta < 0 { i > 0 } else { i + 1 < n });
                let (ui, layout, zone) = (ui.clone(), layout.to_owned(), z.name.clone());
                b.connect_clicked(move |_| {
                    apply(&ui, |ed, merged| {
                        edit::move_module(ed, merged, &layout, &zone, i, delta)
                    })
                });
                r.add_suffix(&b);
            }
            let del = gtk::Button::from_icon_name("user-trash-symbolic");
            del.set_tooltip_text(Some("Remove"));
            del.add_css_class("flat");
            del.set_valign(gtk::Align::Center);
            {
                let (ui, layout, zone) = (ui.clone(), layout.to_owned(), z.name.clone());
                del.connect_clicked(move |_| {
                    apply(&ui, |ed, merged| {
                        edit::remove_module(ed, merged, &layout, &zone, i)
                    })
                });
            }
            r.add_suffix(&del);
            // A named group (the control strip, the F-keys): its members are editable too.
            if kind == "group" && matches!(m, ModuleRef::Named(_)) {
                let b = gtk::Button::from_icon_name("view-list-symbolic");
                b.set_tooltip_text(Some("Edit members"));
                b.add_css_class("flat");
                b.set_valign(gtk::Align::Center);
                let (ui, id) = (ui.clone(), m.id().to_string());
                b.connect_clicked(move |_| group_dialog(&ui, &id));
                r.add_prefix(&b);
            }
            g.add(&r);
        }
        container.append(&g);
    }
    // Zones are user-defined (customization.md §6): add one at the end.
    let add = group(
        "Add a zone",
        Some("Zones become the stylesheet class .zone-<name>."),
    );
    let entry = adw::EntryRow::builder()
        .title("New zone name (a-z, 0-9, - and _)")
        .show_apply_button(true)
        .build();
    {
        let (ui, layout) = (ui.clone(), layout.to_owned());
        entry.connect_apply(move |e| {
            let name = e.text().trim().to_owned();
            let layout = layout.clone();
            apply(&ui, move |ed, merged| {
                edit::add_zone(ed, merged, &layout, &name)
            });
        });
    }
    add.add(&entry);
    container.append(&add);
}

/// The ⚙ on a zone's header: its settings dialog (T111).
fn zone_button(
    ui: &Rc<Ui>,
    layout: &str,
    z: &sliver_config::Zone,
    own: bool,
    pos: (usize, usize),
) -> gtk::Button {
    let b = gtk::Button::from_icon_name("emblem-system-symbolic");
    b.set_tooltip_text(Some("Zone settings"));
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    let (ui, layout, z) = (ui.clone(), layout.to_owned(), z.clone());
    b.connect_clicked(move |_| zone_dialog(&ui, &layout, &z, own, pos));
    b
}

/// A zone's settings: name, width, alignment, default module width, order, removal. Each applies
/// on its own (instant apply + undo, Q3).
/// `pos`: the zone's index and how many zones the layout shows (for the move buttons).
fn zone_dialog(
    ui: &Rc<Ui>,
    layout: &str,
    z: &sliver_config::Zone,
    own: bool,
    (index, count): (usize, usize),
) {
    let dialog = adw::PreferencesDialog::builder()
        .title(glib::markup_escape_text(&format!("Zone “{}”", z.name)))
        .build();
    let page = adw::PreferencesPage::new();
    let look = group(
        "Zone",
        Some(if own {
            "Empty clears the setting."
        } else {
            "Inherited from the parent layout: changing it makes an override here."
        }),
    );
    let zone = z.name.clone();

    let name = adw::EntryRow::builder()
        .title("Name")
        .show_apply_button(true)
        .build();
    name.set_text(&zone);
    {
        let (ui, layout, zone, dialog) =
            (ui.clone(), layout.to_owned(), zone.clone(), dialog.clone());
        name.connect_apply(move |e| {
            let to = e.text().trim().to_owned();
            let (layout, zone) = (layout.clone(), zone.clone());
            apply(&ui, move |ed, merged| {
                edit::rename_zone(ed, merged, &layout, &zone, &to)
            });
            dialog.close();
        });
    }
    look.add(&name);

    // Width-like keys take the same forms as a module's width.
    let width_field = |key: &'static str, title: &str, current: Option<Width>| {
        let e = adw::EntryRow::builder()
            .title(title)
            .show_apply_button(true)
            .build();
        let shown = current.map(|w| match w.to_value() {
            Value::String(s) => s,
            other => crate_inline(&other),
        });
        e.set_text(&shown.unwrap_or_default());
        let (ui, layout, zone) = (ui.clone(), layout.to_owned(), zone.clone());
        e.connect_apply(move |e| {
            let text = e.text().trim().to_owned();
            let value = if text.is_empty() {
                None
            } else {
                let v = parse_cli_value(&text);
                if let Err(err) = Width::parse(&v) {
                    ui.toast(&err);
                    return;
                }
                Some(v)
            };
            let (layout, zone) = (layout.clone(), zone.clone());
            apply(&ui, move |ed, merged| {
                edit::set_zone_key(ed, merged, &layout, &zone, key, value)
            });
        });
        e
    };
    look.add(&width_field(
        "width",
        "Width: auto, fill, a px number, or { fill = 2 }",
        Some(z.width),
    ));
    look.add(&width_field(
        "module-width",
        "Default module width (e.g. fill for tiny-dfr-style equal keys)",
        z.module_width,
    ));

    let aligns = ["start", "center", "end"];
    let align = adw::ComboRow::builder()
        .title("Align (when the modules don't fill the zone)")
        .model(&gtk::StringList::new(&aligns))
        .build();
    align.set_selected(match z.align {
        sliver_config::Align::Start => 0,
        sliver_config::Align::Center => 1,
        sliver_config::Align::End => 2,
    });
    {
        let (ui, layout, zone) = (ui.clone(), layout.to_owned(), zone.clone());
        align.connect_selected_notify(move |c| {
            let v = Value::String(aligns[(c.selected() as usize).min(2)].into());
            let (layout, zone) = (layout.clone(), zone.clone());
            apply(&ui, move |ed, merged| {
                edit::set_zone_key(ed, merged, &layout, &zone, "align", Some(v))
            });
        });
    }
    look.add(&align);

    let order = group("Order", None);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for (label, delta) in [("Move left", -1isize), ("Move right", 1)] {
        let b = gtk::Button::with_label(label);
        b.set_sensitive(if delta < 0 {
            index > 0
        } else {
            index + 1 < count
        });
        let (ui, layout, zone, dialog) =
            (ui.clone(), layout.to_owned(), zone.clone(), dialog.clone());
        b.connect_clicked(move |_| {
            let (layout, zone) = (layout.clone(), zone.clone());
            apply(&ui, move |ed, merged| {
                edit::move_zone(ed, merged, &layout, &zone, delta)
            });
            dialog.close();
        });
        buttons.append(&b);
    }
    let remove = gtk::Button::with_label("Remove zone");
    remove.add_css_class("destructive-action");
    remove.set_sensitive(count > 1); // a layout keeps at least one zone
    {
        let (ui, layout, zone, dialog) =
            (ui.clone(), layout.to_owned(), zone.clone(), dialog.clone());
        remove.connect_clicked(move |_| {
            let (layout, zone) = (layout.clone(), zone.clone());
            apply(&ui, move |ed, merged| {
                edit::remove_zone(ed, merged, &layout, &zone)
            });
            dialog.close();
        });
    }
    buttons.append(&remove);
    order.add(&buttons);

    page.add(&look);
    page.add(&order);
    dialog.add(&page);
    dialog.present(Some(&ui.win_widget()));
}

type DropFn = Rc<dyn Fn(String, f64) -> bool>;

/// What a drag on the Bar page started on.
#[derive(Clone)]
enum Grab {
    /// A Library chip (module reference, the chip widget).
    Chip(String, gtk::Widget),
    /// A module on the preview at x (widget px); `resize` when it was the module's right edge.
    Bar { x: f64, resize: bool },
}

/// Applies a drag that started on the preview at `x0` (widget px) and moved by `dx`, `dy`: resize,
/// remove (dragged off the bar) or move.
fn finish_bar_drag(
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

/// The Library as a palette under the preview, like macOS's Touch Bar customization: drag an item
/// onto the bar (the page's drag gesture), or click to add it at the end of the main zone (or the
/// last zone).
fn palette(ui: &Rc<Ui>, shown_layout: &Rc<RefCell<String>>) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_margin_bottom(10);
    for (id, label) in LIBRARY {
        // "App controls (focused app's buttons)" → "App controls"; the tooltip has the rest.
        let short = label.split(" (").next().unwrap_or(label);
        // Not a GtkButton: its click gesture claims the pointer on press, which would cancel the
        // drag. A card with our own click and drag gestures instead.
        let chip = gtk::Box::builder()
            .css_classes(["card"])
            .tooltip_text(format!(
                "{label} ({id}): drag onto the bar, or click to add it"
            ))
            .build();
        chip.append(
            &gtk::Label::builder()
                .label(short)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(12)
                .margin_end(12)
                .build(),
        );
        chip.set_cursor_from_name(Some("grab"));
        // The page's drag gesture recognizes chips by this name.
        chip.set_widget_name(&format!("lib:{id}"));
        let (ui, shown_layout, id) = (ui.clone(), shown_layout.clone(), (*id).to_owned());
        let click = gtk::GestureClick::new();
        click.connect_released(move |g, _, x, y| {
            // A drag that ended back on the chip isn't a click.
            if !g.widget().is_some_and(|w| w.contains(x, y)) {
                return;
            }
            let state = ui.state();
            let layout = shown_layout.borrow().clone();
            let Ok(zones) = state.loaded.config.layout_zones(&layout) else {
                return;
            };
            let Some(z) = zones
                .iter()
                .find(|z| z.name == "main")
                .or_else(|| zones.last())
            else {
                return;
            };
            let (zone, id) = (z.name.clone(), id.clone());
            apply(&ui, move |ed, merged| {
                edit::add_module(ed, merged, &layout, &zone, &id)
            });
        });
        chip.add_controller(click);
        row.append(&chip);
    }
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let hint = gtk::Label::builder()
        .label("Library: drag onto the bar. Drag a module off the bar to remove it, or its right edge to resize it.")
        .xalign(0.0)
        .wrap(true)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    col.append(&hint);
    col.append(
        &gtk::ScrolledWindow::builder()
            .child(&row)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .build(),
    );
    col.upcast()
}

/// Holds a list's refill closure for the buttons it creates (they can only hold it weakly).
type FillCell = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// A group's members and drawer options (ux.md §4 "Group"): reorder, remove, add from the
/// Library. Edits copy the effective member list into the user's file first (arrays replace).
fn group_dialog(ui: &Rc<Ui>, group_id: &str) {
    let dialog = adw::PreferencesDialog::builder()
        .title(glib::markup_escape_text(group_id))
        .build();
    let page = adw::PreferencesPage::new();
    let members = group(
        "Members",
        Some("In order, left to right. In a drawer the first one is the button that opens it."),
    );
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    members.add(&list);
    // Rebuilt from the freshly loaded config after each edit (apply reloads the state). The row
    // buttons reach it through a weak cell: they live inside what it rebuilds.
    let cell: FillCell = Rc::default();
    let again = {
        let cell = Rc::downgrade(&cell);
        move || {
            // After the click handler returns: the fill removes the button that's running it.
            if let Some(f) = cell.upgrade().and_then(|c| c.borrow().clone()) {
                glib::idle_add_local_once(move || f());
            }
        }
    };
    let fill: Rc<dyn Fn()> = Rc::new({
        let (ui, list, gid) = (ui.clone(), list.downgrade(), group_id.to_owned());
        move || {
            let Some(list) = list.upgrade() else { return };
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let state = ui.state();
            let m: Vec<String> = state
                .loaded
                .merged
                .get("modules")
                .and_then(|m| m.get(&gid))
                .and_then(|g| g.get("modules"))
                .and_then(Value::as_array)
                .map(|a| a.iter().map(crate_inline_or_str).collect())
                .unwrap_or_default();
            if m.is_empty() {
                list.append(&row("(empty)", "Add members below"));
            }
            let n = m.len();
            for (i, id) in m.iter().enumerate() {
                let r = row(id, "");
                for (icon, tip, delta) in [
                    ("go-up-symbolic", "Move left", -1isize),
                    ("go-down-symbolic", "Move right", 1),
                ] {
                    let b = gtk::Button::from_icon_name(icon);
                    b.set_tooltip_text(Some(tip));
                    b.add_css_class("flat");
                    b.set_valign(gtk::Align::Center);
                    b.set_sensitive(if delta < 0 { i > 0 } else { i + 1 < n });
                    let (ui, gid, again) = (ui.clone(), gid.clone(), again.clone());
                    b.connect_clicked(move |_| {
                        let gid = gid.clone();
                        apply(&ui, move |ed, merged| {
                            edit::move_group_member(ed, merged, &gid, i, delta)
                        });
                        again();
                    });
                    r.add_suffix(&b);
                }
                let del = gtk::Button::from_icon_name("user-trash-symbolic");
                del.set_tooltip_text(Some("Remove from the group"));
                del.add_css_class("flat");
                del.set_valign(gtk::Align::Center);
                let (ui2, gid2, again) = (ui.clone(), gid.clone(), again.clone());
                del.connect_clicked(move |_| {
                    let gid = gid2.clone();
                    apply(&ui2, move |ed, merged| {
                        edit::remove_group_member(ed, merged, &gid, i)
                    });
                    again();
                });
                r.add_suffix(&del);
                list.append(&r);
            }
        }
    });
    *cell.borrow_mut() = Some(fill.clone());
    fill();
    // Add a member: the Library, or any module reference.
    let add = adw::EntryRow::builder()
        .title("Add a member: e.g. clock, key/F5, custom/vpn")
        .show_apply_button(true)
        .build();
    {
        let (ui, gid, fill) = (ui.clone(), group_id.to_owned(), fill.clone());
        add.connect_apply(move |e| {
            let id = e.text().trim().to_owned();
            if id.is_empty() {
                return;
            }
            let gid = gid.clone();
            apply(&ui, move |ed, merged| {
                edit::add_group_member(ed, merged, &gid, &id)
            });
            e.set_text("");
            fill();
        });
    }
    let lib = gtk::DropDown::from_strings(
        &std::iter::once("Add from the Library…")
            .chain(LIBRARY.iter().map(|(_, l)| *l))
            .collect::<Vec<_>>(),
    );
    lib.set_valign(gtk::Align::Center);
    {
        let (ui, gid, fill) = (ui.clone(), group_id.to_owned(), fill.clone());
        lib.connect_selected_notify(move |d| {
            let Some((id, _)) = (d.selected() as usize)
                .checked_sub(1)
                .and_then(|i| LIBRARY.get(i))
            else {
                return;
            };
            let gid = gid.clone();
            apply(&ui, move |ed, merged| {
                edit::add_group_member(ed, merged, &gid, id)
            });
            d.set_selected(0);
            fill();
        });
    }
    add.add_suffix(&lib);
    // In its own list: rows added to the group directly would sit above `list`.
    let add_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(12)
        .build();
    add_list.append(&add);
    members.add(&add_list);
    page.add(&members);

    // Drawer (customization.md §3.1): the first member opens the rest.
    let state = ui.state();
    let block = state
        .loaded
        .merged
        .get("modules")
        .and_then(|m| m.get(group_id))
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    let drawer = block.get("drawer").and_then(Value::as_table).cloned();
    let dg = group(
        "Drawer",
        Some("A drawer shows only its first member until tapped, like the macOS control strip."),
    );
    const MODES: &[(&str, &str)] = &[
        ("", "Off: all members show"),
        ("inline", "Opens in place"),
        ("bar", "Opens as a sub-bar"),
    ];
    let current = drawer
        .as_ref()
        .map(|d| d.get("mode").and_then(Value::as_str).unwrap_or("inline"))
        .unwrap_or("");
    let d = drawer.clone().unwrap_or_default();
    let after = adw::EntryRow::builder()
        .title("Close again after (e.g. 5s; empty = stays open)")
        .show_apply_button(true)
        .text(
            d.get("collapse-after")
                .map(crate_inline_or_str)
                .unwrap_or_default(),
        )
        .sensitive(drawer.is_some())
        .build();
    {
        let (ui, gid) = (ui.clone(), group_id.to_owned());
        after.connect_apply(move |e| {
            let t = e.text().trim().to_owned();
            let gid = gid.clone();
            apply(&ui, move |ed, _| {
                let path = format!("modules.{}.drawer.collapse-after", toml_key(&gid));
                if t.is_empty() {
                    ed.remove(&path)?;
                } else {
                    ed.set(&path, &Value::String(t.clone()))?;
                }
                Ok(format!(
                    "{gid}: closes after {}",
                    if t.is_empty() { "never" } else { &t }
                ))
            });
        });
    }
    let open = adw::SwitchRow::builder()
        .title("Start open")
        .active(
            d.get("start-expanded")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
        .sensitive(drawer.is_some())
        .build();
    {
        let (ui, gid) = (ui.clone(), group_id.to_owned());
        open.connect_active_notify(move |s| {
            let (on, gid) = (s.is_active(), gid.clone());
            apply(&ui, move |ed, _| {
                ed.set(
                    &format!("modules.{}.drawer.start-expanded", toml_key(&gid)),
                    &Value::Boolean(on),
                )?;
                Ok(format!(
                    "{gid}: starts {}",
                    if on { "open" } else { "closed" }
                ))
            });
        });
    }
    let mode = adw::ComboRow::builder()
        .title("Kind")
        .model(&gtk::StringList::new(
            &MODES.iter().map(|(_, l)| *l).collect::<Vec<_>>(),
        ))
        .selected(MODES.iter().position(|(m, _)| *m == current).unwrap_or(0) as u32)
        .build();
    {
        let (ui, gid, after, open) = (ui.clone(), group_id.to_owned(), after.clone(), open.clone());
        mode.connect_selected_notify(move |c| {
            let m = MODES[c.selected() as usize].0;
            after.set_sensitive(!m.is_empty());
            open.set_sensitive(!m.is_empty());
            let gid = gid.clone();
            apply(&ui, move |ed, _| {
                let path = format!("modules.{}.drawer", toml_key(&gid));
                if m.is_empty() {
                    ed.remove(&path)?;
                    Ok(format!("{gid} isn't a drawer any more"))
                } else {
                    ed.set(&format!("{path}.mode"), &Value::String(m.into()))?;
                    Ok(format!("{gid}: drawer, {m}"))
                }
            });
        });
    }
    dg.add(&mode);
    dg.add(&after);
    dg.add(&open);
    page.add(&dg);
    dialog.add(&page);
    // The dialog owns the refill cell (the buttons only hold it weakly) until it closes.
    dialog.connect_closed(move |_| {
        cell.borrow_mut().take();
    });
    dialog.present(Some(&ui.win_widget()));
}

fn crate_inline_or_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => crate_inline(other),
    }
}

/// A TOML key, quoted when it isn't bare (`"group/control-strip"`).
fn toml_key(k: &str) -> String {
    if k.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_owned()
    } else {
        Value::String(k.to_owned()).to_string()
    }
}

/// "Group modules…": pick modules of a zone and a name; they become `group/<name>` in place.
fn make_group_dialog(ui: &Rc<Ui>, layout: &str, zone: &str, ids: &[String]) {
    let checks: Vec<gtk::CheckButton> = ids
        .iter()
        .map(|id| gtk::CheckButton::with_label(id))
        .collect();
    let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
    for c in &checks {
        col.append(c);
    }
    let name = gtk::Entry::builder()
        .placeholder_text("group name, e.g. media")
        .margin_top(8)
        .activates_default(true)
        .build();
    col.append(&name);
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Group modules of “{zone}”"))
        .body("The chosen modules become one group where the first of them was. Turn it into a drawer from the group's member list.")
        .extra_child(&col)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("group", "Group")]);
    dialog.set_response_appearance("group", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("group"));
    let (ui2, layout, zone) = (ui.clone(), layout.to_owned(), zone.to_owned());
    dialog.connect_response(None, move |_, r| {
        if r != "group" {
            return;
        }
        let picked: Vec<usize> = checks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_active())
            .map(|(i, _)| i)
            .collect();
        let (layout, zone, n) = (layout.clone(), zone.clone(), name.text().to_string());
        apply(&ui2, move |ed, merged| {
            edit::make_group(ed, merged, &layout, &zone, &picked, &n)
        });
    });
    dialog.present(Some(&ui.win_widget()));
}

/// "+" on a zone: the Library (ux.md §4), plus a field for any module reference.
fn library_button(ui: &Rc<Ui>, layout: &str, zone: &str) -> gtk::MenuButton {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    for (id, label) in LIBRARY {
        let r = adw::ActionRow::builder()
            .title(*label)
            .subtitle(*id)
            .use_markup(false)
            .activatable(true)
            .build();
        r.set_widget_name(id);
        list.append(&r);
    }
    let custom = gtk::Entry::builder()
        .placeholder_text("or type a module: key/F5, custom/vpn, battery#bat2")
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.append(
        &gtk::ScrolledWindow::builder()
            .child(&list)
            .min_content_height(320)
            .propagate_natural_width(true)
            .build(),
    );
    content.append(&custom);
    let pop = gtk::Popover::builder().child(&content).build();
    let button = gtk::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add a module")
        .popover(&pop)
        .build();
    button.add_css_class("flat");
    let add = {
        let (ui, layout, zone, pop) = (ui.clone(), layout.to_owned(), zone.to_owned(), pop.clone());
        Rc::new(move |m: String| {
            pop.popdown();
            let (layout, zone) = (layout.clone(), zone.clone());
            apply(&ui, move |ed, merged| {
                edit::add_module(ed, merged, &layout, &zone, &m)
            });
        })
    };
    {
        let add = add.clone();
        list.connect_row_activated(move |_, r| add(r.widget_name().to_string()));
    }
    custom.connect_activate(move |e| {
        let t = e.text().trim().to_owned();
        if !t.is_empty() {
            add(t);
        }
    });
    button
}

/// The inspector (ux.md §4): the common keys as text fields. Each applies on its own.
fn inspector(
    ui: &Rc<Ui>,
    layout: &str,
    zone: &str,
    index: usize,
    title: &str,
    block: &toml::Table,
    // Its width on the preview (bar px), when opened from there: warns if small for a finger.
    width_px: Option<f32>,
) {
    let dialog = adw::PreferencesDialog::builder()
        .title(glib::markup_escape_text(title))
        .build();
    let page = adw::PreferencesPage::new();
    let look = group(
        "Look",
        Some("Empty clears the setting. Formats use waybar syntax, e.g. {icon} {capacity}%."),
    );
    let actions = group(
        "Touch",
        Some("A shell command, or TOML for typed actions: { key = \"F5\" }, { layer = \"media\" }"),
    );
    let fields: [(&adw::PreferencesGroup, &str, &str); 5] = [
        (&look, "format", "Format"),
        (&look, "icon", "Icon (name or path)"),
        (
            &look,
            "width",
            "Width: auto, fill, a px number, or { fill = 2 }",
        ),
        (&actions, "on-tap", "On tap"),
        (&actions, "on-hold", "On hold"),
    ];
    let tap_entry: Rc<RefCell<Option<adw::EntryRow>>> = Rc::default();
    for (g, key, label) in fields {
        let current = block.get(key).map(|v| match v {
            Value::String(s) => s.clone(),
            other => crate_inline(other),
        });
        let entry = adw::EntryRow::builder()
            .title(label)
            .show_apply_button(true)
            .build();
        entry.set_text(current.as_deref().unwrap_or(""));
        let is_tap = key == "on-tap";
        let (ui, layout, zone, key) = (
            ui.clone(),
            layout.to_owned(),
            zone.to_owned(),
            key.to_owned(),
        );
        entry.connect_apply(move |e| {
            let text = e.text().trim().to_owned();
            let value = (!text.is_empty()).then(|| match key.as_str() {
                // Formats and icons are always strings, even if they look like TOML.
                "format" | "icon" => Value::String(text.clone()),
                _ => parse_cli_value(&text),
            });
            let (layout, zone, key) = (layout.clone(), zone.clone(), key.clone());
            apply(&ui, move |ed, merged| {
                edit::set_module_key(ed, merged, &layout, &zone, index, &key, value)
            });
        });
        if is_tap {
            *tap_entry.borrow_mut() = Some(entry.clone());
        }
        g.add(&entry);
    }
    // D-Bus presets: fill the "On tap" field (press its apply button to keep it).
    {
        let model = gtk::StringList::new(&[]);
        model.append("Choose a D-Bus action…");
        for p in crate::dbus_presets::PRESETS {
            model.append(p.label);
        }
        let combo = adw::ComboRow::builder()
            .title("Fill “On tap” with a D-Bus action")
            .model(&model)
            .build();
        let tap_entry = tap_entry.clone();
        combo.connect_selected_notify(move |c| {
            let i = c.selected() as usize;
            if i == 0 {
                return;
            }
            if let (Some(p), Some(e)) = (
                crate::dbus_presets::PRESETS.get(i - 1),
                tap_entry.borrow().as_ref(),
            ) {
                e.set_text(p.toml);
                e.grab_focus();
            }
        });
        actions.add(&combo);
    }
    if let Some(note) = width_px.and_then(width_note) {
        let size = group("Size", Some(&note));
        page.add(&size);
    }
    page.add(&look);
    page.add(&actions);
    // Visibility (ux.md §4.3): the condition builder, as on rules.
    let write: Rc<dyn Fn(Option<Value>)> = {
        let (ui, layout, zone) = (ui.clone(), layout.to_owned(), zone.to_owned());
        Rc::new(move |v| {
            let (layout, zone) = (layout.clone(), zone.clone());
            apply(&ui, move |ed, merged| {
                edit::set_module_key(ed, merged, &layout, &zone, index, "when", v)
            });
        })
    };
    page.add(&crate::cond::editor(
        "Show only when",
        block.get("when"),
        write,
    ));
    dialog.add(&page);
    dialog.present(Some(&ui.win_widget()));
}

fn crate_inline(v: &Value) -> String {
    sliver_config::writer::inline(v)
}

/// Keyboard and screen-reader access to the preview (ux.md §4 "Keyboard", T114). The preview is
/// one focusable widget; ←/→ (Home/End) select a module, Alt+←/→ move it, Delete removes it,
/// Enter or Space opens the inspector. The selection is in the widget's accessible name
/// ("Touch Bar preview: Volume slider, Controls zone, 3 of 4"), which screen readers speak; every edit rebuilds the page, so the selection
/// and focus live in [`Ui`].
fn keyboard_access(
    ui: &Rc<Ui>,
    picture: &gtk::DrawingArea,
    preview: &Rc<RefCell<Preview>>,
    shown_layout: &Rc<RefCell<String>>,
    frame_focus: &Rc<std::cell::Cell<Option<(f32, f32)>>>,
) {
    picture.set_focusable(true);
    picture.set_focus_on_click(true);
    picture.update_property(&[gtk::accessible::Property::Label("Touch Bar preview")]);
    picture.update_property(&[gtk::accessible::Property::Description(
        "Left and Right select a module, Alt+Left and Alt+Right move it, Delete removes it, \
         Enter edits it",
    )]);

    // Every module of the shown layout in bar order: (zone index, zone name, index, id).
    let modules = {
        let (ui, shown_layout) = (ui.clone(), shown_layout.clone());
        move || -> Vec<(usize, String, usize, String)> {
            let state = ui.state();
            let Ok(zones) = state.loaded.config.layout_zones(&shown_layout.borrow()) else {
                return Vec::new();
            };
            zones
                .iter()
                .enumerate()
                .flat_map(|(zi, z)| {
                    z.modules
                        .iter()
                        .enumerate()
                        .map(move |(i, m)| (zi, z.name.clone(), i, m.id().to_string()))
                })
                .collect()
        }
    };
    let zone_ids = {
        let (ui, shown_layout) = (ui.clone(), shown_layout.clone());
        move |zi: usize| -> Vec<String> {
            let state = ui.state();
            state
                .loaded
                .config
                .layout_zones(&shown_layout.borrow())
                .ok()
                .and_then(|zs| zs.get(zi).cloned())
                .map(|z| z.modules.iter().map(|m| m.id().to_string()).collect())
                .unwrap_or_default()
        }
    };
    // Show (ring), describe and speak the current selection.
    let show = {
        let (ui, picture, preview, frame_focus) = (
            ui.clone(),
            picture.clone(),
            preview.clone(),
            frame_focus.clone(),
        );
        let (modules, zone_ids) = (modules.clone(), zone_ids.clone());
        Rc::new(move |speak: bool| {
            let list = modules();
            let sel = ui
                .bar_selection
                .get()
                .and_then(|(zi, i)| list.iter().position(|(z, _, j, _)| *z == zi && *j == i));
            let Some(k) = sel else {
                frame_focus.set(None);
                picture.queue_draw();
                return;
            };
            let (zi, zone, i, id) = &list[k];
            let span = preview.borrow().module_span(*zi, &zone_ids(*zi), *i);
            frame_focus.set(span);
            picture.queue_draw();
            // Keep it in view: the preview scrolls sideways in narrow windows.
            if let (Some((x, w)), Some(sw)) = (
                span,
                picture
                    .ancestor(gtk::ScrolledWindow::static_type())
                    .and_downcast::<gtk::ScrolledWindow>(),
            ) {
                let adj = sw.hadjustment();
                let (x0, x1) = (f64::from(x) / 2.0, f64::from(x + w) / 2.0);
                if x0 < adj.value() {
                    adj.set_value(x0 - 8.0);
                } else if x1 > adj.value() + adj.page_size() {
                    adj.set_value(x1 - adj.page_size() + 8.0);
                }
            }
            let in_zone = list.iter().filter(|(z, ..)| z == zi).count();
            let text = accessible_name(id, zone, i + 1, in_zone);
            // The focused widget's name changes, which screen readers speak. (Not
            // `GtkAccessible::announce`: it crashed GTK 4.22 here when no AT-SPI bus was there.)
            let _ = speak;
            picture.update_property(&[gtk::accessible::Property::Label(&format!(
                "Touch Bar preview: {text}"
            ))]);
        })
    };
    let select = {
        let (ui, show, modules) = (ui.clone(), show.clone(), modules.clone());
        Rc::new(move |step: isize, absolute: Option<bool>| {
            let list = modules();
            if list.is_empty() {
                return;
            }
            let cur = ui
                .bar_selection
                .get()
                .and_then(|(zi, i)| list.iter().position(|(z, _, j, _)| *z == zi && *j == i));
            let k = match (absolute, cur) {
                (Some(true), _) => 0,
                (Some(false), _) => list.len() - 1,
                (None, None) => 0,
                (None, Some(k)) => k.saturating_add_signed(step).min(list.len() - 1),
            };
            ui.bar_selection.set(Some((list[k].0, list[k].2)));
            show(true);
        })
    };

    let keys = gtk::EventControllerKey::new();
    {
        let (ui, shown_layout, preview, modules) = (
            ui.clone(),
            shown_layout.clone(),
            preview.clone(),
            modules.clone(),
        );
        let select = select.clone();
        keys.connect_key_pressed(move |_, key, _, mods| {
            use gtk::gdk::Key;
            let alt = mods.contains(gtk::gdk::ModifierType::ALT_MASK);
            let selected = || {
                let list = modules();
                let (zi, i) = ui.bar_selection.get()?;
                list.into_iter().find(|(z, _, j, _)| *z == zi && *j == i)
            };
            match key {
                Key::Left | Key::Right if alt => {
                    let Some((zi, zone, i, _)) = selected() else {
                        return glib::Propagation::Stop;
                    };
                    let delta = if key == Key::Left { -1 } else { 1 };
                    let layout = shown_layout.borrow().clone();
                    if let Some(to) = i.checked_add_signed(delta) {
                        ui.bar_selection.set(Some((zi, to)));
                    }
                    ui.bar_focused.set(true);
                    apply(&ui, move |ed, merged| {
                        edit::move_module(ed, merged, &layout, &zone, i, delta)
                    });
                }
                Key::Left | Key::KP_Left => select(-1, None),
                Key::Right | Key::KP_Right => select(1, None),
                Key::Home => select(0, Some(true)),
                Key::End => select(0, Some(false)),
                Key::Delete | Key::KP_Delete | Key::BackSpace => {
                    let Some((_, zone, i, _)) = selected() else {
                        return glib::Propagation::Stop;
                    };
                    let layout = shown_layout.borrow().clone();
                    ui.bar_focused.set(true);
                    apply(&ui, move |ed, merged| {
                        edit::remove_module(ed, merged, &layout, &zone, i)
                    });
                }
                Key::Return | Key::KP_Enter | Key::space => {
                    let Some((zi, zone, i, id)) = selected() else {
                        return glib::Propagation::Stop;
                    };
                    let state = ui.state();
                    let layout = shown_layout.borrow().clone();
                    let Ok(zones) = state.loaded.config.layout_zones(&layout) else {
                        return glib::Propagation::Stop;
                    };
                    let Some(m) = zones.get(zi).and_then(|z| z.modules.get(i)) else {
                        return glib::Propagation::Stop;
                    };
                    let ids: Vec<String> = zones[zi]
                        .modules
                        .iter()
                        .map(|m| m.id().to_string())
                        .collect();
                    let width = preview.borrow().module_span(zi, &ids, i).map(|(_, w)| w);
                    inspector(
                        &ui,
                        &layout,
                        &zone,
                        i,
                        &id,
                        &state.loaded.config.module_block(m),
                        width,
                    );
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }
    picture.add_controller(keys);
    let focus = gtk::EventControllerFocus::new();
    {
        let (ui, show, select) = (ui.clone(), show.clone(), select.clone());
        focus.connect_enter(move |_| {
            ui.bar_focused.set(true);
            if ui.bar_selection.get().is_none() {
                select(0, Some(true));
            } else {
                show(true);
            }
        });
    }
    {
        let (ui, picture2) = (ui.clone(), picture.clone());
        focus.connect_leave(move |_| {
            ui.bar_focused.set(false);
            picture2.queue_draw();
        });
    }
    picture.add_controller(focus);
    // A click selects the module under it and takes keyboard focus (the existing click handler
    // opens the inspector on release); closing the inspector returns focus here.
    let press = gtk::GestureClick::new();
    {
        let (ui, picture2, preview, shown_layout) = (
            ui.clone(),
            picture.clone(),
            preview.clone(),
            shown_layout.clone(),
        );
        press.connect_pressed(move |_, _, x, _| {
            if let Some(hit) = preview.borrow().hit(x as f32 * 2.0) {
                let state = ui.state();
                if let Ok(zones) = state.loaded.config.layout_zones(&shown_layout.borrow())
                    && let Some(z) = zones.get(hit.zone)
                {
                    let ids: Vec<String> = z.modules.iter().map(|m| m.id().to_string()).collect();
                    if let Some(i) = hit.module_index(&ids) {
                        ui.bar_selection.set(Some((hit.zone, i)));
                    }
                }
            }
            picture2.grab_focus();
        });
    }
    picture.add_controller(press);
    // After an edit made from the keyboard the page is rebuilt: put focus back where it was.
    show(false);
    if ui.bar_focused.get() {
        let (picture, show) = (picture.clone(), show.clone());
        // Once laid out (the scroll position needs the allocation).
        glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            picture.grab_focus();
            show(false);
        });
    }
}

/// What a screen reader says for a module on the preview: "Volume slider, Controls zone, 3 of 4".
fn accessible_name(id: &str, zone: &str, n: usize, of: usize) -> String {
    let name = LIBRARY
        .iter()
        .find(|(lib, _)| *lib == id)
        .map_or_else(|| id.to_owned(), |(_, label)| (*label).to_owned());
    let mut zone_chars = zone.chars();
    let zone = zone_chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().chain(zone_chars).collect()
    });
    format!("{name}, {zone} zone, {n} of {of}")
}

#[cfg(test)]
mod a11y_tests {
    #[test]
    fn accessible_names() {
        assert_eq!(
            super::accessible_name("audio/slider", "controls", 3, 4),
            "Volume slider, Controls zone, 3 of 4"
        );
        assert_eq!(
            super::accessible_name("custom/vpn", "main", 1, 1),
            "custom/vpn, Main zone, 1 of 1"
        );
    }
}
