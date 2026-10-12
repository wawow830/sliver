//! Profiles & Presets (ux.md §8, §9): import a tiny-dfr or waybar setup, and export/import a
//! config as a preset file. Imports replace the config through `apply`: validated, with Undo.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use sliver_config::profile;

use crate::pages::group;
use crate::{Ui, apply};

/// Replace the user's config with `text` (an import), after asking.
fn replace_config(ui: &Rc<Ui>, what: &str, text: String, note: String) {
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Replace your config with the {what} import?"))
        .body(format!(
            "{note}You can undo this from the notification right after."
        ))
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("replace", "Replace")]);
    dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
    let (ui2, what) = (ui.clone(), what.to_owned());
    dialog.connect_response(None, move |_, r| {
        if r == "replace" {
            let text = text.clone();
            let what = what.clone();
            apply(&ui2, move |ed, _| {
                ed.replace_text(&text)?;
                Ok(format!("Imported your {what} setup"))
            });
        }
    });
    dialog.present(Some(&ui.win_widget()));
}

fn config_dir(ui: &Ui) -> PathBuf {
    ui.state()
        .edit_target()
        .0
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    page.add(&profiles_group(ui));

    let import = group(
        "Import",
        Some(
            "Bring an existing setup over. The bar keeps working throughout; a broken import is refused.",
        ),
    );
    let dfr = adw::ActionRow::builder()
        .title("tiny-dfr")
        .subtitle("/usr/share/tiny-dfr + /etc/tiny-dfr: same keys, same look")
        .activatable(true)
        .build();
    dfr.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    {
        let ui = ui.clone();
        dfr.connect_activated(move |_| match sliver_config::tinydfr::import_system() {
            Ok(Some(imp)) => {
                // The look is `theme = "classic"`; style.css only holds font/outline overrides,
                // and an existing one is kept.
                let css = config_dir(&ui).join("style.css");
                if !css.exists() {
                    let _ = std::fs::create_dir_all(css.parent().unwrap_or(Path::new(".")));
                    let _ = std::fs::write(&css, &imp.stylesheet);
                }
                let note = if imp.warnings.is_empty() {
                    String::new()
                } else {
                    format!("Notes: {}\n\n", imp.warnings.join("; "))
                };
                replace_config(&ui, "tiny-dfr", imp.config, note);
            }
            Ok(None) => ui.toast("tiny-dfr isn't installed"),
            Err(e) => ui.toast(&e),
        });
    }
    import.add(&dfr);

    let wb = adw::ActionRow::builder()
        .title("waybar")
        .subtitle("Pick a waybar config: modules, formats and actions carry over")
        .activatable(true)
        .build();
    wb.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    {
        let ui = ui.clone();
        wb.connect_activated(move |_| {
            let d = gtk::FileDialog::builder().title("waybar config").build();
            if let Some(home) = std::env::var_os("HOME") {
                d.set_initial_folder(Some(&gio::File::for_path(
                    PathBuf::from(home).join(".config/waybar"),
                )));
            }
            let ui2 = ui.clone();
            d.open(Some(&ui.win_widget()), gio::Cancellable::NONE, move |r| {
                let Ok(f) = r else { return };
                let Some(path) = f.path() else { return };
                let css = path.with_file_name("style.css");
                let opts = sliver_waybar::Options {
                    stylesheet: css.is_file().then(|| css.display().to_string()),
                };
                match sliver_waybar::import_path(&path, &opts) {
                    Ok(imp) => {
                        let note = format!(
                            "{} modules come over, {} don't. ",
                            imp.modules.len(),
                            imp.dropped.len()
                        );
                        replace_config(&ui2, "waybar", imp.toml, note);
                    }
                    Err(e) => ui2.toast(&e),
                }
            });
        });
    }
    import.add(&wb);
    page.add(&import);

    let presets = group(
        "Presets",
        Some("A preset is a config file: share it, keep variants, or try someone else's."),
    );
    let export = adw::ActionRow::builder()
        .title("Export your config…")
        .activatable(true)
        .build();
    export.add_suffix(&gtk::Image::from_icon_name("document-save-symbolic"));
    {
        let ui = ui.clone();
        export.connect_activated(move |_| {
            let d = gtk::FileDialog::builder()
                .title("Export preset")
                .initial_name("sliver-preset.toml")
                .build();
            let ui2 = ui.clone();
            d.save(Some(&ui.win_widget()), gio::Cancellable::NONE, move |r| {
                let Ok(f) = r else { return };
                let Some(dest) = f.path() else { return };
                let src = ui2.state().edit_target().0;
                let text = std::fs::read_to_string(&src).unwrap_or_else(|_| "version = 1\n".into());
                match std::fs::write(&dest, text) {
                    Ok(()) => ui2.toast(&format!("Exported to {}", dest.display())),
                    Err(e) => ui2.toast(&format!("Couldn't export: {e}")),
                }
            });
        });
    }
    presets.add(&export);
    let open = adw::ActionRow::builder()
        .title("Use a preset…")
        .subtitle("Scripts and commands in a preset run as you: read it first.")
        .activatable(true)
        .build();
    open.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    {
        let ui = ui.clone();
        open.connect_activated(move |_| {
            let d = gtk::FileDialog::builder().title("Preset").build();
            let ui2 = ui.clone();
            d.open(Some(&ui.win_widget()), gio::Cancellable::NONE, move |r| {
                let Ok(f) = r else { return };
                let Some(path) = f.path() else { return };
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        // List what it would run, as ux.md §9 asks, before replacing anything.
                        let runs: Vec<String> = text
                            .lines()
                            .map(str::trim)
                            .filter(|l| {
                                l.starts_with("exec")
                                    || l.starts_with("on-")
                                    || l.contains("exec =")
                            })
                            .take(8)
                            .map(str::to_owned)
                            .collect();
                        let note = if runs.is_empty() {
                            "It runs no commands. ".to_owned()
                        } else {
                            format!("It runs commands:\n{}\n\n", runs.join("\n"))
                        };
                        replace_config(&ui2, "preset", text, note);
                    }
                    Err(e) => ui2.toast(&format!("{}: {e}", path.display())),
                }
            });
        });
    }
    presets.add(&open);
    page.add(&presets);

    page.upcast()
}

/// A profile operation, then a toast and a rebuild (the window follows the active profile).
fn profile_op(ui: &Rc<Ui>, done: String, op: impl FnOnce() -> Result<(), String>) {
    match op() {
        Ok(()) => ui.toast(&done),
        Err(e) => ui.toast(&e),
    }
    crate::rebuild(ui);
}

/// Profiles (ux.md §9): whole configs, one active. Switching rewrites `active-profile`, which the
/// agent watches, so the bar follows at once.
fn profiles_group(ui: &Rc<Ui>) -> adw::PreferencesGroup {
    let g = group(
        "Profiles",
        Some(
            "Whole configs to switch between, like Work and Presenting. Everything in this app edits the active one.",
        ),
    );
    let active = profile::active();
    for name in profile::list() {
        let is_default = name == profile::DEFAULT;
        let r = adw::ActionRow::builder()
            .title(if is_default {
                "Default".to_owned()
            } else {
                name.clone()
            })
            .subtitle(if is_default {
                "config.toml"
            } else {
                "your main config plus its own changes"
            })
            .build();
        if name == active {
            r.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
        } else {
            let use_ = gtk::Button::builder()
                .label("Use")
                .valign(gtk::Align::Center)
                .build();
            let (ui, n) = (ui.clone(), name.clone());
            use_.connect_clicked(move |_| {
                let n = n.clone();
                profile_op(&ui, format!("Now using {n}"), || profile::set_active(&n));
            });
            r.add_suffix(&use_);
        }
        if !is_default {
            let rename = gtk::Button::builder()
                .icon_name("document-edit-symbolic")
                .tooltip_text("Rename")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let (ui2, n) = (ui.clone(), name.clone());
            rename.connect_clicked(move |_| rename_dialog(&ui2, &n));
            r.add_suffix(&rename);
            let del = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Delete")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let (ui2, n) = (ui.clone(), name.clone());
            del.connect_clicked(move |_| delete_dialog(&ui2, &n));
            r.add_suffix(&del);
        }
        g.add(&r);
    }
    let add = adw::EntryRow::builder()
        .title("New profile from the active one: name")
        .show_apply_button(true)
        .build();
    {
        let ui = ui.clone();
        add.connect_apply(move |e| {
            let name = e.text().trim().to_owned();
            let from = profile::active();
            profile_op(&ui, format!("Created {name} and switched to it"), || {
                profile::create(&name, Some(&from))?;
                profile::set_active(&name)
            });
        });
    }
    g.add(&add);
    g
}

fn rename_dialog(ui: &Rc<Ui>, name: &str) {
    let entry = gtk::Entry::builder()
        .text(name)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Rename {name}"))
        .extra_child(&entry)
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    let (ui2, name) = (ui.clone(), name.to_owned());
    dialog.connect_response(None, move |_, r| {
        if r == "rename" {
            let to = entry.text().trim().to_owned();
            profile_op(&ui2, format!("Renamed {name} to {to}"), || {
                profile::rename(&name, &to)
            });
        }
    });
    dialog.present(Some(&ui.win_widget()));
}

fn delete_dialog(ui: &Rc<Ui>, name: &str) {
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Delete the {name} profile?"))
        .body(if profile::active() == name {
            "It's active: the bar switches back to Default. Its file is deleted."
        } else {
            "Its file is deleted."
        })
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let (ui2, name) = (ui.clone(), name.to_owned());
    dialog.connect_response(None, move |_, r| {
        if r == "delete" {
            profile_op(&ui2, format!("Deleted {name}"), || profile::remove(&name));
        }
    });
    dialog.present(Some(&ui.win_widget()));
}
