//! Groups: their members in a row, drawers that open in place or as a sub-bar, and which
//! module of a zone a cell shows.

use super::*;

/// Which of a zone's modules a cell shows: `zone_ids` are the zone's top-level module ids in
/// order, `zone_keys` the keys of the zone's cells in order, and `pos` the cell's position in
/// them. Repeated modules are told apart by counting; a group's children are consecutive cells
/// of one module. (The settings preview and Mirror's `HeldPressed` both use it.)
pub fn module_index(zone_ids: &[String], zone_keys: &[String], pos: usize) -> Option<usize> {
    let top = |key: &str| {
        zone_ids
            .iter()
            .find(|id| key == id.as_str() || key.starts_with(&format!("{id}/")))
            .cloned()
    };
    let mut count: HashMap<String, usize> = HashMap::new();
    let mut prev: Option<String> = None;
    for (p, k) in zone_keys.iter().enumerate() {
        let t = top(k)?;
        let group_child = *k != t;
        if !(group_child && prev.as_ref() == Some(&t)) {
            *count.entry(t.clone()).or_default() += 1;
        }
        if p == pos {
            let nth = count[&t] - 1;
            return zone_ids
                .iter()
                .enumerate()
                .filter(|(_, id)| **id == t)
                .map(|(i, _)| i)
                .nth(nth);
        }
        prev = Some(t);
    }
    None
}

pub(super) fn is_bar_drawer(m: &ModuleInstance) -> bool {
    drawer_opt(m, "mode").and_then(toml::Value::as_str) == Some("bar")
}

pub(super) fn drawer_opt<'a>(m: &'a ModuleInstance, key: &str) -> Option<&'a toml::Value> {
    m.get("drawer").and_then(|d| d.get(key))
}

/// The cell keys of drawer groups with `start-expanded = true`, in `zones`. The agent opens each
/// the first time it sees it; after that, taps and `collapse-after` decide.
pub fn start_expanded_drawers(zones: &[sliver_config::ResolvedZone]) -> Vec<String> {
    fn walk(ms: &[ModuleInstance], prefix: &str, out: &mut Vec<String>) {
        for m in ms.iter().filter(|m| m.id.kind == "group") {
            let key = format!("{prefix}{}", m.id);
            if drawer_opt(m, "start-expanded").and_then(toml::Value::as_bool) == Some(true) {
                out.push(key.clone());
            }
            walk(&m.children, &format!("{key}/"), out);
        }
    }
    let mut out = Vec::new();
    for z in zones {
        walk(&z.modules, "", &mut out);
    }
    out
}

/// The cells of an open bar-mode drawer: the group's modules after its leader, then a ✕. `key`
/// is the group's cell key (as in [`Drawer::group`]); `None` when no such group is in `zones`.
pub fn subbar_cells(
    zones: &[sliver_config::ResolvedZone],
    key: &str,
    b: &mut Build,
) -> Option<Vec<Cell>> {
    fn find<'a>(ms: &'a [ModuleInstance], prefix: &str, key: &str) -> Option<&'a ModuleInstance> {
        for m in ms {
            let k = format!("{prefix}{}", m.id);
            if m.id.kind == "group" {
                if k == key {
                    return Some(m);
                }
                if let Some(f) = find(&m.children, &format!("{k}/"), key) {
                    return Some(f);
                }
            }
        }
        None
    }
    let group = zones.iter().find_map(|z| find(&z.modules, "", key))?;
    let mut out = Vec::new();
    let members = group.children.get(1..).unwrap_or_default();
    cells_in(members, &format!("{key}/"), b, &mut out);
    out.extend(menu_cells(&[], None, b.icons, b.icon_size));
    Some(out)
}

/// A group's members, flattened: all of them, or a closed drawer's handle alone (a bar-mode
/// drawer's rest shows as a sub-bar), with the group's classes.
pub(super) fn group_cells(m: &ModuleInstance, key: &str, b: &mut Build, out: &mut Vec<Cell>) {
    let drawer = m.has("drawer");
    let open = (b.ui)(key).expanded;
    // A closed drawer, and any bar-mode drawer, shows only its first module (the handle).
    let bar = drawer && is_bar_drawer(m);
    let shown = if drawer && (bar || !open) {
        1
    } else {
        m.children.len()
    };
    let start = out.len();
    let children = &m.children[..shown.min(m.children.len())];
    let child_prefix = format!("{key}/");
    // waybar's transition-left-to-right = false: the drawer opens towards the left, so
    // the leader comes after the rest.
    let rtl = drawer
        && drawer_opt(m, "transition-left-to-right").and_then(toml::Value::as_bool) == Some(false);
    let mut handle_at = start;
    if rtl && children.len() > 1 {
        let (leader, rest) = children.split_at(1);
        cells_in(rest, &child_prefix, b, out);
        handle_at = out.len();
        cells_in(leader, &child_prefix, b, out);
    } else {
        cells_in(children, &child_prefix, b, out);
    }
    if drawer && let Some(handle) = out.get_mut(handle_at) {
        let collapse_after =
            drawer_opt(m, "collapse-after").and_then(|v| sliver_config::parse_duration(v).ok());
        handle.drawer = Some(Drawer {
            group: key.to_owned(),
            collapse_after,
            opens: if bar { Opens::SubBar } else { Opens::Inline },
        });
        handle.look.checked = open;
        if handle.touch == Touch::None {
            handle.touch = Touch::Agent;
        }
    }
    let child_class = drawer.then(|| {
        drawer_opt(m, "children-class")
            .and_then(toml::Value::as_str)
            .unwrap_or("drawer-child")
            .to_owned()
    });
    for c in &mut out[start..] {
        c.look
            .classes
            .push(format!("group-{}", m.id.name.as_deref().unwrap_or("")));
        if let Some(class) = &child_class {
            c.look.classes.push(class.clone());
        }
    }
}
