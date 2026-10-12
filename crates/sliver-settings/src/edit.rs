//! Editing operations for the settings app (ux.md §4, Q3: instant apply + undo). They're pure:
//! each one takes the user's file (an `Editor`) plus the merged effective config and edits the
//! file. No GTK here, so they're unit-tested.
//!
//! The subtle part is that TOML arrays replace and never merge (ADR-0004 #6). To change one zone
//! of a layout that comes from the built-ins or an include, we first copy that layout's whole
//! effective zone list into the user's file ("materialize"). Otherwise the user's partial list
//! would silently drop the other zones. A zone inherited through `extends` is copied into the
//! child as an override (Q5: whole zones).

use sliver_config::{Editor, ModuleId};
use toml::{Table, Value};

fn layout_raw<'a>(merged: &'a Table, layout: &str) -> Option<&'a Table> {
    merged.get("layouts")?.as_table()?.get(layout)?.as_table()
}

fn own_zones(merged: &Table, layout: &str) -> Vec<Table> {
    layout_raw(merged, layout)
        .and_then(|l| l.get("zones"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|z| z.as_table().cloned()).collect())
        .unwrap_or_default()
}

fn zone_name(z: &Table) -> Option<&str> {
    z.get("name").and_then(Value::as_str)
}

/// The zone named `zone` as `layout` sees it: its own, or inherited through `extends`.
fn effective_zone(merged: &Table, layout: &str, zone: &str) -> Option<Table> {
    let mut cur = layout.to_owned();
    for _ in 0..sliver_config::MAX_DEPTH {
        if let Some(z) = own_zones(merged, &cur)
            .into_iter()
            .find(|z| zone_name(z) == Some(zone))
        {
            return Some(z);
        }
        cur = layout_raw(merged, &cur)?
            .get("extends")?
            .as_str()?
            .to_owned();
    }
    None
}

fn zones_path(layout: &str) -> String {
    format!("layouts.{}.zones", key(layout))
}

fn key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_owned()
    } else {
        Value::String(k.to_owned()).to_string()
    }
}

/// Makes sure the user's file holds `layout`'s zone `zone` and returns its index there.
pub fn ensure_zone(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
) -> Result<usize, String> {
    if layout_raw(merged, layout).is_none() {
        return Err(format!("no layout {layout:?}"));
    }
    let path = zones_path(layout);
    let mut zones: Vec<Table> = match ed.get(&path)? {
        Some(Value::Array(a)) => a
            .into_iter()
            .filter_map(|z| z.as_table().cloned())
            .collect(),
        Some(_) => return Err(format!("{path} isn't a list of zones")),
        None => {
            let own = own_zones(merged, layout);
            if !own.is_empty() {
                ed.set_tables(&path, &own)?;
            }
            own
        }
    };
    if let Some(i) = zones.iter().position(|z| zone_name(z) == Some(zone)) {
        return Ok(i);
    }
    let inherited = effective_zone(merged, layout, zone)
        .ok_or_else(|| format!("{layout:?} has no zone {zone:?}"))?;
    zones.push(inherited);
    ed.set_tables(&path, &zones)?;
    Ok(zones.len() - 1)
}

fn modules_path(layout: &str, zi: usize) -> String {
    format!("{}[{zi}].modules", zones_path(layout))
}

fn modules(ed: &Editor, layout: &str, zi: usize) -> Result<Vec<Value>, String> {
    match ed.get(&modules_path(layout, zi))? {
        Some(Value::Array(a)) => Ok(a),
        None => Ok(Vec::new()),
        Some(_) => Err("modules isn't a list".into()),
    }
}

pub fn add_module(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
    module: &str,
) -> Result<String, String> {
    insert_module(ed, merged, layout, zone, None, module)
}

/// Adds `module` to `zone` at position `at` (`None` = last), as a Library drop does.
pub fn insert_module(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
    at: Option<usize>,
    module: &str,
) -> Result<String, String> {
    ModuleId::parse(module)?;
    let zi = ensure_zone(ed, merged, layout, zone)?;
    let mut m = modules(ed, layout, zi)?;
    let at = at.unwrap_or(m.len()).min(m.len());
    m.insert(at, Value::String(module.to_owned()));
    ed.set(&modules_path(layout, zi), &Value::Array(m))?;
    Ok(format!("Added {module} to {zone}"))
}

pub fn remove_module(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
    index: usize,
) -> Result<String, String> {
    let zi = ensure_zone(ed, merged, layout, zone)?;
    let mut m = modules(ed, layout, zi)?;
    if index >= m.len() {
        return Err("no such module".into());
    }
    let gone = m.remove(index);
    ed.set(&modules_path(layout, zi), &Value::Array(m))?;
    Ok(format!("Removed {}", describe(&gone)))
}

/// Moves a module one step left (`delta = -1`) or right (`delta = 1`) within its zone.
pub fn move_module(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
    index: usize,
    delta: isize,
) -> Result<String, String> {
    let zi = ensure_zone(ed, merged, layout, zone)?;
    let mut m = modules(ed, layout, zi)?;
    let to = index
        .checked_add_signed(delta)
        .filter(|t| *t < m.len())
        .ok_or("can't move further")?;
    if index >= m.len() {
        return Err("no such module".into());
    }
    m.swap(index, to);
    ed.set(&modules_path(layout, zi), &Value::Array(m))?;
    Ok(format!(
        "Moved {}",
        describe(
            &ed.get(&format!("{}[{to}]", modules_path(layout, zi)))?
                .unwrap_or(Value::Boolean(false))
        )
    ))
}

/// Moves the module at `from` in `from_zone` to position `to` in `to_zone` (drag and drop).
/// `to` counts positions *before* the move, as the drop caret shows them.
pub fn move_module_to(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    from_zone: &str,
    from: usize,
    to_zone: &str,
    to: usize,
) -> Result<String, String> {
    let fz = ensure_zone(ed, merged, layout, from_zone)?;
    let tz = ensure_zone(ed, merged, layout, to_zone)?;
    let mut src = modules(ed, layout, fz)?;
    if from >= src.len() {
        return Err("no such module".into());
    }
    let m = src.remove(from);
    let label = describe(&m);
    if fz == tz {
        let to = if to > from { to - 1 } else { to }.min(src.len());
        if to == from {
            return Err("already there".into());
        }
        src.insert(to, m);
        ed.set(&modules_path(layout, fz), &Value::Array(src))?;
    } else {
        let mut dst = modules(ed, layout, tz)?;
        dst.insert(to.min(dst.len()), m);
        ed.set(&modules_path(layout, fz), &Value::Array(src))?;
        ed.set(&modules_path(layout, tz), &Value::Array(dst))?;
    }
    Ok(format!("Moved {label} to {to_zone}"))
}

fn describe(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Table(t) => t
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("module")
            .to_owned(),
        _ => "module".into(),
    }
}

/// Sets (or with `None`, clears) one key of the module at `index`. A named module
/// (`"battery#bat2"`) is configured in its `[modules."battery#bat2"]` block. An inline module
/// (`{ type = … }`) is edited in place.
pub fn set_module_key(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
    index: usize,
    k: &str,
    value: Option<Value>,
) -> Result<String, String> {
    // A named module's block lives outside the zone, so no need to materialize the zone for it.
    let raw = effective_zone(merged, layout, zone)
        .and_then(|z| {
            z.get("modules")
                .and_then(Value::as_array)
                .and_then(|a| a.get(index).cloned())
        })
        .ok_or("no such module")?;
    let path = match &raw {
        Value::String(id) => format!(
            "modules.{}.{}",
            key(&ModuleId::parse(id)?.to_string()),
            key(k)
        ),
        Value::Table(_) => {
            let zi = ensure_zone(ed, merged, layout, zone)?;
            format!("{}[{index}].{}", modules_path(layout, zi), key(k))
        }
        _ => return Err("not a module".into()),
    };
    let label = describe(&raw);
    match value {
        Some(v) => {
            ed.set(&path, &v)?;
            Ok(format!("Set {k} of {label}"))
        }
        None => {
            ed.remove(&path)?;
            Ok(format!("Cleared {k} of {label}"))
        }
    }
}

/// "Customize for app…" (ux.md §4): a layout extending the one shown now, plus a rule at the top
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
    let mut rules: Vec<Table> = match ed.get("rules")? {
        Some(Value::Array(a)) => a
            .into_iter()
            .filter_map(|r| r.as_table().cloned())
            .collect(),
        _ => Vec::new(),
    };
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
        // New app rules go first (ux.md §2: settings inserts by specificity; app rules are specific).
        rules.insert(0, rule);
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

/// Adds a rule at the top (new rules are usually the most specific, ux.md §2). Returns its index.
pub fn add_rule(ed: &mut Editor, merged: &Table, rule: Table) -> Result<String, String> {
    let mut all = rules(ed, merged)?;
    let label = rule_label(&rule, 0);
    all.insert(0, rule);
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
        ed.remove("rules")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use sliver_config::{Context, Hardware, load_str};

    const HERE: Hardware = Hardware { physical_esc: true };

    fn fresh(text: &str) -> (Editor, Table) {
        let ed = Editor::parse(text).unwrap();
        let merged = load_str(text, std::path::Path::new(".")).merged;
        (ed, merged)
    }

    fn resolved_ids(ed: &Editor, layer: &str, ctx: &Context) -> Vec<Vec<String>> {
        let l = ed.load();
        assert!(l.diagnostics.ok(), "{:#?}\n{}", l.diagnostics, ed.text());
        let r = l.config.resolve(layer, ctx).unwrap();
        r.zones
            .iter()
            .map(|z| z.modules.iter().map(|m| m.id.to_string()).collect())
            .collect()
    }

    #[test]
    fn editing_a_builtin_zone_keeps_the_others() {
        let (mut ed, merged) = fresh("version = 1\n# mine\n");
        add_module(&mut ed, &merged, "default", "main", "clock").unwrap();
        let ids = resolved_ids(&ed, "base", &Context::new());
        assert_eq!(
            ids.len(),
            3,
            "esc/main/controls all still there:\n{}",
            ed.text()
        );
        assert_eq!(ids[1], ["app-controls", "group/fkeys", "clock"]);
        assert!(ids[2].contains(&"mpris".to_owned()));
        assert!(ed.text().contains("# mine"));
        assert!(ed.check(HERE).ok());
    }

    #[test]
    fn remove_and_move() {
        let (mut ed, merged) = fresh("version = 1\n");
        remove_module(&mut ed, &merged, "fkeys", "main", 0).unwrap();
        let merged = ed.load().merged;
        move_module(&mut ed, &merged, "fkeys", "main", 0, 1).unwrap();
        let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
        assert_eq!(&ids[1][..3], ["key/F3", "key/F2", "key/F4"]);
        let merged = ed.load().merged;
        assert!(move_module(&mut ed, &merged, "fkeys", "main", 0, -1).is_err());
    }

    #[test]
    fn library_drop_inserts_at_the_caret() {
        let (mut ed, merged) = fresh("version = 1\n");
        insert_module(&mut ed, &merged, "fkeys", "main", Some(2), "clock").unwrap();
        let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
        assert_eq!(&ids[1][..4], ["key/F1", "key/F2", "clock", "key/F3"]);
        let merged = ed.load().merged;
        insert_module(&mut ed, &merged, "fkeys", "main", Some(99), "battery").unwrap();
        let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
        assert_eq!(
            ids[1].last().map(String::as_str),
            Some("battery"),
            "past the end = last"
        );
    }

    #[test]
    fn drag_and_drop_moves() {
        let (mut ed, merged) = fresh("version = 1\n");
        // F1 dropped after F3 (caret position 3, counted before the move).
        move_module_to(&mut ed, &merged, "fkeys", "main", 0, "main", 3).unwrap();
        let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", true));
        assert_eq!(&ids[1][..4], ["key/F2", "key/F3", "key/F1", "key/F4"]);
        let merged = ed.load().merged;
        assert!(
            move_module_to(&mut ed, &merged, "fkeys", "main", 0, "main", 1).is_err(),
            "no-op"
        );
        // Across zones: F12 into the esc zone, first.
        move_module_to(&mut ed, &merged, "fkeys", "main", 11, "esc", 0).unwrap();
        let ids = resolved_ids(&ed, "fn", &Context::new().with("physical_esc", false));
        assert_eq!(ids[0], ["key/F12", "key/Esc#auto"]);
        assert_eq!(ids[1].len(), 11);
    }

    #[test]
    fn inherited_zone_becomes_an_override() {
        let (mut ed, merged) = fresh("version = 1\n[layouts.ff]\nextends = \"default\"\n");
        add_module(&mut ed, &merged, "ff", "controls", "clock").unwrap();
        let t = ed.text();
        assert!(
            t.contains("[[layouts.ff.zones]]\nname = \"controls\""),
            "{t}"
        );
        let l = ed.load();
        let zones = l.config.layout_zones("ff").unwrap();
        assert_eq!(
            zones.iter().map(|z| z.name.as_str()).collect::<Vec<_>>(),
            ["esc", "main", "controls"]
        );
        assert_eq!(zones[2].modules.last().unwrap().id().to_string(), "clock");
        assert_eq!(
            l.config.layout_zones("default").unwrap()[2].modules.len(),
            6,
            "parent untouched"
        );
    }

    #[test]
    fn module_keys_named_and_inline() {
        let src = "version = 1\n[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"clock\", { type = \"custom\", format = \"hi\" }]\n";
        let (mut ed, merged) = fresh(src);
        set_module_key(
            &mut ed,
            &merged,
            "default",
            "main",
            0,
            "format",
            Some(Value::String("{:%H}".into())),
        )
        .unwrap();
        set_module_key(
            &mut ed,
            &merged,
            "default",
            "main",
            1,
            "format",
            Some(Value::String("yo".into())),
        )
        .unwrap();
        let merged = ed.load().merged;
        set_module_key(
            &mut ed,
            &merged,
            "default",
            "main",
            1,
            "on-tap",
            Some(Value::String("notify-send hi".into())),
        )
        .unwrap();
        assert_eq!(
            ed.get("modules.clock.format").unwrap(),
            Some(Value::String("{:%H}".into()))
        );
        assert_eq!(
            ed.get("layouts.default.zones[0].modules[1].format")
                .unwrap(),
            Some(Value::String("yo".into()))
        );
        let merged = ed.load().merged;
        set_module_key(&mut ed, &merged, "default", "main", 0, "format", None).unwrap();
        assert_eq!(ed.get("modules.clock.format").unwrap(), None);
        assert!(ed.check(HERE).ok(), "{:#?}", ed.check(HERE));
    }

    #[test]
    fn rules_crud_keeps_order_and_comments() {
        let src = "version = 1\n# my rules\n[[rules]]\nname = \"locked\"\nwhen = { locked = true }\nlayout = \"fkeys\"\n";
        let (mut ed, merged) = fresh(src);
        let mut r = Table::new();
        r.insert("name".into(), "terminals".into());
        r.insert(
            "when".into(),
            "v = { app-id = [\"foot\", \"kitty\"] }"
                .parse::<Table>()
                .unwrap()["v"]
                .clone(),
        );
        r.insert("layout".into(), "fkeys".into());
        add_rule(&mut ed, &merged, r).unwrap();
        let names = |ed: &Editor| {
            ed.load()
                .config
                .rules
                .iter()
                .map(|r| r.name.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&ed), ["terminals", "locked"]);
        let merged = ed.load().merged;
        move_rule(&mut ed, &merged, 1, -1).unwrap();
        assert_eq!(names(&ed), ["locked", "terminals"]);
        let merged = ed.load().merged;
        set_rule_key(&mut ed, &merged, 1, "layout", Some("media".into())).unwrap();
        let l = ed.load();
        assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
        let ctx = Context::new().with("app_id", "kitty");
        assert_eq!(l.config.resolve("base", &ctx).unwrap().layout, "media");
        assert!(ed.text().contains("# my rules"));
        remove_rule(&mut ed, &l.merged, 0).unwrap();
        let merged = ed.load().merged;
        remove_rule(&mut ed, &merged, 0).unwrap();
        assert!(ed.load().config.rules.is_empty());
        assert!(ed.check(HERE).ok());
        let merged = ed.load().merged;
        assert!(remove_rule(&mut ed, &merged, 0).is_err());
    }

    #[test]
    fn move_rule_down_with_comments() {
        let src = "version = 1\n\n[[rules]]\nname = \"New rule\"\nlayout = \"default\"\nwhen = { app-id = \"change-me\" }\n# terminals want F-keys\n[[rules]]\nname = \"Terminals\"\nwhen = { app-id = [\"foot\", \"kitty\"] }\nlayout = \"fkeys\"\n";
        let (mut ed, merged) = fresh(src);
        move_rule(&mut ed, &merged, 0, 1).unwrap();
        let names: Vec<_> = ed
            .load()
            .config
            .rules
            .iter()
            .map(|r| r.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["Terminals", "New rule"], "{}", ed.text());
    }

    #[test]
    fn customize_for_app_adds_layout_and_rule_first() {
        let (mut ed, merged) =
            fresh("version = 1\n[[rules]]\nwhen = { locked = true }\nlayout = \"fkeys\"\n");
        let (name, _) =
            customize_for_app(&mut ed, &merged, "org.mozilla.firefox", "default").unwrap();
        assert_eq!(name, "firefox");
        let l = ed.load();
        assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
        assert_eq!(l.config.rules[0].layout, "firefox");
        assert_eq!(l.config.rules[1].layout, "fkeys");
        let ctx = Context::new().with("app_id", "org.mozilla.firefox");
        assert_eq!(l.config.resolve("base", &ctx).unwrap().layout, "firefox");
        // Doing it twice doesn't duplicate the rule.
        customize_for_app(&mut ed, &l.merged, "org.mozilla.firefox", "default").unwrap();
        assert_eq!(ed.load().config.rules.len(), 2);
        assert!(customize_for_app(&mut ed, &l.merged, "  ", "default").is_err());
    }
}
