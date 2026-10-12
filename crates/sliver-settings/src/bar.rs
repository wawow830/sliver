//! The Bar page (ux.md §4): live preview of any layer in a simulated context, and editing of the
//! layout it shows (add from the Library, reorder, remove, inspect), plus "Customize for app…".

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{Context, ModuleRef, Width, parse_cli_value};
use toml::Value;

use crate::pages::{group, row, surface, width_text};
use crate::preview::{BAR, Preview};
use crate::{Ui, apply, edit};

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
    let caret: Rc<std::cell::Cell<Option<f64>>> = Rc::default();
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
        picture.set_draw_func(move |_, cr, _, h| {
            if let Some(s) = frame.borrow().as_ref() {
                cr.save().ok();
                cr.scale(0.5, 0.5);
                let _ = cr.set_source_surface(s, 0.0, 0.0);
                cr.source().set_filter(gtk::cairo::Filter::Best);
                let _ = cr.paint();
                cr.restore().ok();
            }
            // Drop position while dragging a module (widget coordinates).
            if let Some(x) = caret.get() {
                cr.set_source_rgb(0.21, 0.52, 0.89);
                cr.rectangle(x - 1.5, 0.0, 3.0, h as f64);
                let _ = cr.fill();
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

    let why = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    why.add_css_class("dim-label");
    page.append(&why);

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
    let shown_layout: Rc<RefCell<String>> = Rc::default();
    let refresh = {
        let (ui, preview, shown_layout) = (ui.clone(), preview.clone(), shown_layout.clone());
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
            picture.queue_draw();
            why.set_text(&match rule {
                Some(i) => format!(
                    "Editing layout “{layout}”, picked by rules[{i}]{}. Click a module to edit it; changes apply instantly.",
                    config.rules[i].name.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
                ),
                None => format!("Editing layout “{layout}”, the {layer} layer's default. Click a module to edit it; changes apply instantly."),
            });
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
            );
        });
    }
    picture.add_controller(click);
    // Drag a module along the preview to move it, also into another zone (GOAL: WYSIWYG
    // drag and drop). The click above is cancelled once the pointer moves past the threshold.
    let drag = gtk::GestureDrag::new();
    let start: Rc<std::cell::Cell<Option<(f64, f64)>>> = Rc::default();
    {
        let start = start.clone();
        drag.connect_drag_begin(move |_, x, y| start.set(Some((x, y))));
    }
    {
        let (start, caret, picture2) = (start.clone(), caret.clone(), picture.clone());
        drag.connect_drag_update(move |_, dx, _| {
            if let Some((x, _)) = start.get()
                && dx.abs() > 8.0
            {
                caret.set(Some(x + dx));
                picture2.queue_draw();
            }
        });
    }
    {
        let (ui, preview, shown_layout, caret, picture2) = (
            ui.clone(),
            preview.clone(),
            shown_layout.clone(),
            caret.clone(),
            picture.clone(),
        );
        drag.connect_drag_end(move |_, dx, _| {
            caret.set(None);
            picture2.queue_draw();
            let Some((x0, _)) = start.take() else { return };
            if dx.abs() <= 8.0 {
                return;
            }
            // Bar coordinates: the preview is drawn at half scale.
            let p = preview.borrow();
            let (Some(from), Some((to_zone, left))) = (
                p.hit(x0 as f32 * 2.0),
                p.drop_target((x0 + dx) as f32 * 2.0),
            ) else {
                return;
            };
            drop(p);
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
            let (Some(fz), Some(tz)) = (zones.get(from.zone), zones.get(to_zone)) else {
                return;
            };
            let Some(fi) = from.module_index(&ids(from.zone)) else {
                return;
            };
            let ti = left
                .and_then(|h| h.module_index(&ids(to_zone)))
                .map_or(0, |i| i + 1);
            let (layout, fz, tz) = (layout.clone(), fz.name.clone(), tz.name.clone());
            apply(&ui, move |ed, merged| {
                edit::move_module_to(ed, merged, &layout, &fz, fi, &tz, ti)
            });
        });
    }
    picture.add_controller(drag);
    picture.set_cursor_from_name(Some("pointer"));
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
        g.set_header_suffix(Some(&library_button(ui, layout, &z.name)));
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
                r.connect_activated(move |_| inspector(&ui, &layout, &zone, i, &title, &block));
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
        g.add(&entry);
    }
    page.add(&look);
    page.add(&actions);
    dialog.add(&page);
    dialog.present(Some(&ui.win_widget()));
}

fn crate_inline(v: &Value) -> String {
    sliver_config::writer::inline(v)
}
