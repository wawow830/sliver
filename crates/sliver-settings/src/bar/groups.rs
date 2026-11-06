//! Groups and drawers: the dialog that edits a group's members and drawer options, and making a
//! group from a multi-selection.

use super::*;

/// Holds a list's refill closure for the buttons it creates (they can only hold it weakly).
pub(super) type FillCell = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// A group's members and drawer options: reorder, remove, add from the
/// Library. Edits copy the effective member list into the user's file first (arrays replace).
pub(super) fn group_dialog(ui: &Rc<Ui>, group_id: &str) {
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

    // Drawer (sliver-config(5)): the first member opens the rest.
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

pub(super) fn crate_inline_or_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => crate_inline(other),
    }
}

/// A TOML key, quoted when it isn't bare (`"group/control-strip"`).
pub(super) fn toml_key(k: &str) -> String {
    if k.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_owned()
    } else {
        Value::String(k.to_owned()).to_string()
    }
}

/// "Group modules…": pick modules of a zone and a name; they become `group/<name>` in place.
pub(super) fn make_group_dialog(
    ui: &Rc<Ui>,
    layout: &str,
    zone: &str,
    ids: &[String],
    checked: &[usize],
) {
    let checks: Vec<gtk::CheckButton> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let c = gtk::CheckButton::with_label(id);
            c.set_active(checked.contains(&i));
            c
        })
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
        ui2.bar_multi.borrow_mut().clear();
        apply(&ui2, move |ed, merged| {
            edit::make_group(ed, merged, &layout, &zone, &picked, &n)
        });
    });
    dialog.present(Some(&ui.win_widget()));
}
