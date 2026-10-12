//! Simple settings pages (ux.md §3): Display & Power, Appearance, Gestures. Every control writes
//! one config key through `apply` (validated, instant, undoable).

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{BAR_GESTURES, parse_cli_value, writer::inline};
use toml::Value;

use crate::pages::{group, row};
use crate::preview::{BAR, Preview};
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
    g.add(&crate::curve::editor(ui, d));
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

    // [display.on-battery] (T68): each value overrides the one above while on battery.
    let o = &d.on_battery;
    let bat = group(
        "On battery",
        Some(
            "Used instead of the values above while on battery. Leave one empty to keep it the same.",
        ),
    );
    entry(
        ui,
        &bat,
        "Maximum brightness (0-255)",
        "display.on-battery.max-brightness",
        &o.max_brightness.map(|b| b.to_string()).unwrap_or_default(),
        false,
    );
    entry(
        ui,
        &bat,
        "Dim after",
        "display.on-battery.dim-after",
        &o.dim_after.map(dur).unwrap_or_default(),
        true,
    );
    entry(
        ui,
        &bat,
        "Turn off after",
        "display.on-battery.off-after",
        &o.off_after.map(dur).unwrap_or_default(),
        true,
    );
    page.add(&bat);
    page.upcast()
}

const THEMES: &[(&str, &str, &str)] = &[
    (
        "sliver",
        "Sliver",
        "The macOS look, with your desktop's accent",
    ),
    (
        "light",
        "Light",
        "Dark on light keys. Lit pixels use more power on the OLED bar",
    ),
    (
        "high-contrast",
        "High contrast",
        "Outlined, bold and large; states inverted or yellow",
    ),
    ("classic", "Classic", "tiny-dfr's look"),
];

/// One theme choice: its name, and the user's own base layer drawn in it (with their stylesheet
/// on top), so the choice is seen rather than guessed.
fn theme_row(ui: &Rc<Ui>, id: &'static str, name: &str, what: &str, current: &str) -> gtk::Widget {
    let state = ui.state();
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let title = gtk::Label::builder().label(name).xalign(0.0).build();
    let sub = gtk::Label::builder()
        .label(what)
        .xalign(0.0)
        .wrap(true)
        .build();
    sub.add_css_class("dim-label");
    sub.add_css_class("caption");
    text.append(&title);
    text.append(&sub);
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    head.append(&text);
    if id == current {
        head.append(&gtk::Image::from_icon_name("object-select-symbolic"));
    }
    let col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(10)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    col.append(&head);

    let mut config = state.loaded.config.clone();
    config.theme = Some(id.to_owned());
    let (sheet, _) = Preview::stylesheet(&config, state.style_dir.as_deref());
    let ctx = sliver_config::Context::new().with("physical_esc", state.hw.physical_esc);
    if let Ok(p) = Preview::new(config, sheet).render("base", ctx)
        && let Some(s) = crate::pages::surface(&p)
    {
        // A quarter of the bar's pixel size, at the panel's aspect ratio.
        let area = gtk::DrawingArea::builder()
            .content_width((BAR.0 / 4) as i32)
            .content_height((BAR.1 / 4) as i32)
            .halign(gtk::Align::Start)
            .build();
        area.set_draw_func(move |_, cr, _, _| {
            cr.scale(0.25, 0.25);
            let _ = cr.set_source_surface(&s, 0.0, 0.0);
            cr.source().set_filter(gtk::cairo::Filter::Best);
            let _ = cr.paint();
        });
        col.append(&area);
    }

    let row = adw::PreferencesRow::builder()
        .title(name)
        .activatable(id != current)
        .child(&col)
        .build();
    if id != current {
        let choose = {
            let ui = ui.clone();
            move || {
                set_key(
                    &ui,
                    "theme",
                    "Theme".into(),
                    Some(Value::String(id.to_owned())),
                )
            }
        };
        let click = gtk::GestureClick::new();
        let c2 = choose.clone();
        click.connect_released(move |_, _, _, _| c2());
        row.add_controller(click);
        row.connect_activate(move |_| choose());
    }
    row.upcast()
}

/// The user's stylesheet: `stylesheet = …`, else `style.css` beside the config.
fn stylesheet_file(state: &crate::State, c: &sliver_config::Config) -> std::path::PathBuf {
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
    dir.join(c.stylesheet.as_deref().unwrap_or("style.css"))
}

/// Reads the managed look from `file`, lets `f` change it, and writes it back (the rest of the
/// file untouched). The config directory is watched, so the window and preview refresh.
fn change_look(ui: &Rc<Ui>, file: &std::path::Path, f: impl FnOnce(&mut crate::look::Look)) {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let mut look = crate::look::Look::parse(&text);
    f(&mut look);
    let new = look.splice(&text);
    if new == text {
        return;
    }
    let r = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(file, new));
    match r {
        Ok(()) => ui.toast("Look saved to your stylesheet"),
        Err(e) => ui.toast(&format!("Couldn't write {}: {e}", file.display())),
    }
}

/// Sets one field of the look from a text field's text, or says why not.
type LookSetter = Rc<dyn Fn(&mut crate::look::Look, &str) -> Result<(), String>>;

/// Font, corner radius, outlines, accent, animations and press feedback, as a managed block in
/// the stylesheet (look.rs).
fn look_group(ui: &Rc<Ui>, file: &std::path::Path) -> adw::PreferencesGroup {
    use crate::look::{Look, Press};
    let look = std::fs::read_to_string(file)
        .map(|t| Look::parse(&t))
        .unwrap_or_default();
    let g = group(
        "Look",
        Some(
            "Saved as a block at the top of your stylesheet; rules you write below it still win. Empty = the theme's own.",
        ),
    );
    let text_row = |title: &str, current: String, apply: LookSetter| {
        let e = adw::EntryRow::builder()
            .title(title)
            .show_apply_button(true)
            .build();
        e.set_text(&current);
        let (ui, file) = (ui.clone(), file.to_path_buf());
        e.connect_apply(move |e| {
            let t = e.text().trim().to_owned();
            // Check on a scratch copy first, so a bad value is an error and not half a write.
            if let Err(msg) = apply(&mut Look::default(), &t) {
                ui.toast(&msg);
                return;
            }
            change_look(&ui, &file, |l| {
                let _ = apply(l, &t);
            });
        });
        e
    };
    g.add(&text_row(
        "Font family",
        look.font_family.clone().unwrap_or_default(),
        Rc::new(|l, t| {
            if t.is_empty() {
                l.font_family = None;
                return Ok(());
            }
            Look::check_family(t)?;
            l.font_family = Some(t.to_owned());
            Ok(())
        }),
    ));
    let number =
        |min: u32, max: u32, what: &'static str, set: fn(&mut Look, Option<u32>)| -> LookSetter {
            Rc::new(move |l, t| {
                if t.is_empty() {
                    set(l, None);
                    return Ok(());
                }
                let n: u32 = t
                    .trim_end_matches("px")
                    .trim()
                    .parse()
                    .map_err(|_| format!("{what} is a number of pixels"))?;
                if !(min..=max).contains(&n) {
                    return Err(format!("{what} must be between {min} and {max} px"));
                }
                set(l, Some(n));
                Ok(())
            })
        };
    g.add(&text_row(
        "Font size (px)",
        look.font_size.map(|n| n.to_string()).unwrap_or_default(),
        number(10, 48, "Font size", |l, n| l.font_size = n),
    ));
    g.add(&text_row(
        "Corner radius (px)",
        look.radius.map(|n| n.to_string()).unwrap_or_default(),
        number(0, 30, "Corner radius", |l, n| l.radius = n),
    ));
    g.add(&text_row(
        "Accent colour override (#rrggbb)",
        look.accent.clone().unwrap_or_default(),
        Rc::new(|l, t| {
            if t.is_empty() {
                l.accent = None;
                return Ok(());
            }
            Look::check_accent(t)?;
            l.accent = Some(t.to_ascii_lowercase());
            Ok(())
        }),
    ));
    let switch = |title: &str, subtitle: &str, on: bool, set: fn(&mut Look, bool)| {
        let r = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(on)
            .build();
        let (ui, file) = (ui.clone(), file.to_path_buf());
        r.connect_active_notify(move |r| {
            let on = r.is_active();
            change_look(&ui, &file, |l| set(l, on));
        });
        r
    };
    g.add(&switch(
        "Outlines",
        "A light border around every key",
        look.outlines,
        |l, on| l.outlines = on,
    ));
    g.add(&switch(
        "Turn animations off",
        "No fades or slides. Your desktop's reduce-motion setting already does this.",
        look.animations_off,
        |l, on| l.animations_off = on,
    ));
    let model = gtk::StringList::new(&[]);
    for (_, _, name) in Press::ALL {
        model.append(name);
    }
    let combo = adw::ComboRow::builder()
        .title("Press feedback")
        .model(&model)
        .selected(
            Press::ALL
                .iter()
                .position(|(p, ..)| *p == look.press)
                .unwrap_or(0) as u32,
        )
        .build();
    {
        let (ui, file) = (ui.clone(), file.to_path_buf());
        combo.connect_selected_notify(move |c| {
            if let Some((p, ..)) = Press::ALL.get(c.selected() as usize) {
                change_look(&ui, &file, |l| l.press = *p);
            }
        });
    }
    g.add(&combo);
    g
}

pub fn appearance_page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let c = &state.loaded.config;
    let page = adw::PreferencesPage::new();
    let g = group(
        "Theme",
        Some("The base look. Your stylesheet goes on top of it; the accent follows your desktop."),
    );
    let current = c.theme.as_deref().unwrap_or("sliver");
    for (id, name, what) in THEMES {
        g.add(&theme_row(ui, id, name, what, current));
    }
    page.add(&g);

    let file = stylesheet_file(&state, c);
    page.add(&look_group(ui, &file));

    let css = group(
        "Stylesheet",
        Some(
            "waybar-style CSS: #clock, #battery.critical, .zone-main, :active, @define-color accent … (customization.md §7).",
        ),
    );
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
