//! The Apps page (ux.md §6, api.md §4): which apps and providers may put things on the bar.
//! It writes `[apps."…"]`, `[providers."…"]` and `[api]` (customization.md §6b).

use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use toml::Value;

use crate::pages::{group, row};
use crate::{Ui, apply};

fn q(k: &str) -> String {
    Value::String(k.to_owned()).to_string()
}

fn set_bool(ui: &Rc<Ui>, path: String, label: String, on: bool) {
    apply(ui, move |ed, _| {
        ed.set(&path, &Value::Boolean(on))?;
        Ok(format!("{label}: {}", if on { "on" } else { "off" }))
    });
}

fn switch_row(
    ui: &Rc<Ui>,
    title: &str,
    subtitle: &str,
    path: String,
    active: bool,
) -> adw::SwitchRow {
    let s = adw::SwitchRow::builder()
        .title(glib::markup_escape_text(title))
        .subtitle(glib::markup_escape_text(subtitle))
        .active(active)
        .build();
    let (ui, label) = (ui.clone(), title.to_owned());
    s.connect_active_notify(move |s| set_bool(&ui, path.clone(), label.clone(), s.is_active()));
    s
}

pub fn page(ui: &Rc<Ui>) -> gtk::Widget {
    let state = ui.state();
    let c = &state.loaded.config;
    let page = adw::PreferencesPage::new();

    let defaults = group("All apps", None);
    let any = c.app_policy("*");
    defaults.add(&switch_row(
        ui,
        "Apps may take the whole bar",
        "A focused app's full-bar mode (a color picker, a call) covers everything but your first zone, until ✕ or focus leaves",
        format!("apps.{}.whole-bar", q("*")),
        any.whole_bar,
    ));
    defaults.add(&switch_row(
        ui,
        "Scripts may target other apps",
        "Lets tools put controls on the bar while some other app is focused",
        "api.target".into(),
        c.api.target,
    ));
    page.add(&defaults);

    // Live providers, filled in when the agent answers.
    let live = group(
        "Publishing now",
        Some("Apps and scripts that have put items on the bar, from the running agent."),
    );
    let waiting = row("Asking the agent…", "");
    live.add(&waiting);
    page.add(&live);
    {
        let (ui, live) = (ui.clone(), live.clone());
        crate::live::contexts(move |r| {
            live.remove(&waiting);
            match r {
                Ok(list) if list.is_empty() => live.add(&row(
                    "Nothing right now",
                    "Apps that use the sliver API show up here while they run.",
                )),
                Ok(list) => {
                    let state = ui.state();
                    let providers: BTreeSet<String> =
                        list.iter().map(|c| c.provider.clone()).collect();
                    for p in providers {
                        let mine: Vec<_> = list.iter().filter(|c| c.provider == p).collect();
                        let what: Vec<String> = mine
                            .iter()
                            .map(|c| {
                                format!(
                                    "{} ({}, {} items{})",
                                    if c.title.is_empty() {
                                        "untitled"
                                    } else {
                                        &c.title
                                    },
                                    c.scope,
                                    c.items,
                                    if c.on_screen { ", on screen" } else { "" }
                                )
                            })
                            .collect();
                        live.add(&switch_row(
                            &ui,
                            &p,
                            &what.join(" · "),
                            format!("providers.{}.show", q(&p)),
                            state.loaded.config.provider_prefs(&p).show,
                        ));
                    }
                }
                Err(e) => live.add(&row("The agent isn't answering", &e)),
            }
        });
    }

    let apps = group(
        "Per app",
        Some(
            "Turn off an app's own buttons in App controls. Your per-app layouts always win anyway.",
        ),
    );
    for (id, prefs) in &c.apps {
        if id == "*" {
            continue;
        }
        let pin = prefs
            .pin
            .as_deref()
            .map(|p| format!(" · only {p} fills its controls"))
            .unwrap_or_default();
        apps.add(&switch_row(
            ui,
            id,
            &format!("its items may fill App controls{pin}"),
            format!("apps.{}.controls", q(id)),
            prefs.controls.unwrap_or(true),
        ));
    }
    let add = adw::EntryRow::builder()
        .title("Turn off an app's controls: app id, e.g. org.mozilla.firefox")
        .show_apply_button(true)
        .build();
    {
        let ui = ui.clone();
        add.connect_apply(move |e| {
            let id = e.text().trim().to_owned();
            if !id.is_empty() {
                set_bool(
                    &ui,
                    format!("apps.{}.controls", q(&id)),
                    format!("{id} controls"),
                    false,
                );
            }
        });
    }
    apps.add(&add);
    page.add(&apps);

    let configured: Vec<_> = c.providers.iter().collect();
    if !configured.is_empty() {
        let g = group("Providers in your config", None);
        for (id, p) in configured {
            let order = p.order.map(|o| format!(" · order {o}")).unwrap_or_default();
            g.add(&switch_row(
                ui,
                id,
                &format!(
                    "{}{order}",
                    if p.allow {
                        "allowed (sandboxed)"
                    } else {
                        "shown"
                    }
                ),
                format!("providers.{}.show", q(id)),
                p.show,
            ));
        }
        page.add(&g);
    }
    page.upcast()
}
