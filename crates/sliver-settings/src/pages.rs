//! The read-only pages (ux.md §3) and small widget helpers.

use adw::prelude::*;
use gtk::glib;
use sliver_config::{Condition, CtxValue, Matcher, Width};

use crate::State;

pub(crate) fn row(title: &str, subtitle: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .use_markup(false)
        .build()
}

pub(crate) fn group(title: &str, description: Option<&str>) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::new();
    g.set_title(&glib::markup_escape_text(title));
    if let Some(d) = description {
        g.set_description(Some(&glib::markup_escape_text(d)));
    }
    g
}

pub(crate) fn config_location(state: &State) -> String {
    state
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "built-in defaults (no config file yet)".into())
}

/// tiny-skia pixels (premultiplied RGBA) as a cairo surface (premultiplied ARGB32, native endian).
pub(crate) fn surface(p: &sliver_render::Pixmap) -> Option<gtk::cairo::ImageSurface> {
    let mut data = Vec::with_capacity(p.data().len());
    for px in p.data().chunks_exact(4) {
        let argb = u32::from_be_bytes([px[3], px[0], px[1], px[2]]);
        data.extend_from_slice(&argb.to_ne_bytes());
    }
    let (w, h) = (p.width() as i32, p.height() as i32);
    gtk::cairo::ImageSurface::create_for_data(data, gtk::cairo::Format::ARgb32, w, h, w * 4).ok()
}

pub(crate) fn width_text(w: Width) -> String {
    match w {
        Width::Auto => "auto".into(),
        Width::Fixed(px) => format!("{px} px"),
        Width::Fill(1) => "fill".into(),
        Width::Fill(n) => format!("fill ×{n}"),
    }
}

pub(crate) fn summarize(c: &Condition) -> String {
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

pub(crate) fn display_page(state: &State) -> gtk::Widget {
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

pub(crate) fn status_page(state: &State) -> gtk::Widget {
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

pub(crate) fn todo_page(title: &str, what: &str, state: &State) -> gtk::Widget {
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
