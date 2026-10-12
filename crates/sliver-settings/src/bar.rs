//! The Bar page (ux.md §4): live preview of any layer in a simulated context, and editing of the
//! layout it shows (add from the Library, reorder, remove, inspect), plus "Customize for app…".

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{Context, ModuleRef, Width, parse_cli_value};
use toml::Value;

use crate::pages::{group, row, surface, width_text};
use crate::preview::{BAR, Feedback, Preview, width_note};
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

    // Drawn at half scale (1004x30) so it stays legible; narrower windows scroll sideways.
    let frame: Rc<RefCell<Option<gtk::cairo::ImageSurface>>> = Rc::default();
    let caret: Rc<std::cell::Cell<Cue>> = Rc::default();
    // Overflow and small touch targets in the current render (ux.md §4), drawn over the preview.
    let sizes: Rc<RefCell<Feedback>> = Rc::default();
    let picture = gtk::DrawingArea::builder()
        .content_width(BAR.0 as i32 / 2)
        .content_height(BAR.1 as i32 / 2)
        .halign(gtk::Align::Center)
        // Room for the overlay scrollbar, so it never sits on (and swallows clicks on) the bar.
        .margin_bottom(12)
        .build();
    {
        let frame = frame.clone();
        let caret = caret.clone();
        let sizes = sizes.clone();
        picture.set_draw_func(move |_, cr, _, h| {
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
    for z in zones {
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
        header.append(&zone_button(ui, layout, &z, origin.is_empty()));
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
fn zone_button(ui: &Rc<Ui>, layout: &str, z: &sliver_config::Zone, own: bool) -> gtk::Button {
    let b = gtk::Button::from_icon_name("emblem-system-symbolic");
    b.set_tooltip_text(Some("Zone settings"));
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    let (ui, layout, z) = (ui.clone(), layout.to_owned(), z.clone());
    b.connect_clicked(move |_| zone_dialog(&ui, &layout, &z, own));
    b
}

/// A zone's settings: name, width, alignment, default module width, order, removal. Each applies
/// on its own (instant apply + undo, Q3).
fn zone_dialog(ui: &Rc<Ui>, layout: &str, z: &sliver_config::Zone, own: bool) {
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
    let fields: [(&adw::PreferencesGroup, &str, &str); 6] = [
        (&look, "format", "Format"),
        (&look, "icon", "Icon (name or path)"),
        (
            &look,
            "width",
            "Width: auto, fill, a px number, or { fill = 2 }",
        ),
        (&actions, "on-tap", "On tap"),
        (&actions, "on-hold", "On hold"),
        (
            &actions,
            "when",
            "Show only when, e.g. { on-battery = true }",
        ),
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
    dialog.add(&page);
    dialog.present(Some(&ui.win_widget()));
}

fn crate_inline(v: &Value) -> String {
    sliver_config::writer::inline(v)
}
