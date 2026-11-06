//! Rules, a layout for an app, and putting the F-keys back.

use super::*;

/// "Customize for app…": a layout extending the one shown now, plus a rule at the top
/// that picks it for `app_id`. Returns the new layout's name.
pub fn customize_for_app(
    ed: &mut Editor,
    merged: &Table,
    app_id: &str,
    base_layout: &str,
) -> Result<(String, String), String> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return Err("type an app id first (e.g. firefox)".into());
    }
    let name: String = app_id
        .rsplit('.')
        .next()
        .unwrap_or(app_id)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if layout_raw(merged, &name).is_none() {
        ed.set(
            &format!("layouts.{}.extends", key(&name)),
            &Value::String(base_layout.to_owned()),
        )?;
    }
    // The effective list (it may still be the built-in one), so the lock-screen rule survives.
    let mut rules = rules(ed, merged)?;
    let already = rules
        .iter()
        .any(|r| r.get("layout").and_then(Value::as_str) == Some(&name));
    if !already {
        let mut when = Table::new();
        when.insert("app-id".into(), Value::String(app_id.to_owned()));
        let mut rule = Table::new();
        rule.insert("name".into(), Value::String(app_id.to_owned()));
        rule.insert("when".into(), Value::Table(when));
        rule.insert("layout".into(), Value::String(name.clone()));
        // New app rules go first (settings inserts by specificity; app rules are
        // specific), but below the lock-screen rule, which must keep winning.
        rules.insert(top_slot(&rules), rule);
        ed.set_tables("rules", &rules)?;
    }
    Ok((
        name.clone(),
        format!("{app_id} now has its own layout “{name}” (extends {base_layout})"),
    ))
}

fn rules(ed: &Editor, merged: &Table) -> Result<Vec<Table>, String> {
    // Rules are an array: like zones, edit the effective list (built-ins have none, includes may).
    let raw = match ed.get("rules")? {
        Some(v) => Some(v),
        None => merged.get("rules").cloned(),
    };
    Ok(match raw {
        Some(Value::Array(a)) => a
            .into_iter()
            .filter_map(|r| r.as_table().cloned())
            .collect(),
        Some(_) => return Err("rules isn't a list".into()),
        None => Vec::new(),
    })
}

fn rule_label(r: &Table, i: usize) -> String {
    r.get("name")
        .and_then(Value::as_str)
        .map_or_else(|| format!("rules[{i}]"), str::to_owned)
}

/// Where new rules go: the top (they're usually the most specific), below any leading
/// lock-screen rules (`when = { locked = true }`), so an app's layout never shows while locked.
fn top_slot(rules: &[Table]) -> usize {
    rules
        .iter()
        .take_while(|r| {
            r.get("when").and_then(Value::as_table).is_some_and(|w| {
                w.iter().any(|(k, v)| {
                    sliver_config::canonical_key(k) == "locked" && v.as_bool() == Some(true)
                })
            })
        })
        .count()
}

/// Adds a rule at the top, below lock-screen rules (see [`top_slot`]).
pub fn add_rule(ed: &mut Editor, merged: &Table, rule: Table) -> Result<String, String> {
    let mut all = rules(ed, merged)?;
    let label = rule_label(&rule, 0);
    let at = top_slot(&all);
    all.insert(at, rule);
    ed.set_tables("rules", &all)?;
    Ok(format!("Added rule {label}"))
}

pub fn remove_rule(ed: &mut Editor, merged: &Table, index: usize) -> Result<String, String> {
    let mut all = rules(ed, merged)?;
    if index >= all.len() {
        return Err("no such rule".into());
    }
    let gone = all.remove(index);
    if all.is_empty() {
        // `rules = []`, not no key: without the key the built-in rules (the lock screen) come back.
        ed.set("rules", &Value::Array(Vec::new()))?;
    } else {
        ed.set_tables("rules", &all)?;
    }
    Ok(format!("Removed rule {}", rule_label(&gone, index)))
}

/// Moves a rule up (`-1`) or down (`1`): order is priority, first match wins.
pub fn move_rule(
    ed: &mut Editor,
    merged: &Table,
    index: usize,
    delta: isize,
) -> Result<String, String> {
    let mut all = rules(ed, merged)?;
    let to = index
        .checked_add_signed(delta)
        .filter(|t| *t < all.len())
        .ok_or("can't move further")?;
    if index >= all.len() {
        return Err("no such rule".into());
    }
    all.swap(index, to);
    ed.set_tables("rules", &all)?;
    Ok(format!("Moved rule {}", rule_label(&all[to], to)))
}

/// Sets or clears one key of rule `index` (`name`, `when`, `layer`, `layout`).
pub fn set_rule_key(
    ed: &mut Editor,
    merged: &Table,
    index: usize,
    k: &str,
    value: Option<Value>,
) -> Result<String, String> {
    let mut all = rules(ed, merged)?;
    let r = all.get_mut(index).ok_or("no such rule")?;
    match value {
        Some(v) => {
            r.insert(k.to_owned(), v);
        }
        None => {
            r.remove(k);
        }
    }
    let label = rule_label(r, index);
    ed.set_tables("rules", &all)?;
    Ok(format!("Changed {k} of rule {label}"))
}

/// Does this report say F1–F12 are only reachable through the safe hatch? (The validator's
/// warning for a config whose base and Fn layers both lack them.)
pub fn fkeys_missing_warning(d: &sliver_config::Diagnostics) -> bool {
    d.warnings
        .iter()
        .any(|w| w.message.contains("F1, F2") && w.message.contains("safe hatch"))
}

/// The layout the Fn layer shows and the F-keys it lacks, as `key/F1`…
pub fn missing_fkeys(config: &sliver_config::Config) -> Option<(String, Vec<String>)> {
    fn keys(m: &sliver_config::ModuleInstance, out: &mut Vec<String>) {
        if m.id.kind == "key" {
            out.extend(m.id.name.clone());
        }
        for c in &m.children {
            keys(c, out);
        }
    }
    let layout = config.layers.get("fn")?.clone();
    let r = config
        .resolve(
            "fn",
            &sliver_config::Context::new().with("physical_esc", true),
        )
        .ok()?;
    let mut have = Vec::new();
    for z in &r.zones {
        for m in &z.modules {
            keys(m, &mut have);
        }
    }
    let missing = (1..=12)
        .map(|n| format!("F{n}"))
        .filter(|k| !have.contains(k))
        .map(|k| format!("key/{k}"))
        .collect();
    Some((layout, missing))
}

/// Puts the given keys back at the end of the layout's `main` zone.
pub fn add_keys(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    keys: &[String],
) -> Result<String, String> {
    for k in keys {
        add_module(ed, merged, layout, "main", k)?;
    }
    Ok(format!("Added {} F-keys to the Fn layer", keys.len()))
}
