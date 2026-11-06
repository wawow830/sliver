//! The Library: modules to add, as a palette to drag from and as a zone's "+" menu.

use super::*;

/// The Library as a palette under the preview, like macOS's Touch Bar customization: drag an item
/// onto the bar (the page's drag gesture), or click to add it at the end of the main zone (or the
/// last zone).
pub(super) fn palette(ui: &Rc<Ui>, shown_layout: &Rc<RefCell<String>>) -> gtk::Widget {
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

/// "+" on a zone: the Library, plus a field for any module reference.
pub(super) fn library_button(ui: &Rc<Ui>, layout: &str, zone: &str) -> gtk::MenuButton {
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
        // Weak: the list and entry inside the popover hold `add`.
        let (ui, layout, zone, pop) = (
            ui.clone(),
            layout.to_owned(),
            zone.to_owned(),
            pop.downgrade(),
        );
        Rc::new(move |m: String| {
            if let Some(pop) = pop.upgrade() {
                pop.popdown();
            }
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
