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
        Some("From the session bus; the reload button at the top left refreshes it."),
    );
    let agent_row = row("sliver-agent", "checking…");
    let daemon_row = row("sliverd", "checking…");
    svc.add(&agent_row);
    svc.add(&daemon_row);
    page.add(&svc);
    // The agent's own list of problems (Control1.Errors), when it has any.
    let reported = group(
        "The agent reports",
        Some("Problems the running agent found. It keeps showing the last good setup meanwhile."),
    );
    reported.set_visible(false);
    page.add(&reported);
    // Config and stylesheet problems are already under Problems (this app checks the same
    // files): only the agent's own (modules, runtime) are new here.
    let ours: Vec<String> = state
        .loaded
        .files
        .iter()
        .map(|f| f.display().to_string())
        .collect();
    let already_shown = move |e: &str| {
        let head = e.split(':').next().unwrap_or("");
        ours.iter().any(|f| e.starts_with(f.as_str())) || head.ends_with(".css")
    };
    crate::live::agent_status(move |a| match a {
        crate::live::Agent::Running {
            healthy,
            errors,
            layer,
            display,
            version,
        } => {
            let all = errors.len();
            let errors: Vec<&String> = errors.iter().filter(|e| !already_shown(e)).collect();
            for e in &errors {
                let r = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(e))
                    .title_lines(0)
                    .build();
                r.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
                reported.add(&r);
            }
            reported.set_visible(!errors.is_empty());
            agent_row.set_subtitle(&glib::markup_escape_text(&format!(
                "running{} · {} · showing the {layer} layer",
                if version.is_empty() {
                    String::new()
                } else {
                    format!(" ({version})")
                },
                {
                    // Healthy = connected and driving the bar (ash, e34d1fc); problems come
                    // from Control1.Errors.
                    let driving = if healthy {
                        "driving the bar"
                    } else {
                        "not driving the bar (standby, or another seat)"
                    };
                    match (all, errors.len()) {
                        (0, _) => format!("{driving}, no problems"),
                        (_, 0) => format!("{driving}, see Problems above"),
                        (_, 1) => format!("{driving}, 1 problem (below)"),
                        (_, n) => format!("{driving}, {n} problems (below)"),
                    }
                }
            )));
            daemon_row.set_subtitle(&glib::markup_escape_text(&format!(
                "connected · display {display}"
            )));
        }
        crate::live::Agent::Absent(why) => {
            agent_row.set_subtitle(&glib::markup_escape_text(&format!(
                "{why}. Start it with `systemctl --user start sliver-agent`; until then sliverd shows its safe layer."
            )));
            daemon_row.set_subtitle("unknown: the agent reports on sliverd");
        }
    });
    page.upcast()
}
