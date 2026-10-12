//! Simple settings pages (ux.md §3): Display & Power, Appearance, Gestures. Every control writes
//! one config key through `apply` (validated, instant, undoable).

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{BAR_GESTURES, parse_cli_value, writer::inline};
use toml::Value;

use crate::pages::{group, row};
use crate::{Ui, apply};

fn set_key(ui: &Rc<Ui>, path: impl Into<String>, label: String, value: Option<Value>) {
    let path: String = path.into();
    apply(ui, move |ed, _| match value {
        Some(v) => {
            ed.set(&path, &v)?;
            Ok(format!("{label}: {}", show(&v)))
        }
        None => {
            ed.remove(&path)?;
            Ok(format!("{label}: back to the default"))
        }
    });
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => inline(other),
    }
}

/// A text field. Empty = remove the key (back to the default). `as_string` keeps text as a TOML
/// string; otherwise the text is parsed as TOML when it can be (numbers, tables).
fn entry(
    ui: &Rc<Ui>,
    g: &adw::PreferencesGroup,
    title: &str,
    path: impl Into<String>,
    current: &str,
    as_string: bool,
) {
    let path: String = path.into();
    let e = adw::EntryRow::builder()
        .title(title)
        .show_apply_button(true)
        .build();
    e.set_text(current);
    let (ui, label) = (ui.clone(), title.to_owned());
    e.connect_apply(move |e| {
        let t = e.text().trim().to_owned();
        let v = (!t.is_empty()).then(|| {
            if as_string {
                Value::String(t.clone())
            } else {
                parse_cli_value(&t)
            }
        });
        set_key(&ui, path.clone(), label.clone(), v);
    });
    g.add(&e);
}

fn combo(
    ui: &Rc<Ui>,
    g: &adw::PreferencesGroup,
    title: &str,
    path: &'static str,
    options: &'static [(&'static str, &'static str)],
    current: &str,
) {
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    let c = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .build();
    c.set_selected(options.iter().position(|(v, _)| *v == current).unwrap_or(0) as u32);
    // Connected after set_selected, so building the page doesn't write anything.
    let (ui, label) = (ui.clone(), title.to_owned());
    c.connect_selected_notify(move |c| {
        if let Some((v, _)) = options.get(c.selected() as usize) {
            set_key(
                &ui,
                path,
                label.clone(),
                Some(Value::String((*v).to_owned())),
            );
        }
    });
    g.add(&c);
}

fn switch(
    ui: &Rc<Ui>,
    g: &adw::PreferencesGroup,
    title: &str,
    subtitle: &str,
    path: &'static str,
    current: bool,
) {
    let s = adw::SwitchRow::builder()
        .title(title)
        .subtitle(glib::markup_escape_text(subtitle))
        .active(current)
        .build();
    let (ui, label) = (ui.clone(), title.to_owned());
    s.connect_active_notify(move |s| {
        set_key(
            &ui,
            path,
            label.clone(),
            Some(Value::Boolean(s.is_active())),
        )
    });
    g.add(&s);
}

fn dur(t: Option<Duration>) -> String {
    match t {
        None => "never".into(),
        Some(t) if t.subsec_millis() == 0 => format!("{}s", t.as_secs()),
        Some(t) => format!("{}ms", t.as_millis()),
    }
}

pub fn display_page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let d = &state.loaded.config.display;
    let page = adw::PreferencesPage::new();
    let g = group(
        "Brightness",
        Some("Adaptive follows the main display's brightness, up to the maximum."),
    );
    combo(
        ui,
        &g,
        "Brightness",
        "display.brightness",
        &[("adaptive", "Adaptive"), ("fixed", "Fixed at the maximum")],
        if d.adaptive { "adaptive" } else { "fixed" },
    );
    entry(
        ui,
        &g,
        "Maximum brightness (0-255)",
        "display.max-brightness",
        &d.max_brightness.to_string(),
        false,
    );
    page.add(&g);
    let idle = group(
        "Idle",
        Some(
            "Durations like 30s or 2m, or never. Dimming and switching off also protect the OLED from burn-in.",
        ),
    );
    entry(
        ui,
        &idle,
        "Dim after",
        "display.dim-after",
        &dur(d.dim_after),
        true,
    );
    entry(
        ui,
        &idle,
        "Turn off after",
        "display.off-after",
        &dur(d.off_after),
        true,
    );
    switch(
        ui,
        &idle,
        "Pixel shift",
        "Slowly moves everything a few pixels (tiny-dfr's EnablePixelShift)",
        "display.pixel-shift",
        d.pixel_shift,
    );
    page.add(&idle);
    page.upcast()
}

const THEMES: &[(&str, &str)] = &[("sliver", "Sliver"), ("classic", "Classic (tiny-dfr)")];

pub fn appearance_page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let c = &state.loaded.config;
    let page = adw::PreferencesPage::new();
    let g = group(
        "Theme",
        Some("The base look. Your stylesheet goes on top of it; the accent follows your desktop."),
    );
    combo(
        ui,
        &g,
        "Theme",
        "theme",
        THEMES,
        c.theme.as_deref().unwrap_or("sliver"),
    );
    page.add(&g);

    let css = group(
        "Stylesheet",
        Some(
            "waybar-style CSS: #clock, #battery.critical, .zone-main, :active, @define-color accent … (customization.md §7).",
        ),
    );
    let dir = state
        .path
        .as_deref()
        .and_then(|p| p.parent())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| {
            state
                .edit_target()
                .0
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default()
        });
    let file = dir.join(c.stylesheet.as_deref().unwrap_or("style.css"));
    let open = adw::ActionRow::builder()
        .title("Edit the stylesheet")
        .subtitle(glib::markup_escape_text(&file.display().to_string()))
        .activatable(true)
        .build();
    open.add_suffix(&gtk::Image::from_icon_name("document-edit-symbolic"));
    {
        let (ui, file) = (ui.clone(), file.clone());
        open.connect_activated(move |_| {
            if !file.exists() {
                let _ = std::fs::create_dir_all(file.parent().unwrap_or(std::path::Path::new(".")));
                let _ = std::fs::write(
                    &file,
                    "/* Your overrides on top of the theme. Saved changes apply at once. */\n",
                );
            }
            let launcher = gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(&file)));
            let ui2 = ui.clone();
            launcher.launch(
                Some(&ui.win_widget()),
                gtk::gio::Cancellable::NONE,
                move |r| {
                    if let Err(e) = r {
                        ui2.toast(&format!("Couldn't open an editor: {e}"));
                    }
                },
            );
        });
    }
    css.add(&open);
    if state.style_problems.is_empty() {
        css.add(&row("Problems", "none"));
    }
    for p in &state.style_problems {
        let r = row("Problem", p);
        r.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        css.add(&r);
    }
    page.add(&css);
    page.upcast()
}

pub fn gestures_page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let c = &state.loaded.config;
    let page = adw::PreferencesPage::new();
    let g = group("Touch", None);
    combo(
        ui,
        &g,
        "Taps fire",
        "bar.fire-on",
        &[("release", "On lift"), ("press", "On touch")],
        match c.bar.fire_on {
            sliver_config::FireOn::Press => "press",
            sliver_config::FireOn::Release => "release",
        },
    );
    combo(
        ui,
        &g,
        "Fn key",
        "bar.fn-mode",
        &[("hold", "Hold"), ("toggle", "Toggle")],
        match c.bar.fn_mode {
            sliver_config::FnMode::Hold => "hold",
            sliver_config::FnMode::Toggle => "toggle",
        },
    );
    page.add(&g);
    let bar = group(
        "Whole-bar gestures",
        Some(
            "A shell command, or TOML for typed actions: { key = \"VolumeUp\" }, { layer = \"media\" }. Empty = nothing.",
        ),
    );
    let raw = state
        .loaded
        .merged
        .get("gestures")
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    for name in BAR_GESTURES {
        let path = format!("gestures.{name}");
        let current = raw.get(*name).map(show).unwrap_or_default();
        entry(ui, &bar, &name.replace('-', " "), path, &current, false);
    }
    page.add(&bar);
    page.upcast()
}
