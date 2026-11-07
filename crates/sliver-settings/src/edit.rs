//! Editing operations for the settings app. Each takes the user's file (an `Editor`) plus the
//! merged effective config and edits the file. No GTK here.
//!
//! TOML arrays replace and never merge, so changing one zone of a layout from the built-ins or an
//! include first copies that layout's whole effective zone list into the user's file
//! ("materialize"). A zone inherited through `extends` is copied into the child as a whole-zone
//! override.

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

/// `layout`'s zones as the config resolves them (`extends`, `zone-order`), as raw tables.
fn effective_zones(merged: &Table, layout: &str) -> Vec<Table> {
    let levels = chain(merged, layout)
        .into_iter()
        .rev()
        .map(|l| (own_zones(merged, &l), order_of(merged, &l)));
    sliver_config::inherit_zones(levels, |z| zone_name(z).unwrap_or_default())
}

/// The zone named `zone` as `layout` sees it: its own, or inherited through `extends`.
fn effective_zone(merged: &Table, layout: &str, zone: &str) -> Option<Table> {
    effective_zones(merged, layout)
        .into_iter()
        .find(|z| zone_name(z) == Some(zone))
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

/// `layout`'s `extends` chain, the layout itself first (just `layout` if it's broken: the
/// validator reports that).
fn chain(merged: &Table, layout: &str) -> Vec<String> {
    let parent = |l: &str| {
        let extends = layout_raw(merged, l)?
            .get("extends")
            .and_then(Value::as_str);
        Some(extends.map(str::to_owned))
    };
    sliver_config::extends_chain(layout, parent).unwrap_or_else(|_| vec![layout.to_owned()])
}

fn order_of(merged: &Table, layout: &str) -> Option<Vec<String>> {
    layout_raw(merged, layout)?
        .get("zone-order")?
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
}

mod modules;
mod rules;
mod zones;

pub use modules::*;
pub use rules::*;
pub use zones::*;

#[cfg(test)]
mod tests;
