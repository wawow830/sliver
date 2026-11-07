//! The zones under the preview: one group per zone, and the zone settings dialog.

use super::*;

pub(super) fn zone_groups(ui: &Rc<Ui>, container: &gtk::Box, layout: &str) {
    let state = ui.state();
    let config = &state.loaded.config;
    let Ok(zones) = config.layout_zones(layout) else {
        return;
    };
    let own: Vec<String> = config
        .layouts()
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
            b.connect_clicked(move |_| make_group_dialog(&ui, &layout, &zone, &ids, &[]));
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
                r.connect_activated(move |_| {
                    inspector(&ui, Target::slot(&layout, &zone, i), &title, &block, px)
                });
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
    // Zones are user-defined (sliver-config(5)): add one at the end.
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

/// The ⚙ on a zone's header: its settings dialog.
pub(super) fn zone_button(
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

/// Each setting applies on its own, with undo.
/// `pos`: the zone's index and how many zones the layout shows (for the move buttons).
pub(super) fn zone_dialog(
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
