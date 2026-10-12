//! dbusmenu (`com.canonical.dbusmenu`): the menu behind a tray item's `Menu` path, as a tree
//! the Touch Bar can show as a sub-bar.

use crate::tray::{TrayItem, parse_item_address};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

const IFACE: &str = "com.canonical.dbusmenu";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Toggle {
    #[default]
    None,
    Check,
    Radio,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuItem {
    pub id: i32,
    /// Label without mnemonic underscores.
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub separator: bool,
    pub toggle: Toggle,
    /// For `toggle != None`: current state (`toggle-state` 1).
    pub checked: bool,
    pub icon_name: String,
    pub children: Vec<MenuItem>,
}

/// dbusmenu labels mark mnemonics with `_`; `__` is a literal underscore.
pub fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

type Props = HashMap<String, OwnedValue>;
type Node = (i32, Props, Vec<OwnedValue>);

fn prop<T: TryFrom<OwnedValue>>(p: &Props, k: &str) -> Option<T> {
    T::try_from(p.get(k)?.try_clone().ok()?).ok()
}

/// Decode one node and its children (bounded depth: a hostile menu can't recurse forever).
fn parse_node(node: Node, depth: u8) -> MenuItem {
    let (id, p, kids) = node;
    let toggle = match prop::<String>(&p, "toggle-type").as_deref() {
        Some("checkmark") => Toggle::Check,
        Some("radio") => Toggle::Radio,
        _ => Toggle::None,
    };
    let children = if depth == 0 {
        Vec::new()
    } else {
        kids.into_iter()
            .filter_map(|k| Node::try_from(k).ok())
            .map(|n| parse_node(n, depth - 1))
            .collect()
    };
    MenuItem {
        id,
        label: strip_mnemonic(&prop::<String>(&p, "label").unwrap_or_default()),
        enabled: prop::<bool>(&p, "enabled").unwrap_or(true),
        visible: prop::<bool>(&p, "visible").unwrap_or(true),
        separator: prop::<String>(&p, "type").as_deref() == Some("separator"),
        toggle,
        checked: prop::<i32>(&p, "toggle-state") == Some(1),
        icon_name: prop::<String>(&p, "icon-name").unwrap_or_default(),
        children,
    }
}

/// Fetch the item's menu (the children of the root node). Invisible entries are kept; the
/// caller decides (they can become visible via `LayoutUpdated`).
pub fn layout(conn: &Connection, item: &TrayItem) -> zbus::Result<Vec<MenuItem>> {
    let Some(menu) = item.menu.as_deref() else {
        return Ok(Vec::new());
    };
    let (dest, _) = parse_item_address(&item.key);
    // Etiquette: tell the app the menu is about to be shown so it can refresh dynamic entries.
    let _ = conn.call_method(
        Some(dest.as_str()),
        menu,
        Some(IFACE),
        "AboutToShow",
        &(0i32,),
    );
    let reply = conn.call_method(
        Some(dest.as_str()),
        menu,
        Some(IFACE),
        "GetLayout",
        &(0i32, 3i32, Vec::<&str>::new()),
    )?;
    let (_rev, root): (u32, Node) = reply.body().deserialize()?;
    Ok(parse_node(root, 4).children)
}

/// Click a menu entry.
pub fn click(conn: &Connection, item: &TrayItem, id: i32) -> zbus::Result<()> {
    let Some(menu) = item.menu.as_deref() else {
        return Ok(());
    };
    let (dest, _) = parse_item_address(&item.key);
    conn.call_method(
        Some(dest.as_str()),
        menu,
        Some(IFACE),
        "Event",
        &(id, "clicked", Value::from(0i32), 0u32),
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonics() {
        assert_eq!(strip_mnemonic("_File"), "File");
        assert_eq!(strip_mnemonic("Save _As"), "Save As");
        assert_eq!(strip_mnemonic("a__b"), "a_b");
        assert_eq!(strip_mnemonic("plain"), "plain");
    }

    fn props(pairs: &[(&str, Value)]) -> Props {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OwnedValue::try_from(v.clone()).unwrap()))
            .collect()
    }

    #[test]
    fn decodes_nested_layout_and_defaults() {
        let leaf = (
            3,
            props(&[
                ("label", Value::from("_Mute")),
                ("toggle-type", Value::from("checkmark")),
                ("toggle-state", Value::from(1i32)),
            ]),
            Vec::<OwnedValue>::new(),
        );
        let sep = (4, props(&[("type", Value::from("separator"))]), Vec::new());
        let sub = (
            2,
            props(&[
                ("label", Value::from("Sub")),
                ("enabled", Value::from(false)),
            ]),
            vec![
                OwnedValue::try_from(Value::Structure(
                    zbus::zvariant::StructureBuilder::new()
                        .add_field(leaf.0)
                        .add_field(leaf.1.clone())
                        .add_field(Vec::<OwnedValue>::new())
                        .build()
                        .unwrap(),
                ))
                .unwrap(),
            ],
        );
        let wrap = |n: Node| {
            OwnedValue::try_from(Value::Structure(
                zbus::zvariant::StructureBuilder::new()
                    .add_field(n.0)
                    .add_field(n.1)
                    .add_field(n.2)
                    .build()
                    .unwrap(),
            ))
            .unwrap()
        };
        let root = (0, Props::new(), vec![wrap(sub), wrap(sep)]);
        let m = parse_node(root, 4);
        assert_eq!(m.children.len(), 2);
        let sub = &m.children[0];
        assert_eq!(
            (sub.label.as_str(), sub.enabled, sub.visible),
            ("Sub", false, true)
        );
        let mute = &sub.children[0];
        assert_eq!(mute.label, "Mute");
        assert_eq!((mute.toggle, mute.checked), (Toggle::Check, true));
        assert!(m.children[1].separator);
    }
}
