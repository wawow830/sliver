//! Profiles & Presets (ux.md §8, §9): import a tiny-dfr or waybar setup, and export/import a
//! config as a preset file. Imports replace the config through `apply`: validated, with Undo.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::pages::{group, row};
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

    let profiles = group("Profiles", None);
    profiles.add(&row("Profiles", "Switchable named configs come with the agent's profile support. Until then, presets do the job."));
    page.add(&profiles);
    page.upcast()
}
