//! `sliver-settings`: the settings app and WYSIWYG Touch Bar editor (GTK4 + libadwaita).
//!
//! First slice of docs/ux.md (T34): the §3 pages, a read-only Bar page with the real render,
//! Layouts & Rules, and Status. Editing comes next and goes through `sliver_config::Editor`.

mod preview;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use sliver_config::{Condition, Context, CtxValue, Hardware, Loaded, Matcher, Width};

use preview::{BAR, Preview};

/// D-Bus-style app id under the project prefix (H1).
const APP_ID: &str = "io.github.wawow830.Sliver.Settings";

struct State {
    path: Option<PathBuf>,
    loaded: Loaded,
    hw: Hardware,
    sheet: sliver_render::Stylesheet,
    problems: sliver_config::Diagnostics,
    style_problems: Vec<String>,
}

impl State {
    fn load() -> State {
        let path = sliver_config::find_config();
        let loaded = match &path {
            Some(p) => sliver_config::load_file(p),
            None => sliver_config::load(),
        };
        let hw = Hardware::detect();
        let mut problems = loaded.diagnostics.clone();
        if problems.ok() {
            problems.extend(loaded.config.validate(hw));
        }
        let dir = path.as_deref().and_then(|p| p.parent());
        let (sheet, style_problems) = Preview::stylesheet(&loaded.config, dir);
        State {
            path,
            loaded,
            hw,
            sheet,
            problems,
            style_problems,
        }
    }
}

fn main() -> glib::ExitCode {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("sliver-settings {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(|app| {
        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Sliver")
            .default_width(1180)
            .default_height(760)
            .build();
        rebuild(&win);
        win.present();
    });
    app.run()
}

/// (Re)builds the whole window from a fresh config load.
fn rebuild(win: &adw::ApplicationWindow) {
    let state = Rc::new(State::load());
    let preview = Rc::new(RefCell::new(Preview::new(
        state.loaded.config.clone(),
        state.sheet.clone(),
    )));
    let pages: Vec<(&str, &str, &str, gtk::Widget)> = vec![
        (
            "bar",
            "Bar",
            "input-keyboard-symbolic",
            bar_page(&state, &preview),
        ),
        (
            "rules",
            "Layouts & Rules",
            "view-list-symbolic",
            rules_page(&state),
        ),
        (
            "gestures",
            "Gestures",
            "input-touchpad-symbolic",
            todo_page(
                "Gestures",
                "Two-finger swipes, long-press timing, double-tap, and whether taps fire on press or release.",
                &state,
            ),
        ),
        (
            "appearance",
            "Appearance",
            "applications-graphics-symbolic",
            todo_page(
                "Appearance",
                "Theme, accent, font, icon theme, outlines and animations. They're all a stylesheet underneath (style.css).",
                &state,
            ),
        ),
        (
            "display",
            "Display & Power",
            "display-brightness-symbolic",
            display_page(&state),
        ),
        (
            "apps",
            "Apps",
            "application-x-executable-symbolic",
            todo_page(
                "Apps",
                "Apps that put buttons on the bar: allow or deny each one, and hide or pin their items.",
                &state,
            ),
        ),
        (
            "profiles",
            "Profiles & Presets",
            "folder-symbolic",
            todo_page(
                "Profiles & Presets",
                "Switch profiles, import your tiny-dfr setup (`sliverctl config import-tiny-dfr` works today), and export or import presets.",
                &state,
            ),
        ),
        (
            "status",
            "Status",
            "dialog-information-symbolic",
            status_page(&state),
        ),
    ];

    let stack = gtk::Stack::new();
    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    for (id, title, icon, widget) in &pages {
        stack.add_titled(widget, Some(id), title);
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_box.append(&gtk::Image::from_icon_name(icon));
        row_box.append(&gtk::Label::new(Some(title)));
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));
        row.set_widget_name(id);
        sidebar.append(&row);
    }
    let problems = state.problems.errors.len();
    if problems > 0 {
        stack.set_visible_child_name("status");
    }

    let content_page = adw::NavigationPage::new(&toolbar(&stack, None), "Bar");
    let split = adw::NavigationSplitView::new();
    {
        let (stack, content_page, split) = (stack.clone(), content_page.clone(), split.clone());
        sidebar.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let id = row.widget_name();
            stack.set_visible_child_name(&id);
            if let Some(child) = stack.child_by_name(&id) {
                content_page.set_title(&stack.page(&child).title().unwrap_or_default());
            }
            split.set_show_content(true);
        });
    }
    let start = if problems > 0 { pages.len() - 1 } else { 0 };
    sidebar.select_row(sidebar.row_at_index(start as i32).as_ref());

    let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
    reload.set_tooltip_text(Some("Reload config"));
    {
        let win = win.clone();
        reload.connect_clicked(move |_| rebuild(&win));
    }
    let sidebar_page = adw::NavigationPage::new(&toolbar(&sidebar, Some(&reload)), "Sliver");
    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(220.0);
    win.set_content(Some(&split));
}

fn toolbar(content: &impl IsA<gtk::Widget>, start: Option<&gtk::Button>) -> adw::ToolbarView {
    let tv = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    if let Some(b) = start {
        header.pack_start(b);
    }
    tv.add_top_bar(&header);
    tv.set_content(Some(content));
    tv
}

fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .use_markup(false)
        .build()
}

fn group(title: &str, description: Option<&str>) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::new();
    g.set_title(&glib::markup_escape_text(title));
    if let Some(d) = description {
        g.set_description(Some(&glib::markup_escape_text(d)));
    }
    g
}

fn config_location(state: &State) -> String {
    state
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "built-in defaults (no config file yet)".into())
}

// ---- Bar --------------------------------------------------------------------------------------

fn bar_page(state: &Rc<State>, preview: &Rc<RefCell<Preview>>) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_margin_top(18);
    page.set_margin_bottom(18);
    page.set_margin_start(18);
    page.set_margin_end(18);

    let config = &state.loaded.config;
    let layers: Vec<String> = config.layers.keys().cloned().collect();
    let layer_names: Vec<&str> = layers.iter().map(String::as_str).collect();
    let layer_dd = gtk::DropDown::from_strings(&layer_names);
    if let Some(i) = layers.iter().position(|l| l == "base") {
        layer_dd.set_selected(i as u32);
    }
    let app_entry = gtk::Entry::builder()
        .placeholder_text("app id, e.g. firefox")
        .width_chars(22)
        .build();
    let locked = gtk::Switch::builder().valign(gtk::Align::Center).build();

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.append(&gtk::Label::new(Some("Layer")));
    controls.append(&layer_dd);
    controls.append(
        &gtk::Label::builder()
            .label("Preview as")
            .margin_start(16)
            .build(),
    );
    controls.append(&app_entry);
    controls.append(
        &gtk::Label::builder()
            .label("Locked")
            .margin_start(16)
            .build(),
    );
    controls.append(&locked);
    page.append(&controls);

    // Half scale (1004x30) stays legible; narrower windows scroll sideways instead of shrinking it.
    // Drawn at half scale (1004x30) so it stays legible; narrower windows scroll sideways.
    let frame: Rc<RefCell<Option<gtk::cairo::ImageSurface>>> = Rc::default();
    let picture = gtk::DrawingArea::builder()
        .content_width(BAR.0 as i32 / 2)
        .content_height(BAR.1 as i32 / 2)
        .halign(gtk::Align::Center)
        .build();
    {
        let frame = frame.clone();
        picture.set_draw_func(move |_, cr, _, _| {
            if let Some(surface) = frame.borrow().as_ref() {
                cr.scale(0.5, 0.5);
                let _ = cr.set_source_surface(surface, 0.0, 0.0);
                cr.source().set_filter(gtk::cairo::Filter::Best);
                let _ = cr.paint();
            }
        });
    }
    let bar_scroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&picture)
        .build();
    page.append(&bar_scroll);

    let why = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    why.add_css_class("dim-label");
    page.append(&why);

    let zones = gtk::Box::new(gtk::Orientation::Vertical, 18);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&zones)
        .build();
    page.append(&scroll);

    let refresh = {
        let (state, preview) = (state.clone(), preview.clone());
        let frame = frame.clone();
        let (layer_dd, app_entry, locked, picture, why, zones) = (
            layer_dd.clone(),
            app_entry.clone(),
            locked.clone(),
            picture.clone(),
            why.clone(),
            zones.clone(),
        );
        let layers = layers.clone();
        Rc::new(move || {
            let layer = layers
                .get(layer_dd.selected() as usize)
                .cloned()
                .unwrap_or_else(|| "base".into());
            let mut ctx = Context::new()
                .with("physical_esc", state.hw.physical_esc)
                .with("locked", locked.is_active());
            let app = app_entry.text();
            if !app.is_empty() {
                ctx.set("app_id", app.as_str());
            }
            while let Some(c) = zones.first_child() {
                zones.remove(&c);
            }
            let config = &state.loaded.config;
            match config.resolve(&layer, &ctx) {
                Ok(r) => {
                    match preview.borrow_mut().render(&layer, ctx.clone()) {
                        Ok(pixmap) => *frame.borrow_mut() = surface(&pixmap),
                        Err(e) => {
                            *frame.borrow_mut() = None;
                            zones.append(&row("Can't render", &e));
                        }
                    }
                    picture.queue_draw();
                    why.set_text(&match r.rule {
                        Some(i) => format!(
                            "Showing layout “{}”: rules[{i}]{} matched.",
                            r.layout,
                            config.rules[i]
                                .name
                                .as_deref()
                                .map(|n| format!(" ({n})"))
                                .unwrap_or_default()
                        ),
                        None => format!(
                            "Showing layout “{}”, the {layer} layer's default; no rule matched.",
                            r.layout
                        ),
                    });
                    for z in &r.zones {
                        let g = group(
                            &format!("Zone “{}”", z.name),
                            Some(&format!("width {}", width_text(z.width))),
                        );
                        if z.modules.is_empty() {
                            g.add(&row("(empty)", "nothing shows here in this context"));
                        }
                        for m in &z.modules {
                            let kids = if m.children.is_empty() {
                                String::new()
                            } else {
                                format!(" · {} inside", m.children.len())
                            };
                            g.add(&row(
                                &m.id.to_string(),
                                &format!("{} · {}{kids}", m.id.kind, width_text(m.common.width)),
                            ));
                        }
                        zones.append(&g);
                    }
                }
                Err(e) => {
                    *frame.borrow_mut() = None;
                    picture.queue_draw();
                    why.set_text(&format!("Can't show this layer: {e}"));
                }
            }
        })
    };
    {
        let r = refresh.clone();
        layer_dd.connect_selected_notify(move |_| r());
    }
    {
        let r = refresh.clone();
        app_entry.connect_changed(move |_| r());
    }
    {
        let r = refresh.clone();
        locked.connect_active_notify(move |_| r());
    }
    refresh();
    page.upcast()
}

/// tiny-skia pixels (premultiplied RGBA) as a cairo surface (premultiplied ARGB32, native endian).
fn surface(p: &sliver_render::Pixmap) -> Option<gtk::cairo::ImageSurface> {
    let mut data = Vec::with_capacity(p.data().len());
    for px in p.data().chunks_exact(4) {
        let argb = u32::from_be_bytes([px[3], px[0], px[1], px[2]]);
        data.extend_from_slice(&argb.to_ne_bytes());
    }
    let (w, h) = (p.width() as i32, p.height() as i32);
    gtk::cairo::ImageSurface::create_for_data(data, gtk::cairo::Format::ARgb32, w, h, w * 4).ok()
}

fn width_text(w: Width) -> String {
    match w {
        Width::Auto => "auto".into(),
        Width::Fixed(px) => format!("{px} px"),
        Width::Fill(1) => "fill".into(),
        Width::Fill(n) => format!("fill ×{n}"),
    }
}

// ---- Layouts & Rules --------------------------------------------------------------------------

fn rules_page(state: &State) -> gtk::Widget {
    let c = &state.loaded.config;
    let page = adw::PreferencesPage::new();

    let layers = group(
        "Layers",
        Some("Each layer's default layout. Fn shows the fn layer; other layers open from actions."),
    );
    for (layer, layout) in &c.layers {
        layers.add(&row(layer, &format!("→ {layout}")));
    }
    page.add(&layers);

    let rules = group(
        "Rules",
        Some(
            "Checked top to bottom; the first match wins. With no match, the layer's default layout is used.",
        ),
    );
    if c.rules.is_empty() {
        rules.add(&row("No rules yet", "Every app gets the default layouts."));
    }
    for (i, r) in c.rules.iter().enumerate() {
        let title = r.name.clone().unwrap_or_else(|| format!("rules[{i}]"));
        rules.add(&row(
            &title,
            &format!(
                "when {} → {} ({} layer)",
                summarize(&r.when),
                r.layout,
                r.layer
            ),
        ));
    }
    page.add(&rules);

    let layouts = group("Layouts", None);
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

fn summarize(c: &Condition) -> String {
    if c.is_always() {
        return "always".into();
    }
    let lit = |v: &CtxValue| match v {
        CtxValue::Bool(b) => b.to_string(),
        CtxValue::Num(n) => n.to_string(),
        CtxValue::Str(s) => format!("“{s}”"),
    };
    let mut parts: Vec<String> = c
        .all
        .iter()
        .map(|(k, m)| match m {
            Matcher::Is(v) => format!("{k} = {}", lit(v)),
            Matcher::OneOf(vs) => format!(
                "{k} is one of {}",
                vs.iter().map(lit).collect::<Vec<_>>().join(", ")
            ),
            Matcher::Regex(_) => format!("{k} matches a pattern"),
            Matcher::Contains(s) => format!("{k} contains “{s}”"),
            Matcher::Below(n) => format!("{k} < {n}"),
            Matcher::Above(n) => format!("{k} > {n}"),
            Matcher::Between(a, b) => format!("{k} between {} and {}", lit(a), lit(b)),
        })
        .collect();
    if !c.any.is_empty() {
        parts.push(format!(
            "any of ({})",
            c.any.iter().map(summarize).collect::<Vec<_>>().join(" | ")
        ));
    }
    if let Some(n) = &c.not {
        parts.push(format!("not ({})", summarize(n)));
    }
    parts.join(" and ")
}

// ---- Display & Power, Status, placeholders ----------------------------------------------------

fn display_page(state: &State) -> gtk::Widget {
    let d = &state.loaded.config.display;
    let page = adw::PreferencesPage::new();
    let g = group(
        "Brightness and idle",
        Some(
            "Read-only for now. Edit [display] in config.toml, or use `sliverctl config set display.dim-after 20s`.",
        ),
    );
    let secs = |t: Option<std::time::Duration>| {
        t.map(|t| format!("{} s", t.as_secs()))
            .unwrap_or_else(|| "never".into())
    };
    g.add(&row(
        "Brightness",
        if d.adaptive {
            "adaptive (follows the main display)"
        } else {
            "fixed"
        },
    ));
    g.add(&row(
        "Maximum brightness",
        &format!("{} / 255", d.max_brightness),
    ));
    g.add(&row("Dim after", &secs(d.dim_after)));
    g.add(&row("Turn off after", &secs(d.off_after)));
    g.add(&row(
        "Pixel shift",
        if d.pixel_shift { "on" } else { "off" },
    ));
    page.add(&g);
    page.upcast()
}

fn status_page(state: &State) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let cfg = group("Config", None);
    cfg.add(&row("File", &config_location(state)));
    let p = &state.problems;
    cfg.add(&row(
        "Result",
        &if p.ok() {
            format!("valid · {} warnings", p.warnings.len())
        } else {
            format!(
                "{} errors: the bar keeps its last good config until they're fixed",
                p.errors.len()
            )
        },
    ));
    cfg.add(&row("Watched files", &state.loaded.files.len().to_string()));
    page.add(&cfg);

    let hw = group("Hardware", None);
    hw.add(&row(
        "Escape key",
        if state.hw.physical_esc {
            "physical Esc next to the bar: the bar's Esc is optional"
        } else {
            "no physical Esc: the default layouts put Esc on the bar"
        },
    ));
    hw.add(&row(
        "Safe hatch",
        "double-press Fn and hold: built-in F-keys, whatever the config says",
    ));
    page.add(&hw);

    if !(p.errors.is_empty() && p.warnings.is_empty() && state.style_problems.is_empty()) {
        let probs = group("Problems", None);
        let add = |icon: &str, title: &str, text: &str| {
            let r = row(title, text);
            r.add_prefix(&gtk::Image::from_icon_name(icon));
            probs.add(&r);
        };
        for e in &p.errors {
            add(
                "dialog-error-symbolic",
                if e.path.is_empty() { "config" } else { &e.path },
                &e.message,
            );
        }
        for w in &p.warnings {
            add(
                "dialog-warning-symbolic",
                if w.path.is_empty() { "config" } else { &w.path },
                &w.message,
            );
        }
        for s in &state.style_problems {
            add("dialog-warning-symbolic", "stylesheet", s);
        }
        page.add(&probs);
    }

    let svc = group(
        "Services",
        Some("Live agent/daemon status needs the D-Bus client (next slice)."),
    );
    svc.add(&row("sliver-agent", "not checked yet"));
    svc.add(&row("sliverd", "not checked yet"));
    page.add(&svc);
    page.upcast()
}

fn todo_page(title: &str, what: &str, state: &State) -> gtk::Widget {
    adw::StatusPage::builder()
        .icon_name("document-edit-symbolic")
        .title(title)
        .description(glib::markup_escape_text(&format!(
            "{what}\n\nNot editable here yet. Everything is in your config file:\n{}",
            config_location(state)
        )))
        .build()
        .upcast()
}
