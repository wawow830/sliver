//! Profiles & Presets (ux.md §8, §9): import a tiny-dfr or waybar setup, and export/import a
//! config as a preset file. Imports replace the config through `apply`: validated, with Undo.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use gtk::glib;
use sliver_config::preset::{self, Bundle};
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
        Some(
            "A preset (.sliver) is your whole setup in one readable file: config, stylesheet and icons. Share it, keep variants, or try someone else's.",
        ),
    );
    let export = adw::ActionRow::builder()
        .title("Export your setup…")
        .activatable(true)
        .build();
    export.add_suffix(&gtk::Image::from_icon_name("document-save-symbolic"));
    {
        let ui = ui.clone();
        export.connect_activated(move |_| export_preset(&ui));
    }
    presets.add(&export);
    let open = adw::ActionRow::builder()
        .title("Use a preset…")
        .subtitle("You see everything it would run before anything changes.")
        .activatable(true)
        .build();
    open.add_suffix(&gtk::Image::from_icon_name("document-open-symbolic"));
    {
        let ui = ui.clone();
        open.connect_activated(move |_| {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Sliver presets"));
            filter.add_pattern(&format!("*.{}", preset::EXTENSION));
            filter.add_pattern("*.toml");
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let d = gtk::FileDialog::builder()
                .title("Preset")
                .filters(&filters)
                .build();
            let ui2 = ui.clone();
            d.open(Some(&ui.win_widget()), gio::Cancellable::NONE, move |r| {
                let Ok(f) = r else { return };
                let Some(path) = f.path() else { return };
                open_preset(&ui2, &path);
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

/// A `.sliver` bundle, or a bare config file (what older versions exported) as a one-file bundle.
fn read_bundle(path: &Path) -> Result<Bundle, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let bare_config = || {
        text.parse::<toml::Table>()
            .is_ok_and(|t| !t.contains_key("preset"))
    };
    match Bundle::parse(&text) {
        Ok(b) => Ok(b),
        Err(_) if bare_config() => Ok(Bundle {
            name: path.file_stem().map(|s| s.to_string_lossy().into_owned()),
            files: [(preset::CONFIG.to_owned(), text.clone().into_bytes())].into(),
        }),
        Err(e) => Err(e),
    }
}

/// Reviews the preset at `path` (the Use a preset… dialog, or `sliver-settings FILE.sliver`).
pub fn open_preset(ui: &Rc<Ui>, path: &Path) {
    match read_bundle(path) {
        Ok(b) => review_preset(ui, b),
        Err(e) => ui.toast(&e),
    }
}

fn export_preset(ui: &Rc<Ui>) {
    let Some(config) = sliver_config::find_config() else {
        return ui.toast("Nothing to export yet: you're on the built-in defaults");
    };
    let profile = profile::active();
    let name = if profile == profile::DEFAULT {
        "sliver-preset".to_owned()
    } else {
        profile
    };
    let d = gtk::FileDialog::builder()
        .title("Export preset")
        .initial_name(format!("{name}.{}", preset::EXTENSION))
        .build();
    let ui2 = ui.clone();
    d.save(Some(&ui.win_widget()), gio::Cancellable::NONE, move |r| {
        let Ok(f) = r else { return };
        let Some(dest) = f.path() else { return };
        let stem = dest
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let result = preset::export(&config, Some(&stem)).and_then(|x| {
            std::fs::write(&dest, x.bundle.to_text())
                .map(|()| x.notes)
                .map_err(|e| format!("{}: {e}", dest.display()))
        });
        match result {
            Ok(notes) if notes.is_empty() => ui2.toast(&format!("Exported to {}", dest.display())),
            Ok(notes) => {
                let dialog = adw::AlertDialog::builder()
                    .heading("Exported")
                    .body(format!("{}\n\n{}", dest.display(), notes.join("\n")))
                    .build();
                dialog.add_responses(&[("ok", "OK")]);
                dialog.present(Some(&ui2.win_widget()));
            }
            Err(e) => ui2.toast(&format!("Couldn't export: {e}")),
        }
    });
}

/// Shows everything the preset would run (ux.md §9) and installs it only after a choice.
/// A free profile name from a preset's name: `My desk` → `my-desk`, then `my-desk-2`, ….
fn profile_name_for(preset_name: &str) -> String {
    let mut base: String = preset_name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    base = base
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    base.truncate(56);
    if base.is_empty() || base == profile::DEFAULT {
        base = "preset".into();
    }
    let taken = profile::list();
    (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|n| !taken.contains(n))
        .expect("some name is free")
}

/// Shows everything the preset would run (ux.md §9) and installs it only after a choice. By
/// default it becomes a new profile, so trying one never overwrites your setup.
fn review_preset(ui: &Rc<Ui>, b: Bundle) {
    let runs = b.runs();
    let name = b.name.clone().unwrap_or_else(|| "this preset".into());
    let files: Vec<&str> = b.files.keys().map(String::as_str).collect();
    let body = format!(
        "{} file{}: {}.{}",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        files.join(", "),
        if runs.is_empty() {
            " It runs no commands.".to_owned()
        } else {
            " It runs these as you. Read them before choosing Use it, or import it without them:"
                .to_owned()
        }
    );
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Use {name}?"))
        .body(body)
        .build();
    let extra = gtk::Box::new(gtk::Orientation::Vertical, 12);
    if !runs.is_empty() {
        let list = gtk::Label::builder()
            .label(
                runs.iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .xalign(0.0)
            .selectable(true)
            .wrap(true)
            .css_classes(["monospace", "caption"])
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .min_content_height(80)
            .max_content_height(220)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        extra.append(&scroll);
    }
    let as_profile = gtk::CheckButton::builder()
        .label("As a new profile (keeps your current setup)")
        .active(true)
        .build();
    let profile_name = gtk::Entry::builder()
        .text(profile_name_for(&name))
        .placeholder_text("profile name")
        .build();
    as_profile
        .bind_property("active", &profile_name, "sensitive")
        .sync_create()
        .build();
    extra.append(&as_profile);
    extra.append(&profile_name);
    dialog.set_extra_child(Some(&extra));
    if runs.is_empty() {
        dialog.add_responses(&[("cancel", "Cancel"), ("use", "Use it")]);
        dialog.set_response_appearance("use", adw::ResponseAppearance::Suggested);
    } else {
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("safe", "Without commands"),
            ("use", "Use it"),
        ]);
        dialog.set_response_appearance("use", adw::ResponseAppearance::Destructive);
    }
    dialog.set_default_response(Some("cancel"));
    let ui2 = ui.clone();
    dialog.connect_response(None, move |_, r| {
        let b = match r {
            "use" => b.clone(),
            "safe" => b.without_commands().0,
            _ => return,
        };
        let target = as_profile
            .is_active()
            .then(|| profile_name.text().trim().to_owned());
        install_preset(&ui2, &b, &name, target);
    });
    dialog.present(Some(&ui.win_widget()));
}

/// Installs as the main config (backed up), or as a new profile `profiles/<p>.toml` with its
/// other files in `profiles/<p>/` (as `sliverctl preset import --profile`). Either way it
/// becomes active, with Undo.
fn install_preset(ui: &Rc<Ui>, b: &Bundle, name: &str, as_profile: Option<String>) {
    let Some(dir) = profile::user_dir() else {
        return ui.toast("No home directory to install into");
    };
    let left = profile::active();
    let undo: Box<dyn Fn() -> Result<(), String>>;
    let msg;
    match as_profile {
        Some(p) => {
            if let Err(e) = profile::valid_name(&p) {
                return ui.toast(&e);
            }
            let Some(config) = profile::path(&p) else {
                return ui.toast("\"default\" is your main config");
            };
            if config.exists() {
                return ui.toast(&format!("There's already a profile {p}"));
            }
            let files = config.with_extension("");
            if let Err(e) = b.install_at(&config, &files, &dir) {
                return ui.toast(&format!("Not applied: {e}"));
            }
            if let Err(e) = profile::set_active(&p) {
                return ui.toast(&e);
            }
            msg = format!("Using {name} as profile {p}");
            let left = left.clone();
            undo = Box::new(move || {
                profile::set_active(&left)?;
                profile::remove(&p)?;
                match std::fs::remove_dir_all(&files) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        Err(format!("{}: {e}", files.display()))
                    }
                    _ => Ok(()),
                }
            });
        }
        None => {
            let installed = match b.install(&dir) {
                Ok(i) => i,
                Err(e) => return ui.toast(&format!("Not applied: {e}")),
            };
            // A preset installed as the main config: an active profile would hide it.
            let switched =
                left != profile::DEFAULT && profile::set_active(profile::DEFAULT).is_ok();
            msg = if switched {
                format!("Using {name} (switched from {left} to Default)")
            } else {
                format!("Using {name}")
            };
            let left = left.clone();
            undo = Box::new(move || {
                undo_install(&dir, &installed)?;
                profile::set_active(&left)
            });
        }
    }
    let toast = adw::Toast::builder()
        .title(glib::markup_escape_text(&msg))
        .button_label("Undo")
        .timeout(8)
        .build();
    let ui2 = ui.clone();
    toast.connect_button_clicked(move |_| {
        match undo() {
            Ok(()) => ui2.toast("Restored your previous setup"),
            Err(e) => ui2.toast(&format!("Couldn't undo: {e}")),
        }
        crate::rebuild(&ui2);
    });
    ui.add_toast(toast);
    crate::rebuild(ui);
}

/// Puts back what [`Bundle::install`] replaced: backed-up files return, new ones go.
fn undo_install(dir: &Path, i: &preset::Installed) -> Result<(), String> {
    // config.toml last, so the agent reloads a complete set (as install does).
    let mut written = i.written.clone();
    written.sort_by_key(|p| p.ends_with(preset::CONFIG));
    for w in &written {
        let rel = w.strip_prefix(dir).unwrap_or(w);
        let saved = i
            .backup
            .as_ref()
            .map(|b| b.join(rel))
            .filter(|p| p.is_file());
        let r = match saved {
            Some(s) => std::fs::copy(&s, w).map(|_| ()),
            None => std::fs::remove_file(w),
        };
        r.map_err(|e| format!("{}: {e}", w.display()))?;
    }
    Ok(())
}
