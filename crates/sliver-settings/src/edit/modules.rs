//! Modules in zones and groups, and their keys.

use super::*;

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

fn group_path(group: &str) -> String {
    format!("modules.{}.modules", key(group))
}

/// A group's members: the user's list, else the effective one (e.g. the built-in control strip).
pub fn group_members(ed: &Editor, merged: &Table, group: &str) -> Result<Vec<Value>, String> {
    match ed.get(&group_path(group))? {
        Some(Value::Array(a)) => return Ok(a),
        Some(_) => return Err(format!("{group}: modules isn't a list")),
        None => {}
    }
    Ok(merged
        .get("modules")
        .and_then(|m| m.get(group))
        .and_then(|g| g.get("modules"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn set_members(ed: &mut Editor, group: &str, members: Vec<Value>) -> Result<(), String> {
    ed.set(&group_path(group), &Value::Array(members))
}

/// Adds `module` at the end of `group`.
pub fn add_group_member(
    ed: &mut Editor,
    merged: &Table,
    group: &str,
    module: &str,
) -> Result<String, String> {
    ModuleId::parse(module)?;
    if module == group {
        return Err("a group can't contain itself".into());
    }
    let mut m = group_members(ed, merged, group)?;
    m.push(Value::String(module.to_owned()));
    set_members(ed, group, m)?;
    Ok(format!("Added {module} to {group}"))
}

pub fn remove_group_member(
    ed: &mut Editor,
    merged: &Table,
    group: &str,
    index: usize,
) -> Result<String, String> {
    let mut m = group_members(ed, merged, group)?;
    if index >= m.len() {
        return Err("no such member".into());
    }
    let gone = m.remove(index);
    set_members(ed, group, m)?;
    Ok(format!("Removed {} from {group}", describe(&gone)))
}

/// Moves member `index` one step left (`-1`) or right (`1`).
pub fn move_group_member(
    ed: &mut Editor,
    merged: &Table,
    group: &str,
    index: usize,
    delta: isize,
) -> Result<String, String> {
    let mut m = group_members(ed, merged, group)?;
    let to = index as isize + delta;
    if index >= m.len() || to < 0 || to as usize >= m.len() {
        return Err("can't move further".into());
    }
    m.swap(index, to as usize);
    let label = describe(&m[to as usize]);
    set_members(ed, group, m)?;
    Ok(format!("Moved {label} in {group}"))
}

/// Makes `group/<name>` from the modules at `indices` of `zone` (in zone order) and puts it in
/// the zone where the first of them was.
pub fn make_group(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
    indices: &[usize],
    name: &str,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("a group name needs letters, digits, - or _".into());
    }
    let group = format!("group/{name}");
    if merged.get("modules").and_then(|m| m.get(&group)).is_some() {
        return Err(format!("{group} already exists"));
    }
    let zi = ensure_zone(ed, merged, layout, zone)?;
    let all = modules(ed, layout, zi)?;
    let mut picked: Vec<usize> = indices.iter().copied().filter(|&i| i < all.len()).collect();
    picked.sort_unstable();
    picked.dedup();
    let Some(&first) = picked.first() else {
        return Err("pick at least one module".into());
    };
    let members: Vec<Value> = picked.iter().map(|&i| all[i].clone()).collect();
    let mut rest: Vec<Value> = Vec::new();
    for (i, m) in all.into_iter().enumerate() {
        if i == first {
            rest.push(Value::String(group.clone()));
        }
        if !picked.contains(&i) {
            rest.push(m);
        }
    }
    let n = members.len();
    set_members(ed, &group, members)?;
    ed.set(&modules_path(layout, zi), &Value::Array(rest))?;
    Ok(format!("Grouped {n} modules as {group}"))
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

/// Sets (or with `None` clears) key `k` of the named module `id` (`[modules."id"]`): for a
/// module inside a group, which has no zone slot of its own.
pub fn set_named_module_key(
    ed: &mut Editor,
    id: &str,
    k: &str,
    value: Option<Value>,
) -> Result<String, String> {
    let id = ModuleId::parse(id)?.to_string();
    let path = format!("modules.{}.{}", key(&id), key(k));
    match value {
        Some(v) => {
            ed.set(&path, &v)?;
            Ok(format!("Set {k} of {id}"))
        }
        None => {
            ed.remove(&path)?;
            Ok(format!("Cleared {k} of {id}"))
        }
    }
}

/// What turns module block `old` into `new`: changed or new keys, and `None` for removed ones.
/// Sorted by key, so the file changes predictably.
pub fn block_changes(old: &Table, new: &Table) -> Vec<(String, Option<Value>)> {
    let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| old.get(*k) != new.get(*k))
        .map(|k| (k.clone(), new.get(k).cloned()))
        .collect()
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
