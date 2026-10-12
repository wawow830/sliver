//! The action picker (ux.md §4 "Actions"): what a gesture does, chosen from a list instead of
//! typed as TOML. The common forms of customization.md §5; anything else (macros, argv `exec`,
//! signals…) stays editable as TOML under Advanced and is never rewritten.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use sliver_config::{Key, parse_cli_value, writer::inline};
use toml::{Table, Value};

/// What a gesture does, as the picker shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    None,
    /// A shell command (`"pavucontrol"`).
    Command(String),
    /// `{ key = "F5" }`.
    Key(String),
    /// `{ keys = ["LeftCtrl", "LeftAlt", "T"] }`.
    Keys(Vec<String>),
    /// `{ open = "org.gnome.Nautilus" }`.
    Open(String),
    /// `{ url = "https://…" }`.
    Url(String),
    /// `{ layer = "media", mode = "toggle" }`.
    Layer {
        name: String,
        mode: String,
    },
    /// `{ close-layer = true }`.
    CloseLayer,
    /// `{ layout = "fkeys" }`.
    Layout(String),
    /// `{ profile = "work" }`.
    Profile(String),
    /// `{ dbus = { … } }`, kept whole (fir's presets fill it).
    DBus(Value),
}

/// The kinds in the picker, in order: (id, label).
pub const KINDS: &[(&str, &str)] = &[
    ("none", "Nothing"),
    ("command", "Run a command"),
    ("key", "Press a key"),
    ("keys", "Press a key combination"),
    ("open", "Open an app"),
    ("url", "Open a web address"),
    ("layer", "Open a layer"),
    ("close", "Close the open layer"),
    ("layout", "Switch layout"),
    ("profile", "Switch profile"),
    ("dbus", "Call D-Bus"),
];

pub const LAYER_MODES: &[(&str, &str)] = &[
    ("toggle", "until tapped again"),
    ("hold", "while held"),
    ("oneshot", "for one tap"),
];

impl Pick {
    pub fn kind(&self) -> &'static str {
        match self {
            Pick::None => "none",
            Pick::Command(_) => "command",
            Pick::Key(_) => "key",
            Pick::Keys(_) => "keys",
            Pick::Open(_) => "open",
            Pick::Url(_) => "url",
            Pick::Layer { .. } => "layer",
            Pick::CloseLayer => "close",
            Pick::Layout(_) => "layout",
            Pick::Profile(_) => "profile",
            Pick::DBus(_) => "dbus",
        }
    }

    /// The picker's view of an `on-*` value, or `None` if it can't show it (then it's TOML only).
    pub fn from_value(v: Option<&Value>) -> Option<Pick> {
        let Some(v) = v else {
            return Some(Pick::None);
        };
        let t = match v {
            Value::String(s) => return Some(Pick::Command(s.clone())),
            Value::Table(t) => t,
            _ => return None,
        };
        let s = |k: &str| t.get(k).and_then(Value::as_str).map(str::to_owned);
        let only = |keys: &[&str]| t.keys().all(|k| keys.contains(&k.as_str()));
        Some(if only(&["key"]) && t.contains_key("key") {
            Pick::Key(s("key")?)
        } else if only(&["keys"]) && t.contains_key("keys") {
            Pick::Keys(
                t["keys"]
                    .as_array()?
                    .iter()
                    .map(|k| k.as_str().map(str::to_owned))
                    .collect::<Option<_>>()?,
            )
        } else if only(&["open"]) && t.contains_key("open") {
            Pick::Open(s("open")?)
        } else if only(&["url"]) && t.contains_key("url") {
            Pick::Url(s("url")?)
        } else if only(&["layer", "mode"]) && t.contains_key("layer") {
            Pick::Layer {
                name: s("layer")?,
                mode: s("mode").unwrap_or_else(|| "toggle".into()),
            }
        } else if only(&["close-layer"]) && t.get("close-layer") == Some(&Value::Boolean(true)) {
            Pick::CloseLayer
        } else if only(&["layout"]) && t.contains_key("layout") {
            Pick::Layout(s("layout")?)
        } else if only(&["profile"]) && t.contains_key("profile") {
            Pick::Profile(s("profile")?)
        } else if only(&["dbus"]) && t.get("dbus").is_some_and(Value::is_table) {
            Pick::DBus(v.clone())
        } else {
            return None;
        })
    }

    /// The `on-*` value to write (`None` = no action). Text forms that are still empty write
    /// nothing yet.
    pub fn to_value(&self) -> Option<Value> {
        let one = |k: &str, v: Value| {
            let mut t = Table::new();
            t.insert(k.into(), v);
            Some(Value::Table(t))
        };
        let s = |s: &str| Value::String(s.to_owned());
        match self {
            Pick::None => None,
            Pick::Command(c) if c.trim().is_empty() => None,
            Pick::Command(c) => Some(s(c.trim())),
            Pick::Key(k) => one("key", s(k)),
            Pick::Keys(ks) if ks.is_empty() => None,
            Pick::Keys(ks) => one("keys", Value::Array(ks.iter().map(|k| s(k)).collect())),
            Pick::Open(a) if a.trim().is_empty() => None,
            Pick::Open(a) => one("open", s(a.trim())),
            Pick::Url(u) if u.trim().is_empty() => None,
            Pick::Url(u) => one("url", s(u.trim())),
            Pick::Layer { name, mode } => {
                let mut t = Table::new();
                t.insert("layer".into(), s(name));
                if mode != "toggle" {
                    t.insert("mode".into(), s(mode));
                }
                Some(Value::Table(t))
            }
            Pick::CloseLayer => one("close-layer", Value::Boolean(true)),
            Pick::Layout(l) => one("layout", s(l)),
            Pick::Profile(p) => one("profile", s(p)),
            Pick::DBus(v) => Some(v.clone()),
        }
    }
}

/// What the dropdowns offer, from the loaded config.
pub struct Choices {
    pub layers: Vec<String>,
    pub layouts: Vec<String>,
    pub profiles: Vec<String>,
}

fn default_pick(kind: &str, c: &Choices) -> Pick {
    let first =
        |v: &[String], fallback: &str| v.first().cloned().unwrap_or_else(|| fallback.into());
    match kind {
        "command" => Pick::Command(String::new()),
        "key" => Pick::Key("Esc".into()),
        "keys" => Pick::Keys(Vec::new()),
        "open" => Pick::Open(String::new()),
        "url" => Pick::Url(String::new()),
        "layer" => Pick::Layer {
            // The media layer if there is one (tiny-dfr's), else the first custom layer.
            name: c
                .layers
                .iter()
                .find(|l| *l == "media")
                .or_else(|| c.layers.iter().find(|l| *l != "base" && *l != "fn"))
                .cloned()
                .unwrap_or_else(|| first(&c.layers, "media")),
            mode: "toggle".into(),
        },
        "close" => Pick::CloseLayer,
        "layout" => Pick::Layout(first(&c.layouts, "default")),
        "profile" => Pick::Profile(first(&c.profiles, "default")),
        "dbus" => Pick::DBus(
            crate::dbus_presets::PRESETS
                .first()
                .map(|p| parse_cli_value(p.toml))
                .unwrap_or(Value::Table(Table::new())),
        ),
        _ => Pick::None,
    }
}

/// A dropdown over `options` showing `current`; `set` gets the chosen one.
fn choose(options: &[String], current: &str, set: impl Fn(String) + 'static) -> gtk::DropDown {
    let d = gtk::DropDown::from_strings(&options.iter().map(String::as_str).collect::<Vec<_>>());
    d.set_valign(gtk::Align::Center);
    d.set_selected(options.iter().position(|o| o == current).unwrap_or(0) as u32);
    let options = options.to_vec();
    d.connect_selected_notify(move |d| {
        if let Some(o) = options.get(d.selected() as usize) {
            set(o.clone());
        }
    });
    d
}

/// A searchable dropdown over every key name.
fn key_choice(current: &str, set: impl Fn(String) + 'static) -> gtk::DropDown {
    let names: Vec<String> = Key::names().map(str::to_owned).collect();
    let d = choose(&names, current, set);
    d.set_enable_search(true);
    d.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
    d
}

/// Rebuilds the value widgets (set once the closure can refer to itself).
type Refill = Rc<RefCell<Option<Box<dyn Fn()>>>>;

/// The picker for one gesture: `current` is its value now, `write` gets the new one (`None` =
/// no action) after every real change.
pub fn editor(
    title: &str,
    current: Option<&Value>,
    choices: Rc<Choices>,
    write: Rc<dyn Fn(Option<Value>)>,
) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    let advanced = adw::EntryRow::builder()
        .title("Advanced: the action as TOML (customization.md §5)")
        .show_apply_button(true)
        .text(current.map(inline).unwrap_or_default())
        .build();
    {
        let write = write.clone();
        advanced.connect_apply(move |e| {
            let t = e.text().trim().to_owned();
            write((!t.is_empty()).then(|| parse_cli_value(&t)));
        });
    }
    let Some(pick) = Pick::from_value(current) else {
        g.set_description(Some(
            "This action is more than the picker shows (a macro, an argv exec, a signal…), so \
             it's only editable as TOML.",
        ));
        g.add(&advanced);
        return g;
    };
    let pick = Rc::new(RefCell::new(pick));
    let last: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(pick.borrow().to_value()));
    // Writes only real changes, and keeps the Advanced row in step.
    let commit = {
        let (pick, write, advanced) = (pick.clone(), write.clone(), advanced.clone());
        Rc::new(move || {
            let v = pick.borrow().to_value();
            if *last.borrow() == v {
                return;
            }
            // A text field not filled in yet keeps the old action until it is.
            if v.is_none() && !matches!(*pick.borrow(), Pick::None) {
                return;
            }
            *last.borrow_mut() = v.clone();
            advanced.set_text(&v.as_ref().map(inline).unwrap_or_default());
            write(v);
        })
    };
    let kind = adw::ComboRow::builder()
        .title("Does")
        .model(&gtk::StringList::new(
            &KINDS.iter().map(|(_, l)| *l).collect::<Vec<_>>(),
        ))
        .selected(
            KINDS
                .iter()
                .position(|(k, _)| *k == pick.borrow().kind())
                .unwrap_or(0) as u32,
        )
        .build();
    let values = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(8)
        .build();
    let refill: Refill = Rc::default();
    *refill.borrow_mut() = Some(Box::new({
        let (pick, commit, values, choices) = (
            pick.clone(),
            commit.clone(),
            values.downgrade(),
            choices.clone(),
        );
        move || {
            let Some(values) = values.upgrade() else {
                return;
            };
            while let Some(c) = values.first_child() {
                values.remove(&c);
            }
            // Set the pick and write it.
            let set = {
                let (pick, commit) = (pick.clone(), commit.clone());
                move |p: Pick| {
                    *pick.borrow_mut() = p;
                    commit();
                }
            };
            let current = pick.borrow().clone();
            let row = |title: &str| adw::ActionRow::builder().title(title).build();
            let entry = |text: &str, hint: &str| {
                gtk::Entry::builder()
                    .text(text)
                    .placeholder_text(hint)
                    .valign(gtk::Align::Center)
                    .width_chars(28)
                    .build()
            };
            match current {
                Pick::None | Pick::CloseLayer => {
                    values.set_visible(false);
                    return;
                }
                Pick::Command(c) => {
                    let r = row("Command");
                    let e = entry(&c, "e.g. pavucontrol");
                    let set = set.clone();
                    crate::cond::on_commit(&e, move |t| set(Pick::Command(t)));
                    r.add_suffix(&e);
                    values.append(&r);
                }
                Pick::Key(k) => {
                    let r = row("Key");
                    let set = set.clone();
                    r.add_suffix(&key_choice(&k, move |k| set(Pick::Key(k))));
                    values.append(&r);
                }
                Pick::Keys(ks) => {
                    let r = row("Keys, pressed together");
                    let e = entry(&ks.join(" + "), "e.g. LeftCtrl + LeftAlt + T");
                    let set = set.clone();
                    crate::cond::on_commit(&e, move |t| {
                        set(Pick::Keys(
                            t.split('+')
                                .map(|k| k.trim().to_owned())
                                .filter(|k| !k.is_empty())
                                .collect(),
                        ))
                    });
                    r.add_suffix(&e);
                    values.append(&r);
                }
                Pick::Open(a) => {
                    let r = row("App");
                    let e = entry(&a, "app id, e.g. org.gnome.Nautilus");
                    {
                        let set = set.clone();
                        crate::cond::on_commit(&e, move |t| set(Pick::Open(t)));
                    }
                    let e2 = e.clone();
                    let pick_app = crate::cond::app_picker(move |id| {
                        e2.set_text(id);
                        e2.emit_activate();
                    });
                    r.add_suffix(&e);
                    r.add_suffix(&pick_app);
                    values.append(&r);
                }
                Pick::Url(u) => {
                    let r = row("Address");
                    let e = entry(&u, "https://…");
                    let set = set.clone();
                    crate::cond::on_commit(&e, move |t| set(Pick::Url(t)));
                    r.add_suffix(&e);
                    values.append(&r);
                }
                Pick::Layer { name, mode } => {
                    let r = row("Layer");
                    {
                        let (set, mode) = (set.clone(), mode.clone());
                        r.add_suffix(&choose(&choices.layers, &name, move |n| {
                            set(Pick::Layer {
                                name: n,
                                mode: mode.clone(),
                            })
                        }));
                    }
                    values.append(&r);
                    let r = row("Open");
                    let modes: Vec<String> =
                        LAYER_MODES.iter().map(|(_, l)| (*l).to_owned()).collect();
                    let cur = LAYER_MODES
                        .iter()
                        .find(|(m, _)| *m == mode)
                        .map_or("", |(_, l)| *l);
                    let (set, name) = (set.clone(), name.clone());
                    r.add_suffix(&choose(&modes, cur, move |l| {
                        let m = LAYER_MODES
                            .iter()
                            .find(|(_, x)| *x == l)
                            .map_or("toggle", |(m, _)| *m);
                        set(Pick::Layer {
                            name: name.clone(),
                            mode: m.into(),
                        })
                    }));
                    values.append(&r);
                }
                Pick::Layout(l) => {
                    let r = row("Layout");
                    let set = set.clone();
                    r.add_suffix(&choose(&choices.layouts, &l, move |l| set(Pick::Layout(l))));
                    values.append(&r);
                }
                Pick::Profile(p) => {
                    let r = row("Profile");
                    let set = set.clone();
                    r.add_suffix(&choose(&choices.profiles, &p, move |p| {
                        set(Pick::Profile(p))
                    }));
                    values.append(&r);
                }
                Pick::DBus(v) => {
                    let r = row("Call");
                    let mut labels: Vec<String> = crate::dbus_presets::PRESETS
                        .iter()
                        .map(|p| p.label.to_owned())
                        .collect();
                    let current = crate::dbus_presets::PRESETS
                        .iter()
                        .find(|p| parse_cli_value(p.toml) == v)
                        .map(|p| p.label.to_owned());
                    if current.is_none() {
                        labels.insert(0, "Your own call (see Advanced)".into());
                    }
                    let set = set.clone();
                    r.add_suffix(&choose(
                        &labels,
                        current.as_deref().unwrap_or(""),
                        move |l| {
                            if let Some(p) =
                                crate::dbus_presets::PRESETS.iter().find(|p| p.label == l)
                            {
                                set(Pick::DBus(parse_cli_value(p.toml)));
                            }
                        },
                    ));
                    values.append(&r);
                }
            }
            values.set_visible(true);
        }
    }));
    if let Some(f) = refill.borrow().as_ref() {
        f();
    }
    {
        let (pick, commit, refill, choices) = (
            pick.clone(),
            commit.clone(),
            refill.clone(),
            choices.clone(),
        );
        kind.connect_selected_notify(move |k| {
            let (id, _) = KINDS[k.selected() as usize];
            if pick.borrow().kind() == id {
                return;
            }
            *pick.borrow_mut() = default_pick(id, &choices);
            commit();
            if let Some(f) = refill.borrow().as_ref() {
                f();
            }
        });
    }
    let kinds = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    kinds.append(&kind);
    let adv = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(8)
        .build();
    adv.append(&advanced);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&kinds);
    col.append(&values);
    col.append(&adv);
    g.add(&col);
    // `refill` lives as long as the kind row's handler (the dialog).
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        format!("v = {s}").parse::<Table>().unwrap()["v"].clone()
    }

    #[test]
    fn round_trips_what_it_can_show() {
        for s in [
            r#""pavucontrol""#,
            r#"{ key = "F5" }"#,
            r#"{ keys = ["LeftCtrl", "LeftAlt", "T"] }"#,
            r#"{ open = "org.gnome.Nautilus" }"#,
            r#"{ url = "https://example.org" }"#,
            r#"{ layer = "media" }"#,
            r#"{ layer = "media", mode = "hold" }"#,
            r#"{ close-layer = true }"#,
            r#"{ layout = "fkeys" }"#,
            r#"{ profile = "work" }"#,
            crate::dbus_presets::PRESETS[0].toml,
        ] {
            let p = Pick::from_value(Some(&v(s))).unwrap_or_else(|| panic!("can't show {s}"));
            assert_eq!(p.to_value(), Some(v(s)), "{s}");
        }
        assert_eq!(Pick::from_value(None), Some(Pick::None));
        assert_eq!(Pick::None.to_value(), None);
    }

    #[test]
    fn keeps_to_toml_what_it_cannot_show() {
        for s in [
            r#"[{ key = "F1" }, { sleep = "100ms" }]"#,
            r#"{ exec = ["foot", "-e", "htop"] }"#,
            r#"{ signal = 8 }"#,
            r#"{ key = "F5", url = "x" }"#,
        ] {
            assert_eq!(Pick::from_value(Some(&v(s))), None, "{s}");
        }
    }

    #[test]
    fn every_default_is_a_valid_action() {
        let c = Choices {
            layers: vec!["base".into(), "fn".into(), "media".into()],
            layouts: vec!["default".into()],
            profiles: vec!["default".into()],
        };
        for (k, _) in KINDS {
            let Some(val) = default_pick(k, &c).to_value() else {
                continue;
            };
            let mut m = Table::new();
            m.insert("format".into(), "x".into());
            m.insert("on-tap".into(), val);
            let mut mods = Table::new();
            mods.insert("custom/x".into(), Value::Table(m));
            let mut cfg = Table::new();
            cfg.insert("modules".into(), Value::Table(mods));
            let text = toml::to_string(&cfg).unwrap();
            let l = sliver_config::load_str(&text, std::path::Path::new("."));
            assert!(l.diagnostics.ok(), "{k}: {text}\n{:#?}", l.diagnostics);
        }
        assert!(Key::names().any(|n| n == "F5"));
    }
}
