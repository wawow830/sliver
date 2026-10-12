//! The condition builder (ux.md §5): rules' `when` and modules' "Show only when" as rows of
//! pickers instead of TOML. One level, all-of or any-of, as ux.md asks; anything the builder can't
//! show (`not`, regexes, nesting, other keys) stays editable as TOML under Advanced.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use sliver_config::{canonical_key, parse_cli_value, writer::inline};
use toml::{Table, Value};

/// On/off states a row can test, in the order the Add menu shows them: (key as written, label).
pub const FLAGS: &[(&str, &str)] = &[
    ("locked", "Screen locked"),
    ("on-battery", "On battery"),
    ("fullscreen", "Fullscreen window"),
    ("screencast", "Screen shared"),
    ("external-display", "External display connected"),
    ("dnd", "Do not disturb"),
    ("caffeine", "Caffeine on"),
    ("mic-muted", "Microphone muted"),
    ("dark-mode", "Dark mode"),
    ("idle", "Idle"),
    ("app-controls", "The focused app fills App controls"),
];

/// What `media-status` can be.
pub const MEDIA: &[&str] = &["playing", "paused", "stopped", "none"];

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// `app-id = "x"` or `app-id = ["a", "b"]` (case-insensitive; .desktop ids match too).
    App(Vec<String>),
    /// `locked = true` and the other [`FLAGS`]. Off is written `not = { locked = true }`: true
    /// unless the flag is known to be on, which fails safe when a backend can't tell.
    Flag { key: String, on: bool },
    /// `media-status = "playing"`.
    Media(String),
    /// `battery-pct = { below = N }`.
    BatteryBelow(i64),
    /// `workspace = N`, or `workspace-name = "x"` when it isn't a number.
    Workspace(String),
    /// `title = { contains = "x" }`.
    TitleContains(String),
    /// `time = { between = ["09:00", "17:00"] }` (wraps past midnight when from > to).
    Time(String, String),
}

/// A condition the builder can show: rows that must all (or any) match. No rows = always.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cond {
    pub any: bool,
    pub rows: Vec<Row>,
}

fn row_of(key: &str, v: &Value) -> Option<Row> {
    let k = canonical_key(key);
    let one = |t: &Table, op: &str| -> Option<Value> {
        (t.len() == 1).then(|| t.get(op).cloned()).flatten()
    };
    Some(match (k.as_str(), v) {
        ("app_id", Value::String(s)) => Row::App(vec![s.clone()]),
        ("app_id", Value::Array(a)) => Row::App(
            a.iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<_>>()?,
        ),
        ("media_status", Value::String(s)) => Row::Media(s.clone()),
        ("battery_pct", Value::Table(t)) => Row::BatteryBelow(one(t, "below")?.as_integer()?),
        ("workspace", Value::Integer(n)) => Row::Workspace(n.to_string()),
        ("workspace_name", Value::String(s)) => Row::Workspace(s.clone()),
        ("title", Value::Table(t)) => Row::TitleContains(one(t, "contains")?.as_str()?.to_owned()),
        ("time", Value::Table(t)) => match one(t, "between")?.as_array()?.as_slice() {
            [Value::String(a), Value::String(b)] => Row::Time(a.clone(), b.clone()),
            _ => return None,
        },
        (k, Value::Boolean(b)) => {
            let (key, _) = FLAGS.iter().find(|(f, _)| canonical_key(f) == k)?;
            Row::Flag {
                key: (*key).to_owned(),
                on: *b,
            }
        }
        _ => return None,
    })
}

fn flag_key(k: &str) -> Option<&'static str> {
    let k = canonical_key(k);
    FLAGS
        .iter()
        .find(|(f, _)| canonical_key(f) == k)
        .map(|(f, _)| *f)
}

/// `{ flag = true }` or `{ any = [{ a = true }, { b = true }] }` under `not`: those flags, off.
fn negated(v: &Value) -> Option<Vec<Row>> {
    let t = v.as_table()?;
    let one = |t: &Table| -> Option<Row> {
        let (k, v) = t.iter().next()?;
        (t.len() == 1 && v.as_bool() == Some(true)).then_some(())?;
        Some(Row::Flag {
            key: flag_key(k)?.to_owned(),
            on: false,
        })
    };
    match (t.len(), t.get("any")) {
        (1, Some(Value::Array(branches))) => branches.iter().map(|b| one(b.as_table()?)).collect(),
        _ => one(t).map(|r| vec![r]),
    }
}

fn flag_table(key: &str) -> Value {
    let mut t = Table::new();
    t.insert(key.to_owned(), Value::Boolean(true));
    Value::Table(t)
}

fn row_entry(r: &Row) -> (String, Value) {
    let s = |s: &str| Value::String(s.to_owned());
    let op = |op: &str, v: Value| {
        let mut t = Table::new();
        t.insert(op.into(), v);
        Value::Table(t)
    };
    match r {
        Row::App(ids) if ids.len() == 1 => ("app-id".into(), s(&ids[0])),
        Row::App(ids) => (
            "app-id".into(),
            Value::Array(ids.iter().map(|i| s(i)).collect()),
        ),
        Row::Flag { key, on: true } => (key.clone(), Value::Boolean(true)),
        Row::Flag { key, on: false } => {
            let mut t = Table::new();
            t.insert(key.clone(), Value::Boolean(true));
            ("not".into(), Value::Table(t))
        }
        Row::Media(m) => ("media-status".into(), s(m)),
        Row::BatteryBelow(n) => ("battery-pct".into(), op("below", Value::Integer(*n))),
        Row::Workspace(w) => match w.trim().parse::<i64>() {
            Ok(n) => ("workspace".into(), Value::Integer(n)),
            Err(_) => ("workspace-name".into(), s(w.trim())),
        },
        Row::TitleContains(t) => ("title".into(), op("contains", s(t))),
        Row::Time(a, b) => ("time".into(), op("between", Value::Array(vec![s(a), s(b)]))),
    }
}

impl Cond {
    /// The builder's view of a `when` value, or `None` if it can't show it (then it's TOML only).
    pub fn from_value(v: Option<&Value>) -> Option<Cond> {
        let Some(v) = v else {
            return Some(Cond::default());
        };
        let t = v.as_table()?;
        if let (1, Some(Value::Array(branches))) = (t.len(), t.get("any")) {
            let rows = branches
                .iter()
                .map(|b| {
                    let b = b.as_table()?;
                    let (k, v) = b.iter().next()?;
                    if b.len() != 1 {
                        return None;
                    }
                    match (k.as_str(), negated(v).as_deref()) {
                        ("not", Some([r])) => Some(r.clone()),
                        ("not", _) => None,
                        _ => row_of(k, v),
                    }
                })
                .collect::<Option<Vec<_>>>()?;
            return Some(Cond { any: true, rows });
        }
        let mut rows = Vec::new();
        for (k, v) in t {
            if k == "not" {
                rows.extend(negated(v)?);
            } else {
                rows.push(row_of(k, v)?);
            }
        }
        Some(Cond { any: false, rows })
    }

    /// The `when` value to write (`None` = no condition: always). All-of rows that test the same
    /// key are merged (apps) or the last one wins, since a TOML table has one entry per key.
    pub fn to_value(&self) -> Option<Value> {
        if self.rows.is_empty() {
            return None;
        }
        if self.any && self.rows.len() > 1 {
            let branches = self
                .rows
                .iter()
                .map(|r| {
                    let (k, v) = row_entry(r);
                    let mut t = Table::new();
                    t.insert(k, v);
                    Value::Table(t)
                })
                .collect();
            let mut t = Table::new();
            t.insert("any".into(), Value::Array(branches));
            return Some(Value::Table(t));
        }
        let mut t = Table::new();
        let mut apps: Vec<String> = Vec::new();
        let mut off: Vec<&str> = Vec::new();
        for r in &self.rows {
            match r {
                Row::App(ids) => apps.extend(ids.iter().cloned()),
                Row::Flag { key, on: false } => off.push(key),
                r => {
                    let (k, v) = row_entry(r);
                    t.insert(k, v);
                }
            }
        }
        if !apps.is_empty() {
            let (k, v) = row_entry(&Row::App(apps));
            t.insert(k, v);
        }
        // Several flags off: not (a or b), i.e. neither.
        match off.as_slice() {
            [] => {}
            [one] => {
                t.insert("not".into(), flag_table(one));
            }
            many => {
                let mut any = Table::new();
                any.insert(
                    "any".into(),
                    Value::Array(many.iter().map(|k| flag_table(k)).collect()),
                );
                t.insert("not".into(), Value::Table(any));
            }
        }
        Some(Value::Table(t))
    }
}

/// Rebuilds the row list (set once the closure can refer to itself).
type Refill = Rc<RefCell<Option<Box<dyn Fn()>>>>;

/// A new row of the kind the Add menu offers, with a sensible start value.
fn new_row(kind: &str) -> Row {
    match kind {
        "app" => Row::App(Vec::new()),
        "media" => Row::Media("playing".into()),
        "battery" => Row::BatteryBelow(20),
        "workspace" => Row::Workspace("1".into()),
        "title" => Row::TitleContains(String::new()),
        "time" => Row::Time("09:00".into(), "17:00".into()),
        flag => Row::Flag {
            key: flag.to_owned(),
            on: true,
        },
    }
}

fn label(r: &Row) -> String {
    match r {
        Row::App(_) => "App".into(),
        Row::Flag { key, .. } => FLAGS
            .iter()
            .find(|(k, _)| k == key)
            .map_or(key.clone(), |(_, l)| (*l).to_owned()),
        Row::Media(_) => "Media".into(),
        Row::BatteryBelow(_) => "Battery below (%)".into(),
        Row::Workspace(_) => "Workspace (number or name)".into(),
        Row::TitleContains(_) => "Window title contains".into(),
        Row::Time(..) => "Time between".into(),
    }
}

/// Calls `f` with the entry's text when the user is done with it (Enter, or leaving the field),
/// so typing doesn't write the config on every keystroke.
pub(crate) fn on_commit(e: &gtk::Entry, f: impl Fn(String) + 'static) {
    let f = Rc::new(f);
    {
        let f = f.clone();
        e.connect_activate(move |e| f(e.text().to_string()));
    }
    let focus = gtk::EventControllerFocus::new();
    let e2 = e.clone();
    focus.connect_leave(move |_| f(e2.text().to_string()));
    e.add_controller(focus);
}

/// Installed `.desktop` apps as (id without `.desktop`, name), by name.
pub(crate) fn installed_apps() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = gtk::gio::AppInfo::all()
        .into_iter()
        .filter(|a| a.should_show())
        .filter_map(|a| {
            let id = a.id()?.trim_end_matches(".desktop").to_owned();
            Some((id, a.display_name().to_string()))
        })
        .collect();
    v.sort_by_key(|a| a.1.to_lowercase());
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

/// "Add app…": a popover with a search field over the installed apps; `chosen` gets the app id.
pub(crate) fn app_picker(chosen: impl Fn(&str) + 'static) -> gtk::MenuButton {
    let apps = installed_apps();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search apps")
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    for (id, name) in &apps {
        let l = gtk::Label::builder()
            .label(name.as_str())
            .tooltip_text(id.as_str())
            .xalign(0.0)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(8)
            .margin_end(8)
            .build();
        l.set_widget_name(id);
        list.append(&l);
    }
    {
        let search = search.clone();
        list.set_filter_func(move |row| {
            let q = search.text().to_lowercase();
            q.is_empty()
                || row.child().is_some_and(|c| {
                    let name = c
                        .downcast_ref::<gtk::Label>()
                        .map(|l| l.label().to_lowercase())
                        .unwrap_or_default();
                    name.contains(&q) || c.widget_name().to_lowercase().contains(&q)
                })
        });
    }
    {
        let list = list.clone();
        search.connect_search_changed(move |_| list.invalidate_filter());
    }
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.append(&search);
    col.append(
        &gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(240)
            .max_content_height(320)
            .propagate_natural_width(true)
            .build(),
    );
    let pop = gtk::Popover::builder().child(&col).build();
    {
        let pop = pop.clone();
        list.connect_row_activated(move |_, r| {
            pop.popdown();
            if let Some(c) = r.child() {
                chosen(&c.widget_name());
            }
        });
    }
    gtk::MenuButton::builder()
        .label("Add app…")
        .popover(&pop)
        .valign(gtk::Align::Center)
        .build()
}

/// The builder: `current` is the `when` value now, and `write` gets the new one (`None` = no
/// condition) after every change.
pub fn editor(
    title: &str,
    current: Option<&Value>,
    write: Rc<dyn Fn(Option<Value>)>,
) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    let advanced = adw::EntryRow::builder()
        .title("Advanced: the condition as TOML (customization.md §6)")
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
    let Some(cond) = Cond::from_value(current) else {
        g.set_description(Some(
            "This condition uses more than the builder can show (not, regex, nesting…), so it's \
             only editable as TOML.",
        ));
        g.add(&advanced);
        return g;
    };
    g.set_description(Some("No conditions means always."));
    let cond = Rc::new(RefCell::new(cond));
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let mode = adw::ComboRow::builder()
        .title("Match")
        .model(&gtk::StringList::new(&["all of these", "any of these"]))
        .selected(u32::from(cond.borrow().any))
        .build();
    // Rows are rebuilt after adding or removing one; value edits only write.
    let refill: Refill = Rc::default();
    // Writes only real changes (an empty new row, or re-confirming a field, isn't one).
    let last: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(cond.borrow().to_value()));
    let commit = {
        let (cond, write, advanced) = (cond.clone(), write.clone(), advanced.clone());
        Rc::new(move || {
            let v = cond.borrow().to_value();
            if *last.borrow() == v {
                return;
            }
            *last.borrow_mut() = v.clone();
            advanced.set_text(&v.as_ref().map(inline).unwrap_or_default());
            write(v);
        })
    };
    {
        let (cond, commit) = (cond.clone(), commit.clone());
        mode.connect_selected_notify(move |m| {
            cond.borrow_mut().any = m.selected() == 1;
            commit();
        });
    }
    *refill.borrow_mut() = Some(Box::new({
        // Weak: the rows' buttons hold `refill`, so strong refs here would be a cycle.
        let (cond, commit, list, refill) = (
            cond.clone(),
            commit.clone(),
            list.downgrade(),
            Rc::downgrade(&refill),
        );
        move || {
            let Some(list) = list.upgrade() else { return };
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let rows = cond.borrow().rows.clone();
            if rows.is_empty() {
                list.append(
                    &adw::ActionRow::builder()
                        .title("Always")
                        .subtitle("Add a condition below")
                        .build(),
                );
            }
            for (i, r) in rows.iter().enumerate() {
                let row = adw::ActionRow::builder().title(label(r)).build();
                // Set row i of the condition and write it.
                let set = {
                    let (cond, commit) = (cond.clone(), commit.clone());
                    move |r: Row| {
                        if let Some(slot) = cond.borrow_mut().rows.get_mut(i) {
                            *slot = r;
                        }
                        commit();
                    }
                };
                match r {
                    Row::App(ids) => {
                        let e = gtk::Entry::builder()
                            .text(ids.join(", "))
                            .placeholder_text("app ids, e.g. org.mozilla.firefox, foot")
                            .valign(gtk::Align::Center)
                            .width_chars(28)
                            .build();
                        let set = set.clone();
                        on_commit(&e, move |t| {
                            set(Row::App(
                                t.split(',')
                                    .map(|s| s.trim().to_owned())
                                    .filter(|s| !s.is_empty())
                                    .collect(),
                            ))
                        });
                        // Pick from installed apps (searchable): appends its id.
                        let pick = app_picker({
                            let e = e.clone();
                            move |id: &str| {
                                let mut t = e.text().to_string();
                                if !t.trim().is_empty() {
                                    t.push_str(", ");
                                }
                                t.push_str(id);
                                e.set_text(&t);
                                e.emit_activate();
                            }
                        });
                        row.add_suffix(&e);
                        row.add_suffix(&pick);
                    }
                    Row::Flag { key, on } => {
                        let s = gtk::Switch::builder()
                            .active(*on)
                            .valign(gtk::Align::Center)
                            .tooltip_text("On: while it is. Off: while it isn't.")
                            .build();
                        let (set, key) = (set.clone(), key.clone());
                        s.connect_active_notify(move |s| {
                            set(Row::Flag {
                                key: key.clone(),
                                on: s.is_active(),
                            })
                        });
                        row.add_suffix(&s);
                    }
                    Row::Media(m) => {
                        let d = gtk::DropDown::from_strings(MEDIA);
                        d.set_valign(gtk::Align::Center);
                        d.set_selected(MEDIA.iter().position(|x| x == m).unwrap_or(0) as u32);
                        let set = set.clone();
                        d.connect_selected_notify(move |d| {
                            set(Row::Media(MEDIA[d.selected() as usize].to_owned()))
                        });
                        row.add_suffix(&d);
                    }
                    Row::BatteryBelow(n) => {
                        let s = gtk::SpinButton::with_range(1.0, 100.0, 5.0);
                        s.set_value(*n as f64);
                        s.set_valign(gtk::Align::Center);
                        let set = set.clone();
                        s.connect_value_changed(move |s| set(Row::BatteryBelow(s.value() as i64)));
                        row.add_suffix(&s);
                    }
                    Row::Workspace(w) | Row::TitleContains(w) => {
                        let e = gtk::Entry::builder()
                            .text(w.as_str())
                            .valign(gtk::Align::Center)
                            .build();
                        let (set, ws) = (set.clone(), matches!(r, Row::Workspace(_)));
                        on_commit(&e, move |t| {
                            set(if ws {
                                Row::Workspace(t)
                            } else {
                                Row::TitleContains(t)
                            })
                        });
                        row.add_suffix(&e);
                    }
                    Row::Time(a, b) => {
                        let from = gtk::Entry::builder()
                            .text(a.as_str())
                            .width_chars(6)
                            .valign(gtk::Align::Center)
                            .build();
                        let to = gtk::Entry::builder()
                            .text(b.as_str())
                            .width_chars(6)
                            .valign(gtk::Align::Center)
                            .build();
                        for e in [&from, &to] {
                            let (set, from, to) = (set.clone(), from.clone(), to.clone());
                            on_commit(e, move |_| {
                                set(Row::Time(from.text().to_string(), to.text().to_string()))
                            });
                        }
                        row.add_suffix(&from);
                        row.add_suffix(&gtk::Label::new(Some("–")));
                        row.add_suffix(&to);
                    }
                }
                let del = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .tooltip_text("Remove this condition")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                {
                    let (cond, commit, refill) = (cond.clone(), commit.clone(), refill.clone());
                    del.connect_clicked(move |_| {
                        cond.borrow_mut().rows.remove(i);
                        commit();
                        if let Some(r) = refill.upgrade()
                            && let Some(f) = r.borrow().as_ref()
                        {
                            f();
                        }
                    });
                }
                row.add_suffix(&del);
                list.append(&row);
            }
        }
    }));
    if let Some(f) = refill.borrow().as_ref() {
        f();
    }

    // Add: a menu of the kinds of condition.
    let kinds: Vec<(&str, String)> = [
        ("app", "App".to_owned()),
        ("media", "Media playing / paused".to_owned()),
        ("battery", "Battery below…".to_owned()),
        ("workspace", "Workspace".to_owned()),
        ("title", "Window title contains…".to_owned()),
        ("time", "Time of day".to_owned()),
    ]
    .into_iter()
    .chain(FLAGS.iter().map(|(k, l)| (*k, (*l).to_owned())))
    .collect();
    let menu = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    for (k, l) in &kinds {
        let r = gtk::Label::builder()
            .label(l.as_str())
            .xalign(0.0)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(8)
            .margin_end(8)
            .build();
        r.set_widget_name(k);
        menu.append(&r);
    }
    // Scrolls rather than growing past the screen (a popover that can't fit isn't shown).
    let pop = gtk::Popover::builder()
        .child(
            &gtk::ScrolledWindow::builder()
                .child(&menu)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_width(true)
                .propagate_natural_height(true)
                .max_content_height(320)
                .build(),
        )
        .build();
    let add = gtk::MenuButton::builder()
        .label("Add condition")
        .popover(&pop)
        .halign(gtk::Align::Start)
        .margin_top(8)
        .build();
    {
        let (cond, commit, refill, pop) =
            (cond.clone(), commit.clone(), refill.clone(), pop.clone());
        menu.connect_row_activated(move |_, r| {
            pop.popdown();
            let kind = r
                .child()
                .map(|c| c.widget_name().to_string())
                .unwrap_or_default();
            cond.borrow_mut().rows.push(new_row(&kind));
            commit();
            if let Some(f) = refill.borrow().as_ref() {
                f();
            }
        });
    }
    let modes = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_bottom(8)
        .build();
    modes.append(&mode);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&modes);
    col.append(&list);
    col.append(&add);
    let adv = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(12)
        .build();
    adv.append(&advanced);
    col.append(&adv);
    g.add(&col);
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        format!("v = {s}").parse::<Table>().unwrap()["v"].clone()
    }

    fn round_trip(s: &str) -> Value {
        Cond::from_value(Some(&v(s)))
            .unwrap_or_else(|| panic!("builder can't show {s}"))
            .to_value()
            .unwrap()
    }

    #[test]
    fn round_trips_what_it_can_show() {
        for s in [
            r#"{ app-id = "org.mozilla.firefox" }"#,
            r#"{ app-id = ["foot", "kitty"], fullscreen = true }"#,
            r#"{ locked = true }"#,
            r#"{ battery-pct = { below = 20 }, not = { on-battery = true } }"#,
            r#"{ not = { app-controls = true } }"#,
            r#"{ fullscreen = true, not = { any = [{ locked = true }, { screencast = true }] } }"#,
            r#"{ any = [{ not = { dnd = true } }, { media-status = "playing" }] }"#,
            r#"{ media-status = "playing" }"#,
            r#"{ workspace = 3 }"#,
            r#"{ workspace-name = "mail" }"#,
            r#"{ title = { contains = "Meet" } }"#,
            r#"{ time = { between = ["22:00", "06:00"] } }"#,
            r#"{ any = [{ screencast = true }, { app-id = "zoom" }] }"#,
        ] {
            assert_eq!(round_trip(s), v(s), "{s}");
        }
    }

    #[test]
    fn keeps_to_toml_what_it_cannot_show() {
        for s in [
            r#"{ not = { locked = true, on-battery = true } }"#,
            r#"{ not = { title = "x" } }"#,
            r#"{ title = { regex = "^Meet" } }"#,
            r#"{ any = [{ locked = true, on-battery = true }] }"#,
            r#"{ volume = { above = 50 } }"#,
        ] {
            assert_eq!(Cond::from_value(Some(&v(s))), None, "{s}");
        }
        assert_eq!(Cond::from_value(None), Some(Cond::default()));
    }

    #[test]
    fn a_false_flag_reads_as_off_and_is_written_fail_safe() {
        let c = Cond::from_value(Some(&v(r#"{ on-battery = false }"#))).unwrap();
        assert_eq!(
            c.rows,
            [Row::Flag {
                key: "on-battery".into(),
                on: false
            }]
        );
        assert_eq!(
            c.to_value().unwrap(),
            v(r#"{ not = { on-battery = true } }"#)
        );
    }

    #[test]
    fn writes_valid_conditions() {
        let c = Cond {
            any: false,
            rows: vec![
                Row::App(vec!["foot".into()]),
                Row::App(vec!["kitty".into()]),
                Row::Workspace("web".into()),
                new_row("locked"),
            ],
        };
        let out = c.to_value().unwrap();
        assert_eq!(
            out,
            v(r#"{ app-id = ["foot", "kitty"], workspace-name = "web", locked = true }"#)
        );
        // Every row kind produces something sliver-config accepts.
        let all = Cond {
            any: true,
            rows: ["app", "media", "battery", "workspace", "title", "time"]
                .iter()
                .map(|k| new_row(k))
                .chain(FLAGS.iter().map(|(k, _)| new_row(k)))
                .collect(),
        };
        let mut rule = Table::new();
        rule.insert("when".into(), all.to_value().unwrap());
        rule.insert("layout".into(), "default".into());
        let mut cfg = Table::new();
        cfg.insert("rules".into(), Value::Array(vec![Value::Table(rule)]));
        let l = sliver_config::load_str(&toml::to_string(&cfg).unwrap(), std::path::Path::new("."));
        assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
        assert_eq!(Cond::default().to_value(), None, "no rows: always");
    }
}
