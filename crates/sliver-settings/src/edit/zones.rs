//! Zones: add, remove, rename, reorder, and their keys.

use super::*;

/// The zone names `layout` shows, in order.
pub fn effective_zone_order(merged: &Table, layout: &str) -> Vec<String> {
    effective_zones(merged, layout)
        .iter()
        .filter_map(|z| zone_name(z).map(str::to_owned))
        .collect()
}

/// Does a parent of `layout` define (or inherit) `zone`?
fn inherited(merged: &Table, layout: &str, zone: &str) -> bool {
    chain(merged, layout).iter().skip(1).any(|l| {
        own_zones(merged, l)
            .iter()
            .any(|z| zone_name(z) == Some(zone))
    })
}

/// The user's file's own zone list for `layout`, materialized from the merged config first.
fn own_list(ed: &mut Editor, merged: &Table, layout: &str) -> Result<Vec<Table>, String> {
    if layout_raw(merged, layout).is_none() {
        return Err(format!("no layout {layout:?}"));
    }
    let path = zones_path(layout);
    match ed.get(&path)? {
        Some(Value::Array(a)) => Ok(a
            .into_iter()
            .filter_map(|z| z.as_table().cloned())
            .collect()),
        Some(_) => Err(format!("{path} isn't a list of zones")),
        None => Ok(own_zones(merged, layout)),
    }
}

fn set_own_list(ed: &mut Editor, layout: &str, zones: &[Table]) -> Result<(), String> {
    ed.set_tables(&zones_path(layout), zones)
}

fn order_path(layout: &str) -> String {
    format!("layouts.{}.zone-order", key(layout))
}

/// The user's `zone-order` for `layout`, or the merged one.
fn own_order(ed: &Editor, merged: &Table, layout: &str) -> Result<Option<Vec<String>>, String> {
    Ok(match ed.get(&order_path(layout))? {
        Some(Value::Array(a)) => Some(
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
        ),
        Some(_) => return Err("zone-order isn't a list".into()),
        None => order_of(merged, layout),
    })
}

fn set_order(ed: &mut Editor, layout: &str, order: &[String]) -> Result<(), String> {
    let v = Value::Array(order.iter().map(|n| Value::String(n.clone())).collect());
    ed.set(&order_path(layout), &v)
}

/// Zone names become stylesheet classes (`.zone-<name>`): keep them simple.
pub fn valid_zone_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "zone names use a-z, 0-9, - and _ (they become the class .zone-{name})"
        ));
    }
    Ok(())
}

/// Adds an empty zone `name` at the end of `layout`.
pub fn add_zone(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    name: &str,
) -> Result<String, String> {
    valid_zone_name(name)?;
    if effective_zone_order(merged, layout)
        .iter()
        .any(|n| n == name)
    {
        return Err(format!("{layout:?} already has a zone {name:?}"));
    }
    let mut zones = own_list(ed, merged, layout)?;
    let mut z = Table::new();
    z.insert("name".into(), Value::String(name.to_owned()));
    z.insert("modules".into(), Value::Array(Vec::new()));
    zones.push(z);
    set_own_list(ed, layout, &zones)?;
    if let Some(mut order) = own_order(ed, merged, layout)? {
        order.push(name.to_owned());
        set_order(ed, layout, &order)?;
    }
    Ok(format!("Added zone {name}"))
}

/// Removes zone `name` from `layout`. An inherited zone is hidden with `zone-order`.
pub fn remove_zone(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    name: &str,
) -> Result<String, String> {
    let order = effective_zone_order(merged, layout);
    if !order.iter().any(|n| n == name) {
        return Err(format!("{layout:?} has no zone {name:?}"));
    }
    if order.len() == 1 {
        return Err("a layout needs at least one zone".into());
    }
    let mut zones = own_list(ed, merged, layout)?;
    let before = zones.len();
    zones.retain(|z| zone_name(z) != Some(name));
    if zones.len() != before {
        set_own_list(ed, layout, &zones)?;
    }
    let explicit = own_order(ed, merged, layout)?;
    if inherited(merged, layout, name) || explicit.is_some() {
        let rest: Vec<String> = order.into_iter().filter(|n| n != name).collect();
        set_order(ed, layout, &rest)?;
    }
    Ok(format!("Removed zone {name}"))
}

/// Renames zone `from` of `layout` to `to`. An inherited zone becomes the child's own zone
/// under the new name (with its modules), and the old one is hidden.
pub fn rename_zone(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    from: &str,
    to: &str,
) -> Result<String, String> {
    valid_zone_name(to)?;
    let order = effective_zone_order(merged, layout);
    if !order.iter().any(|n| n == from) {
        return Err(format!("{layout:?} has no zone {from:?}"));
    }
    if from == to {
        return Err("same name".into());
    }
    if order.iter().any(|n| n == to) {
        return Err(format!("{layout:?} already has a zone {to:?}"));
    }
    let mut zones = own_list(ed, merged, layout)?;
    match zones.iter_mut().find(|z| zone_name(z) == Some(from)) {
        Some(z) => {
            z.insert("name".into(), Value::String(to.to_owned()));
        }
        None => {
            let mut z = effective_zone(merged, layout, from)
                .ok_or_else(|| format!("{layout:?} has no zone {from:?}"))?;
            z.insert("name".into(), Value::String(to.to_owned()));
            zones.push(z);
        }
    }
    set_own_list(ed, layout, &zones)?;
    let explicit = own_order(ed, merged, layout)?;
    if inherited(merged, layout, from) || explicit.is_some() {
        let renamed: Vec<String> = order
            .into_iter()
            .map(|n| if n == from { to.to_owned() } else { n })
            .collect();
        set_order(ed, layout, &renamed)?;
    }
    Ok(format!(
        "Renamed zone {from} to {to} (style rules for .zone-{from} won't apply to it)"
    ))
}

/// Moves zone `name` one place left (`delta = -1`) or right (`1`).
pub fn move_zone(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    name: &str,
    delta: isize,
) -> Result<String, String> {
    let mut order = effective_zone_order(merged, layout);
    let i = order
        .iter()
        .position(|n| n == name)
        .ok_or_else(|| format!("{layout:?} has no zone {name:?}"))?;
    let j = i
        .checked_add_signed(delta)
        .filter(|j| *j < order.len())
        .ok_or("can't move further")?;
    order.swap(i, j);
    let plain = chain(merged, layout).len() == 1 && own_order(ed, merged, layout)?.is_none();
    if plain {
        // No inheritance, no zone-order: reorder the list itself (tidier TOML).
        let mut zones = own_list(ed, merged, layout)?;
        zones.sort_by_key(|z| {
            zone_name(z)
                .and_then(|n| order.iter().position(|o| o == n))
                .unwrap_or(usize::MAX)
        });
        set_own_list(ed, layout, &zones)?;
    } else {
        set_order(ed, layout, &order)?;
    }
    Ok(format!("Moved zone {name}"))
}

/// Sets (or with `None`, clears) one key of zone `zone` (`width`, `align`, `module-width`).
pub fn set_zone_key(
    ed: &mut Editor,
    merged: &Table,
    layout: &str,
    zone: &str,
    k: &str,
    value: Option<Value>,
) -> Result<String, String> {
    let zi = ensure_zone(ed, merged, layout, zone)?;
    let path = format!("{}[{zi}].{}", zones_path(layout), key(k));
    match value {
        Some(v) => {
            ed.set(&path, &v)?;
            Ok(format!("Set {k} of zone {zone}"))
        }
        None => {
            ed.remove(&path)?;
            Ok(format!("Cleared {k} of zone {zone}"))
        }
    }
}
