//! The inspector: what a selected module is, and its settings as fields and pickers.

use super::*;

/// What an inspector edits: module `index` of a zone, or (`member`) a module inside a group,
/// whose settings live in its `[modules."id"]` block.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub layout: String,
    pub zone: String,
    pub index: usize,
    /// (innermost group, member id).
    pub member: Option<(String, String)>,
}

/// For a cell inside a group: the innermost group and the member's id, from the zone's top-level
/// module `top` (`group/tools`) and the cell key (`group/tools/custom/b`,
/// `group/a/group/b/clock`). `None` for a top-level module.
pub(crate) fn group_member(top: &str, key: &str) -> Option<(String, String)> {
    let mut group = top.to_owned();
    let mut rest = key.strip_prefix(&format!("{top}/"))?;
    while let Some(after) = rest.strip_prefix("group/")
        && let Some(slash) = after.find('/')
    {
        group = format!("group/{}", &after[..slash]);
        rest = &after[slash + 1..];
    }
    Some((group, rest.to_owned()))
}

impl Target {
    pub(super) fn slot(layout: &str, zone: &str, index: usize) -> Self {
        Target {
            layout: layout.to_owned(),
            zone: zone.to_owned(),
            index,
            member: None,
        }
    }

    fn set(
        &self,
        ed: &mut sliver_config::writer::Editor,
        merged: &toml::Table,
        k: &str,
        v: Option<Value>,
    ) -> Result<String, String> {
        match &self.member {
            Some((_, id)) => edit::set_named_module_key(ed, id, k, v),
            None => edit::set_module_key(ed, merged, &self.layout, &self.zone, self.index, k, v),
        }
    }

    /// Several keys in one edit (one undo step).
    fn set_many(
        &self,
        ed: &mut sliver_config::writer::Editor,
        merged: &toml::Table,
        changes: &[(String, Option<Value>)],
        what: &str,
    ) -> Result<String, String> {
        if changes.is_empty() {
            return Err("Nothing changed".into());
        }
        for (k, v) in changes {
            self.set(ed, merged, k, v.clone())?;
        }
        Ok(what.to_owned())
    }

    /// Everything the module sets now, as the current config resolves it.
    fn block(&self, ui: &Ui) -> Option<toml::Table> {
        let state = ui.state();
        let c = &state.loaded.config;
        if let Some((_, id)) = &self.member {
            let id = sliver_config::ModuleId::parse(id).ok()?;
            return Some(c.module_block(&ModuleRef::Named(id)));
        }
        let zones = c.layout_zones(&self.layout).ok()?;
        let z = zones.iter().find(|z| z.name == self.zone)?;
        Some(c.module_block(z.modules.get(self.index)?))
    }
}

/// The inspector: the common keys as text fields. Each applies on its own.
pub(super) fn inspector(
    ui: &Rc<Ui>,
    target: Target,
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
    let fields: [(&adw::PreferencesGroup, &str, &str); 3] = [
        (&look, "format", "Format"),
        (&look, "icon", "Icon"),
        (
            &look,
            "width",
            "Width: auto, fill, a px number, or { fill = 2 }",
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
        if key == "icon" {
            let button = gtk::Button::builder()
                .icon_name("image-x-generic-symbolic")
                .tooltip_text("Choose an icon, emoji or file")
                .valign(gtk::Align::Center)
                .build();
            button.add_css_class("flat");
            let (ui, target, dialog, entry2) =
                (ui.clone(), target.clone(), dialog.clone(), entry.clone());
            let theme = block
                .get("icon-theme")
                .and_then(Value::as_str)
                .map(str::to_owned);
            button.connect_clicked(move |_| {
                let (ui, target, entry) = (ui.clone(), target.clone(), entry2.clone());
                let pick: Rc<dyn Fn(crate::icon_picker::Choice)> = Rc::new(move |c| match c {
                    crate::icon_picker::Choice::Icon(name) => {
                        entry.set_text(&name);
                        entry.emit_by_name::<()>("apply", &[]);
                    }
                    crate::icon_picker::Choice::Emoji(text) => {
                        let target = target.clone();
                        apply(&ui, move |ed, merged| {
                            target.set(ed, merged, "icon", None)?;
                            target
                                .set(ed, merged, "format", Some(Value::String(text.clone())))
                                .map(|_| format!("Label set to {text}"))
                        });
                    }
                });
                crate::icon_picker::open(&dialog, theme.clone(), pick);
            });
            entry.add_suffix(&button);
        }
        let (ui, target, key) = (ui.clone(), target.clone(), key.to_owned());
        entry.connect_apply(move |e| {
            let text = e.text().trim().to_owned();
            let value = (!text.is_empty()).then(|| match key.as_str() {
                // Formats and icons are always strings, even if they look like TOML.
                "format" | "icon" => Value::String(text.clone()),
                _ => parse_cli_value(&text),
            });
            let (target, key) = (target.clone(), key.clone());
            apply(&ui, move |ed, merged| target.set(ed, merged, &key, value));
        });
        g.add(&entry);
    }
    // Touch: one action picker per gesture.
    let state = ui.state();
    let choices = Rc::new(crate::act::Choices {
        layers: state.loaded.config.layers.keys().cloned().collect(),
        layouts: state.loaded.config.layouts.keys().cloned().collect(),
        profiles: sliver_config::profile::list(),
    });
    // One picker per gesture. A waybar spelling (on-click…) shows under its
    // gesture and moves to the canonical key on the first edit.
    let picker = |g: Gesture, label: &str| {
        let alias = g.waybar_alias().filter(|a| block.contains_key(*a));
        let current = block
            .get(g.key())
            .or_else(|| alias.and_then(|a| block.get(a)));
        let write: Rc<dyn Fn(Option<Value>)> = {
            let (ui, target) = (ui.clone(), target.clone());
            Rc::new(move |v| {
                let target = target.clone();
                let mut changes = vec![(g.key().to_owned(), v)];
                if let Some(a) = alias {
                    changes.push((a.to_owned(), None));
                }
                let what = format!("Set {}", g.key());
                apply(&ui, move |ed, merged| {
                    target.set_many(ed, merged, &changes, &what)
                });
            })
        };
        crate::act::editor(label, current, choices.clone(), write)
    };
    let gesture_pickers = [
        picker(Gesture::Tap, "On tap"),
        picker(Gesture::Hold, "On hold"),
    ];
    let more = [
        (Gesture::DoubleTap, "On double tap"),
        (Gesture::SwipeLeft, "On swipe left"),
        (Gesture::SwipeRight, "On swipe right"),
        (Gesture::Release, "On release"),
    ];
    let more_set = more.iter().any(|(g, _)| {
        block.contains_key(g.key()) || g.waybar_alias().is_some_and(|a| block.contains_key(a))
    });
    let more_box = gtk::Box::new(gtk::Orientation::Vertical, 18);
    more_box.set_margin_top(12);
    for (g, label) in more {
        more_box.append(&picker(g, label));
    }
    let more_gestures = gtk::Expander::builder()
        .label("More gestures: double tap, swipes, release")
        .expanded(more_set)
        .child(&more_box)
        .build();
    let more_group = adw::PreferencesGroup::new();
    more_group.add(&more_gestures);
    // Updates: what a script runs and how often.
    let kind =
        sliver_config::ModuleId::parse(target.member.as_ref().map_or(title, |(_, id)| id.as_str()))
            .map(|id| id.kind)
            .unwrap_or_default();
    let mut update_fields: Vec<(&str, &str)> = Vec::new();
    let graph = kind == "graph" || kind == "custom-graph";
    if graph {
        update_fields.push((
            "source",
            "Graph of: cpu, memory, battery, custom/x… (empty: its own command)",
        ));
    }
    if kind == "custom" || kind == "custom-graph" {
        update_fields.push(("exec", "Command whose output it shows"));
    }
    if graph {
        update_fields.push(("samples", "Samples kept (default 30, newest at the right)"));
        update_fields.push(("min", "Bottom of the scale (default 0)"));
        update_fields.push(("max", "Top of the scale (default 100)"));
    }
    if kind == "custom" || graph || block.contains_key("interval") {
        update_fields.push(("interval", "Refresh every: seconds, \"5s\", \"1m\"…"));
    }
    if kind == "custom" {
        update_fields.push(("timeout", "Time limit for one run: seconds or \"2s\""));
    }
    let updates = (!update_fields.is_empty()).then(|| {
        let g = group("Updates", None);
        for (key, label) in update_fields {
            let entry = adw::EntryRow::builder()
                .title(label)
                .show_apply_button(true)
                .text(
                    block
                        .get(key)
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => crate_inline(other),
                        })
                        .unwrap_or_default(),
                )
                .build();
            let (ui, target) = (ui.clone(), target.clone());
            entry.connect_apply(move |e| {
                let text = e.text().trim().to_owned();
                let value = (!text.is_empty()).then(|| match key {
                    "exec" | "source" => Value::String(text.clone()),
                    _ => parse_cli_value(&text),
                });
                let target = target.clone();
                apply(&ui, move |ed, merged| target.set(ed, merged, key, value));
            });
            g.add(&entry);
        }
        g
    });
    // Advanced: the whole block as TOML; Apply writes only the keys that changed.
    let advanced = group(
        "Advanced",
        Some(
            "Everything this item sets, as TOML (sliver-config(5)). Apply writes only what you changed.",
        ),
    );
    {
        let buffer = gtk::TextBuffer::new(None);
        buffer.set_text(&toml::to_string(block).unwrap_or_default());
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .css_classes(["card"])
            .build();
        view.update_property(&[gtk::accessible::Property::Label("The item's TOML")]);
        let apply_btn = gtk::Button::builder()
            .label("Apply")
            .halign(gtk::Align::End)
            .margin_top(8)
            .css_classes(["suggested-action"])
            .build();
        let (ui, target, old) = (ui.clone(), target.clone(), block.clone());
        apply_btn.connect_clicked(move |_| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            let new: toml::Table = match text.parse() {
                Ok(t) => t,
                Err(e) => return ui.toast(&format!("Not valid TOML: {}", e.message())),
            };
            // Against the block as it is now: the pickers above may have written since.
            let now = target.block(&ui).unwrap_or_else(|| old.clone());
            let changes = edit::block_changes(&now, &new);
            let what = match changes.len() {
                1 => format!("Set {} as TOML", changes[0].0),
                n => format!("Set {n} keys as TOML"),
            };
            let target = target.clone();
            apply(&ui, move |ed, merged| {
                target.set_many(ed, merged, &changes, &what)
            });
        });
        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        col.append(&view);
        col.append(&apply_btn);
        advanced.add(&col);
    }
    if let Some(note) = width_px.and_then(width_note) {
        let size = group("Size", Some(&note));
        page.add(&size);
    }
    // A group's member: its own settings here, the member list there.
    if let Some((g, _)) = &target.member {
        let name = g.trim_start_matches("group/");
        let in_group = group(
            "In a group",
            Some(&format!(
                "This is inside “{name}”. What you set here applies wherever it's used."
            )),
        );
        let b = gtk::Button::builder()
            .label(format!("Edit the members of “{name}”…"))
            .halign(gtk::Align::Start)
            .build();
        let (ui, g) = (ui.clone(), g.clone());
        b.connect_clicked(move |_| group_dialog(&ui, &g));
        in_group.add(&b);
        page.add(&in_group);
    }
    page.add(&look);
    for g in &gesture_pickers {
        page.add(g);
    }
    page.add(&more_group);
    if let Some(u) = &updates {
        page.add(u);
    }
    // Visibility: the condition builder, as on rules.
    let write: Rc<dyn Fn(Option<Value>)> = {
        let (ui, target) = (ui.clone(), target.clone());
        Rc::new(move |v| {
            let target = target.clone();
            apply(&ui, move |ed, merged| target.set(ed, merged, "when", v));
        })
    };
    page.add(&crate::cond::editor(
        "Show only when",
        block.get("when"),
        write,
    ));
    page.add(&advanced);
    dialog.add(&page);
    dialog.present(Some(&ui.win_widget()));
}

pub(super) fn crate_inline(v: &Value) -> String {
    sliver_config::writer::inline(v)
}
