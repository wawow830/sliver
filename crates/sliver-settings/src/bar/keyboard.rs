//! Keyboard and screen-reader access to the preview.

use super::*;

/// Keyboard and screen-reader access to the preview. The preview is
/// one focusable widget; ←/→ (Home/End) select a module, Alt+←/→ move it, Delete removes it,
/// Enter or Space opens the inspector. The selection is in the widget's accessible name
/// ("Touch Bar preview: Volume slider, Controls zone, 3 of 4"), which screen readers speak; every edit rebuilds the page, so the selection
/// and focus live in [`Ui`].
pub(super) fn keyboard_access(
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
        // Weak: the preview's key handler holds `show`.
        let (ui, picture, preview, frame_focus) = (
            ui.clone(),
            picture.downgrade(),
            preview.clone(),
            frame_focus.clone(),
        );
        let (modules, zone_ids) = (modules.clone(), zone_ids.clone());
        Rc::new(move |speak: bool| {
            let Some(picture) = picture.upgrade() else {
                return;
            };
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
    // Touching the mirrored real bar selects that module here.
    *ui.on_held_pressed.borrow_mut() = Some(Box::new({
        let (ui, show, modules, picture) = (
            Rc::downgrade(ui),
            show.clone(),
            modules.clone(),
            picture.downgrade(),
        );
        move |zone: &str, index: usize| {
            let (Some(ui), Some(picture)) = (ui.upgrade(), picture.upgrade()) else {
                return;
            };
            if let Some((zi, ..)) = modules()
                .into_iter()
                .find(|(_, z, j, _)| z == zone && *j == index)
            {
                ui.bar_selection.set(Some((zi, index)));
                // The ring shows while the preview has focus.
                picture.grab_focus();
                show(true);
            }
        }
    }));

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
                        Target::slot(&layout, &zone, i),
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
        let ui = ui.clone();
        focus.connect_leave(move |c| {
            ui.bar_focused.set(false);
            if let Some(w) = c.widget() {
                w.queue_draw();
            }
        });
    }
    picture.add_controller(focus);
    // A click selects the module under it and takes keyboard focus (the existing click handler
    // opens the inspector on release); closing the inspector returns focus here.
    let press = gtk::GestureClick::new();
    {
        let (ui, preview, shown_layout) = (ui.clone(), preview.clone(), shown_layout.clone());
        press.connect_pressed(move |g, _, x, _| {
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
            if let Some(w) = g.widget() {
                w.grab_focus();
            }
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
pub(super) fn accessible_name(id: &str, zone: &str, n: usize, of: usize) -> String {
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
    fn group_members_from_cell_keys() {
        use super::group_member;
        let gm = |g: &str, m: &str| Some((g.to_owned(), m.to_owned()));
        assert_eq!(
            group_member("group/tools", "group/tools/custom/b"),
            gm("group/tools", "custom/b")
        );
        assert_eq!(
            group_member("group/a", "group/a/group/b/clock"),
            gm("group/b", "clock")
        );
        assert_eq!(group_member("group/tools", "group/tools"), None);
        assert_eq!(group_member("clock", "clock"), None);
    }

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
