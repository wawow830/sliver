//! The Layouts & Rules page (ux.md §5), editable: layer defaults, and rules in priority order
//! (first match wins) that can be added, moved, removed and edited. Layouts are listed with
//! their inheritance. They're edited on the Bar page.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{parse_cli_value, writer::inline};
use toml::{Table, Value};

use crate::pages::{group, row, summarize};
use crate::{Ui, apply, edit};

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let c = &state.loaded.config;
    let page = adw::PreferencesPage::new();

    // "Active now": asked of the running agent, filled in when it answers.
    let now = group("Active now", None);
    let now_row = row("Checking…", "");
    now.add(&now_row);
    page.add(&now);
    let rule_rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>> = Rc::default();
    {
        let (now_row, rule_rows) = (now_row.clone(), rule_rows.clone());
        crate::live::active(move |r| match r {
            Ok(a) => {
                let (title, why) = crate::live::explain(&a);
                now_row.set_title(&title);
                now_row.set_subtitle(&why);
                if a.source == "rule"
                    && a.rule >= 0
                    && let Some(rw) = rule_rows.borrow().get(a.rule as usize)
                {
                    let tag = gtk::Label::new(Some("active now"));
                    tag.add_css_class("accent");
                    tag.add_css_class("caption");
                    rw.add_prefix(&tag);
                }
            }
            Err(e) => {
                now_row.set_title("The agent isn't answering");
                now_row.set_subtitle(&format!("Start it to see what the bar shows ({e})."));
            }
        });
    }

    let layers = group(
        "Layers",
        Some("Each layer's default layout. Fn shows the fn layer; other layers open from actions."),
    );
    for (layer, layout) in &c.layers {
        let e = adw::EntryRow::builder()
            .title(layer.as_str())
            .show_apply_button(true)
            .build();
        e.set_text(layout);
        let (ui, layer) = (ui.clone(), layer.clone());
        e.connect_apply(move |e| {
            let layout = e.text().trim().to_owned();
            let layer = layer.clone();
            apply(&ui, move |ed, _| {
                ed.set(
                    &format!("layers.{}", quote(&layer)),
                    &Value::String(layout.clone()),
                )?;
                Ok(format!("The {layer} layer now shows {layout} by default"))
            });
        });
        layers.add(&e);
    }
    page.add(&layers);

    let rules = group(
        "Rules",
        Some(
            "Checked top to bottom; the first match wins. With no match, the layer's default layout is used.",
        ),
    );
    let add = gtk::Button::from_icon_name("list-add-symbolic");
    add.add_css_class("flat");
    add.set_tooltip_text(Some("Add a rule at the top"));
    {
        let ui = ui.clone();
        add.connect_clicked(move |_| {
            apply(&ui, |ed, merged| {
                let mut r = Table::new();
                r.insert("name".into(), "New rule".into());
                let mut when = Table::new();
                when.insert("app-id".into(), "change-me".into());
                r.insert("when".into(), Value::Table(when));
                r.insert("layout".into(), "default".into());
                edit::add_rule(ed, merged, r).map(|m| format!("{m}. Click it to edit."))
            });
        });
    }
    rules.set_header_suffix(Some(&add));
    if c.rules.is_empty() {
        rules.add(&row("No rules yet", "Every app gets the default layouts. Add one with +, or use “Customize for app…” on the Bar page."));
    }
    let n = c.rules.len();
    for (i, r) in c.rules.iter().enumerate() {
        let title = r.name.clone().unwrap_or_else(|| format!("rules[{i}]"));
        let rw = row(
            &title,
            &format!(
                "when {} → {} ({} layer)",
                summarize(&r.when),
                r.layout,
                r.layer
            ),
        );
        rw.set_activatable(true);
        {
            let (ui, title) = (ui.clone(), title.clone());
            rw.connect_activated(move |_| rule_dialog(&ui, i, &title));
        }
        for (icon, tip, delta) in [
            ("go-up-symbolic", "Higher priority", -1isize),
            ("go-down-symbolic", "Lower priority", 1),
        ] {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(tip));
            b.add_css_class("flat");
            b.set_valign(gtk::Align::Center);
            b.set_sensitive(if delta < 0 { i > 0 } else { i + 1 < n });
            let ui = ui.clone();
            b.connect_clicked(move |_| {
                apply(&ui, |ed, merged| edit::move_rule(ed, merged, i, delta))
            });
            rw.add_suffix(&b);
        }
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        del.set_valign(gtk::Align::Center);
        del.set_tooltip_text(Some("Remove"));
        {
            let ui = ui.clone();
            del.connect_clicked(move |_| apply(&ui, |ed, merged| edit::remove_rule(ed, merged, i)));
        }
        rw.add_suffix(&del);
        rule_rows.borrow_mut().push(rw.clone());
        rules.add(&rw);
    }
    page.add(&rules);

    let layouts = group(
        "Layouts",
        Some(
            "Edit a layout on the Bar page: pick the layer, or preview as an app its rule matches.",
        ),
    );
    for (name, l) in &c.layouts {
        let zones = c
            .layout_zones(name)
            .map(|z| {
                z.iter()
                    .map(|z| format!("{} ({})", z.name, z.modules.len()))
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .unwrap_or_else(|e| e.to_string());
        let ext = l
            .extends
            .as_deref()
            .map(|p| format!("extends {p} · "))
            .unwrap_or_default();
        layouts.add(&row(name, &format!("{ext}{zones}")));
    }
    page.add(&layouts);
    page.upcast()
}

fn quote(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_owned()
    } else {
        Value::String(k.to_owned()).to_string()
    }
}

/// Edit one rule's fields. Each applies on its own.
fn rule_dialog(ui: &Rc<Ui>, index: usize, title: &str) {
    let state = ui.state();
    let raw = state
        .loaded
        .merged
        .get("rules")
        .and_then(Value::as_array)
        .and_then(|a| a.get(index))
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    let dialog = adw::PreferencesDialog::builder()
        .title(glib::markup_escape_text(title))
        .build();
    let page = adw::PreferencesPage::new();
    let g = group(
        "Rule",
        Some(
            "When is a table of context keys (customization.md §6), e.g. { app-id = \"firefox\" }, { app-id = [\"foot\", \"kitty\"], fullscreen = true }, { on-battery = true }.",
        ),
    );
    let layouts: Vec<String> = state.loaded.config.layouts.keys().cloned().collect();
    let fields = [
        ("name", "Name", None),
        ("when", "When", None),
        ("layer", "Layer (default: base)", None),
        (
            "layout",
            "Layout",
            Some(format!("one of: {}", layouts.join(", "))),
        ),
    ];
    for (key, label, hint) in fields {
        let e = adw::EntryRow::builder()
            .title(label)
            .show_apply_button(true)
            .build();
        if let Some(h) = hint {
            e.set_tooltip_text(Some(&h));
        }
        e.set_text(
            &raw.get(key)
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => inline(other),
                })
                .unwrap_or_default(),
        );
        let ui = ui.clone();
        e.connect_apply(move |e| {
            let text = e.text().trim().to_owned();
            let value = (!text.is_empty()).then(|| {
                if key == "when" {
                    parse_cli_value(&text)
                } else {
                    Value::String(text.clone())
                }
            });
            apply(&ui, move |ed, merged| {
                edit::set_rule_key(ed, merged, index, key, value)
            });
        });
        g.add(&e);
    }
    page.add(&g);
    dialog.add(&page);
    dialog.present(Some(&ui.win_widget()));
}
