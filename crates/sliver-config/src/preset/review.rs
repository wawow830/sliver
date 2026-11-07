//! What a bundle would run, for the user to review before importing, and the bundle without it.

use super::*;

impl Bundle {
    /// Everything the preset would run or open: module scripts (`exec`, `exec-if`) and the
    /// command, D-Bus, open and URL actions of every gesture and menu entry, in every TOML file.
    pub fn runs(&self) -> Vec<Run> {
        let mut out = Vec::new();
        for (name, text) in self.toml_files() {
            match text.parse::<Table>() {
                Ok(t) => walk_runs(&Value::Table(t), name, "", &mut out),
                Err(_) => out.push(Run {
                    at: name.clone(),
                    what: "unreadable TOML (it won't load)".into(),
                }),
            }
        }
        out
    }

    /// The same preset with every script and command action removed (the review's "start
    /// disabled"). Returns how many were removed.
    pub fn without_commands(&self) -> (Bundle, usize) {
        let mut b = self.clone();
        let mut removed = 0;
        for (name, data) in &mut b.files {
            if !name.ends_with(".toml") {
                continue;
            }
            let Some(mut doc) = std::str::from_utf8(data)
                .ok()
                .and_then(|s| s.parse::<toml_edit::DocumentMut>().ok())
            else {
                continue;
            };
            removed += strip_table(doc.as_table_mut());
            *data = doc.to_string().into_bytes();
        }
        (b, removed)
    }
}

pub(super) fn walk_runs(v: &Value, file: &str, path: &str, out: &mut Vec<Run>) {
    match v {
        Value::Table(t) => {
            for (k, x) in t {
                let p = join(path, k);
                if k == "exec" || k == "exec-if" {
                    describe(x, &format!("{file}: {p}"), out);
                } else if k.starts_with("on-") {
                    actions(x, &format!("{file}: {p}"), out);
                } else {
                    walk_runs(x, file, &p, out);
                }
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                walk_runs(x, file, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// A module's `exec`: a shell string or an argv.
pub(super) fn describe(v: &Value, at: &str, out: &mut Vec<Run>) {
    let what = match v {
        Value::String(s) => format!("sh: {s}"),
        Value::Array(a) => format!("exec: {}", argv(a)),
        other => format!("exec: {other}"),
    };
    out.push(Run {
        at: at.to_owned(),
        what,
    });
}

pub(super) fn argv(a: &[Value]) -> String {
    a.iter()
        .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_owned))
        .collect::<Vec<_>>()
        .join(" ")
}

/// An action (`on-*`): what it runs, if anything. Keys, layers and layouts run nothing.
pub(super) fn actions(v: &Value, at: &str, out: &mut Vec<Run>) {
    let mut push = |what: String| {
        out.push(Run {
            at: at.to_owned(),
            what,
        })
    };
    match v {
        Value::String(s) => push(format!("sh: {s}")),
        Value::Array(steps) => {
            for (i, s) in steps.iter().enumerate() {
                actions(s, &format!("{at}[{i}]"), out);
            }
        }
        Value::Table(t) => {
            if let Some(x) = t.get("exec") {
                match x {
                    Value::Array(a) => push(format!("exec: {}", argv(a))),
                    other => push(format!("exec: {other}")),
                }
            }
            if let Some(Value::Table(d)) = t.get("dbus") {
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("");
                let bus = if s("bus").is_empty() {
                    "session"
                } else {
                    s("bus")
                };
                push(format!(
                    "dbus: {bus} {} {} {}.{}",
                    s("dest"),
                    s("path"),
                    s("interface"),
                    s("method")
                ));
            }
            for k in ["open", "url"] {
                if let Some(x) = t.get(k) {
                    push(format!(
                        "{k}: {}",
                        x.as_str().map_or_else(|| x.to_string(), str::to_owned)
                    ));
                }
            }
        }
        _ => {}
    }
}

pub(super) fn runs_something(v: &Value) -> bool {
    let mut out = Vec::new();
    actions(v, "", &mut out);
    !out.is_empty()
}

pub(super) fn item_value(i: &toml_edit::Item) -> Option<Value> {
    // Round-trip one item through text to judge it with the same rules as `runs`.
    let mut doc = toml_edit::DocumentMut::new();
    doc["x"] = i.clone();
    doc.to_string()
        .parse::<Table>()
        .ok()
        .and_then(|mut t| t.remove("x"))
}

pub(super) fn strip_table(t: &mut dyn toml_edit::TableLike) -> usize {
    let doomed: Vec<String> = t
        .iter()
        .filter(|(k, i)| {
            *k == "exec"
                || *k == "exec-if"
                || (k.starts_with("on-") && item_value(i).is_some_and(|v| runs_something(&v)))
        })
        .map(|(k, _)| k.to_owned())
        .collect();
    for k in &doomed {
        t.remove(k);
    }
    let mut n = doomed.len();
    for (_, item) in t.iter_mut() {
        n += strip_item(item);
    }
    n
}

pub(super) fn strip_item(i: &mut toml_edit::Item) -> usize {
    match i {
        toml_edit::Item::Table(t) => strip_table(t),
        toml_edit::Item::ArrayOfTables(a) => a.iter_mut().map(|t| strip_table(t)).sum(),
        toml_edit::Item::Value(v) => strip_value(v),
        toml_edit::Item::None => 0,
    }
}

pub(super) fn strip_value(v: &mut toml_edit::Value) -> usize {
    match v {
        toml_edit::Value::InlineTable(t) => strip_table(t),
        toml_edit::Value::Array(a) => a.iter_mut().map(strip_value).sum(),
        _ => 0,
    }
}
