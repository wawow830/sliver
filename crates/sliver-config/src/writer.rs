//! Editing config files while keeping the user's comments and formatting (customization.md G9).
//! The settings app and `sliverctl config set` go through this, and nothing else writes config.
//!
//! Key paths use the spelling from diagnostics: `bar.spacing`, `modules."custom/vpn".format`,
//! `layouts.default.zones[1].modules`, `rules[0].layout`.

use std::io;
use std::path::{Path, PathBuf};

use toml::Value;
use toml_edit::{DocumentMut, Item};

use crate::diag::Diagnostics;
use crate::load::{Loaded, load_str};
use crate::validate::Hardware;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// Parses `a.b."c/d"[2].e`.
pub fn parse_path(path: &str) -> Result<Vec<Seg>, String> {
    let mut segs = Vec::new();
    let mut chars = path.trim().chars().peekable();
    let err = |m: &str| format!("bad key path {path:?}: {m}");
    loop {
        match chars.peek() {
            None => break,
            Some('"') => {
                chars.next();
                let mut k = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => k.extend(chars.next()),
                        Some(c) => k.push(c),
                        None => return Err(err("unclosed quote")),
                    }
                }
                segs.push(Seg::Key(k));
            }
            Some('[') => {
                chars.next();
                let n: String = chars.by_ref().take_while(|c| *c != ']').collect();
                segs.push(Seg::Index(
                    n.trim()
                        .parse()
                        .map_err(|_| err("index must be a number"))?,
                ));
            }
            Some(_) => {
                let mut k = String::new();
                while let Some(&c) = chars.peek() {
                    if c == '.' || c == '[' {
                        break;
                    }
                    k.push(c);
                    chars.next();
                }
                if k.is_empty() {
                    return Err(err("empty key"));
                }
                segs.push(Seg::Key(k));
            }
        }
        match chars.peek() {
            Some('.') => {
                chars.next();
                if chars.peek().is_none() {
                    return Err(err("trailing '.'"));
                }
            }
            Some('[') | None => {}
            Some(c) => return Err(err(&format!("unexpected {c:?}"))),
        }
    }
    if segs.is_empty() {
        return Err(err("empty"));
    }
    Ok(segs)
}

/// Parses a command-line value: TOML if it parses (`8`, `true`, `"x"`, `[1, 2]`, `{ key = "F1" }`),
/// otherwise a bare string (`sliverctl config set modules.clock.format '{:%H:%M}'`).
pub fn parse_cli_value(s: &str) -> Value {
    format!("v = {s}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::String(s.to_owned()))
}

/// A config file open for editing.
#[derive(Debug, Clone)]
pub struct Editor {
    path: Option<PathBuf>,
    doc: DocumentMut,
}

impl Editor {
    /// Opens a config file. A missing file starts as `version = 1`.
    pub fn open(path: &Path) -> io::Result<Editor> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => "version = 1\n".to_owned(),
            Err(e) => return Err(e),
        };
        let mut ed =
            Editor::parse(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        ed.path = Some(path.to_path_buf());
        Ok(ed)
    }

    pub fn parse(text: &str) -> Result<Editor, String> {
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| format!("TOML syntax: {e}"))?;
        Ok(Editor { path: None, doc })
    }

    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// The value at `path`, as plain TOML.
    pub fn get(&self, path: &str) -> Result<Option<Value>, String> {
        let segs = parse_path(path)?;
        let table: toml::Table = self.text().parse().map_err(|e| format!("{e}"))?;
        let mut cur = Value::Table(table);
        for s in &segs {
            let next = match (s, &cur) {
                (Seg::Key(k), Value::Table(t)) => t.get(k).cloned(),
                (Seg::Index(i), Value::Array(a)) => a.get(*i).cloned(),
                _ => None,
            };
            match next {
                Some(v) => cur = v,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// Sets `path` to `value`, creating parent tables as needed. Comments elsewhere stay put.
    pub fn set(&mut self, path: &str, value: &Value) -> Result<(), String> {
        let segs = parse_path(path)?;
        let (last, parents) = segs.split_last().expect("non-empty");
        let mut item = self.doc.as_item_mut();
        for (depth, s) in parents.iter().enumerate() {
            let create = match segs.get(depth + 1) {
                Some(Seg::Index(_)) => Create::Array {
                    tables: depth + 2 == segs.len() && matches!(value, Value::Table(_)),
                },
                _ => Create::Table,
            };
            item = step_mut(item, s, create)
                .ok_or_else(|| format!("can't descend into {}", show(&segs[..=depth])))?;
        }
        let new = to_item(value, item.is_table() || item.is_array_of_tables());
        match last {
            Seg::Key(k) => {
                let mut new = new;
                let tl = item
                    .as_table_like_mut()
                    .ok_or_else(|| format!("{} isn't a table", show(parents)))?;
                // Keep a replaced value's surrounding whitespace and trailing comment.
                if let (Some(old), Some(nv)) =
                    (tl.get(k).and_then(Item::as_value), new.as_value_mut())
                {
                    *nv.decor_mut() = old.decor().clone();
                }
                tl.insert(k, new);
                if let Some(it) = item.as_inline_table_mut() {
                    it.fmt();
                }
            }
            Seg::Index(i) => {
                if let Some(a) = item.as_array_of_tables_mut() {
                    let Item::Table(t) = to_item(value, true) else {
                        return Err("[[array of tables]] entries must be tables".into());
                    };
                    let len = a.len();
                    match a.get_mut(*i) {
                        Some(slot) => *slot = t,
                        None if *i == len => a.push(t),
                        None => return Err(format!("{} has no index {i}", show(parents))),
                    }
                } else if let Some(a) = item.as_array_mut() {
                    let v = new
                        .into_value()
                        .map_err(|_| "array items must be values".to_owned())?;
                    match *i {
                        i if i < a.len() => {
                            a.replace(i, v);
                        }
                        i if i == a.len() => a.push_formatted(v),
                        _ => return Err(format!("{} has no index {i}", show(parents))),
                    }
                } else {
                    return Err(format!("{} isn't an array", show(parents)));
                }
            }
        }
        Ok(())
    }

    /// Removes `path`. Returns whether something was there.
    pub fn remove(&mut self, path: &str) -> Result<bool, String> {
        let segs = parse_path(path)?;
        let (last, parents) = segs.split_last().expect("non-empty");
        let mut item = self.doc.as_item_mut();
        for s in parents {
            match step_mut(item, s, Create::No) {
                Some(i) => item = i,
                None => return Ok(false),
            }
        }
        Ok(match last {
            Seg::Key(k) => item.as_table_like_mut().and_then(|t| t.remove(k)).is_some(),
            Seg::Index(i) => {
                if let Some(a) = item.as_array_of_tables_mut().filter(|a| *i < a.len()) {
                    a.remove(*i);
                    true
                } else if let Some(a) = item.as_array_mut().filter(|a| *i < a.len()) {
                    a.remove(*i);
                    true
                } else {
                    false
                }
            }
        })
    }

    /// Loads the edited text as a config: includes are resolved next to the file.
    pub fn load(&self) -> Loaded {
        let dir = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(Path::new("."));
        load_str(&self.text(), dir)
    }

    /// Parse plus whole-config diagnostics for the edited text.
    pub fn check(&self, hw: Hardware) -> Diagnostics {
        let l = self.load();
        let mut d = l.diagnostics;
        if d.ok() {
            d.extend(l.config.validate(hw));
        }
        d
    }

    /// Validates, then writes atomically (temp file in the same directory, then rename). Refuses if
    /// there are errors, so a broken config never lands on disk from our tools.
    pub fn save(&self, hw: Hardware) -> Result<Diagnostics, Diagnostics> {
        let d = self.check(hw);
        if !d.ok() {
            return Err(d);
        }
        let path = self
            .path
            .as_deref()
            .expect("save() needs an editor from open()");
        if let Err(e) = write_atomic(path, &self.text()) {
            let mut d = d;
            d.error(path.display().to_string(), format!("can't write: {e}"));
            return Err(d);
        }
        Ok(d)
    }
}

fn show(segs: &[Seg]) -> String {
    let mut s = String::new();
    for seg in segs {
        match seg {
            Seg::Key(k) => s = crate::diag::join(&s, k),
            Seg::Index(i) => s.push_str(&format!("[{i}]")),
        }
    }
    s
}

/// What to create when a path step is missing.
#[derive(Clone, Copy, PartialEq)]
enum Create {
    No,
    Table,
    /// The next segment is an index. `tables` = it will hold tables (`[[x]]` when possible).
    Array {
        tables: bool,
    },
}

fn step_mut<'a>(item: &'a mut Item, s: &Seg, create: Create) -> Option<&'a mut Item> {
    match s {
        Seg::Key(k) => {
            let is_section = item.is_table();
            let tl = item.as_table_like_mut()?;
            if tl.get(k).is_none() {
                let child = match (create, is_section) {
                    (Create::No, _) => return None,
                    (Create::Table, true) => {
                        let mut t = toml_edit::Table::new();
                        t.set_implicit(true);
                        Item::Table(t)
                    }
                    (Create::Table, false) => Item::Value(toml_edit::InlineTable::new().into()),
                    (Create::Array { tables: true }, true) => {
                        Item::ArrayOfTables(toml_edit::ArrayOfTables::new())
                    }
                    (Create::Array { .. }, _) => Item::Value(toml_edit::Array::new().into()),
                };
                tl.insert(k, child);
            }
            item.get_mut(k.as_str())
        }
        Seg::Index(i) => item.get_mut(*i),
    }
}

fn to_item(v: &Value, section_ok: bool) -> Item {
    // Render through text so toml_edit formats it the default way.
    let src = format!("v = {}", inline(v));
    let doc: DocumentMut = src.parse().expect("rendered TOML parses");
    let value = doc["v"].clone();
    match (section_ok, &value) {
        (true, Item::Value(toml_edit::Value::InlineTable(t))) if !t.is_empty() => {
            Item::Table(t.clone().into_table())
        }
        _ => value,
    }
}

/// A value as inline TOML.
pub(crate) fn inline(v: &Value) -> String {
    match v {
        Value::Table(t) => {
            let parts: Vec<String> = t
                .iter()
                .map(|(k, v)| format!("{} = {}", key_str(k), inline(v)))
                .collect();
            if parts.is_empty() {
                "{}".into()
            } else {
                format!("{{ {} }}", parts.join(", "))
            }
        }
        Value::Array(a) => format!("[{}]", a.iter().map(inline).collect::<Vec<_>>().join(", ")),
        scalar => scalar.to_string(),
    }
}

pub(crate) fn key_str(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_owned()
    } else {
        Value::String(k.to_owned()).to_string()
    }
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".into());
    let tmp = dir.join(format!(".{name}.sliver-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: Hardware = Hardware { physical_esc: true };

    const SAMPLE: &str = r#"# my sliver config
version = 1

[bar]
spacing = 8 # roomy

# clock tweaks
[modules.clock]
format = "{:%H:%M}"
actions = { on-hold = "mode" }

[[rules]]
when = { app-id = "foot" }   # terminals
layout = "fkeys"
"#;

    #[test]
    fn paths() {
        assert_eq!(
            parse_path(r#"modules."custom/vpn".format"#).unwrap(),
            [
                Seg::Key("modules".into()),
                Seg::Key("custom/vpn".into()),
                Seg::Key("format".into())
            ]
        );
        assert_eq!(
            parse_path("layouts.default.zones[1].modules[0]").unwrap(),
            [
                Seg::Key("layouts".into()),
                Seg::Key("default".into()),
                Seg::Key("zones".into()),
                Seg::Index(1),
                Seg::Key("modules".into()),
                Seg::Index(0)
            ]
        );
        for bad in ["", "a.", "a..b", "a[x]", "\"open"] {
            assert!(parse_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn cli_values() {
        assert_eq!(parse_cli_value("8"), Value::Integer(8));
        assert_eq!(parse_cli_value("true"), Value::Boolean(true));
        assert_eq!(
            parse_cli_value("{:%H:%M}"),
            Value::String("{:%H:%M}".into())
        );
        assert_eq!(
            parse_cli_value("\"quoted\""),
            Value::String("quoted".into())
        );
        assert!(matches!(
            parse_cli_value(r#"{ key = "F1" }"#),
            Value::Table(_)
        ));
    }

    #[test]
    fn edits_keep_comments() {
        let mut e = Editor::parse(SAMPLE).unwrap();
        e.set("bar.spacing", &Value::Integer(4)).unwrap();
        e.set("modules.clock.format", &Value::String("{:%H:%M:%S}".into()))
            .unwrap();
        e.set(
            "modules.clock.actions.on-swipe-left",
            &Value::String("shift_down".into()),
        )
        .unwrap();
        e.set(
            r#"modules."custom/vpn".format"#,
            &Value::String("vpn".into()),
        )
        .unwrap();
        e.set("rules[0].layout", &Value::String("media".into()))
            .unwrap();
        let t = e.text();
        for kept in [
            "# my sliver config",
            "# clock tweaks",
            "# terminals",
            "# roomy",
        ] {
            assert!(t.contains(kept), "{kept:?} lost:\n{t}");
        }
        assert!(
            t.contains("[modules.\"custom/vpn\"]\nformat = \"vpn\""),
            "{t}"
        );
        assert!(
            !t.contains("[modules]\n"),
            "implicit parent stays implicit:\n{t}"
        );
        assert_eq!(e.get("bar.spacing").unwrap(), Some(Value::Integer(4)));
        assert_eq!(
            e.get("rules[0].layout").unwrap(),
            Some(Value::String("media".into()))
        );
        assert_eq!(
            e.get("modules.clock.actions.on-swipe-left").unwrap(),
            Some(Value::String("shift_down".into())),
            "inline table edited in place:\n{t}"
        );
        assert_eq!(e.get("nope.nothing").unwrap(), None);
    }

    #[test]
    fn arrays_and_removal() {
        let mut e = Editor::parse(SAMPLE).unwrap();
        let zone: Value =
            r#"v = { name = "main", modules = ["clock"] }"#.parse::<toml::Table>().unwrap()["v"]
                .clone();
        e.set("layouts.default.zones", &Value::Array(vec![]))
            .unwrap();
        e.set("layouts.default.zones[0]", &zone).unwrap();
        e.set(
            "layouts.default.zones[0].modules[1]",
            &Value::String("battery".into()),
        )
        .unwrap();
        let mut rule = toml::Table::new();
        rule.insert("layout".into(), "media".into());
        e.set("rules[1]", &Value::Table(rule)).unwrap();
        assert_eq!(
            e.get("layouts.default.zones[0].modules[1]").unwrap(),
            Some(Value::String("battery".into()))
        );
        assert_eq!(
            e.get("rules[1].layout").unwrap(),
            Some(Value::String("media".into()))
        );
        assert!(
            e.set("rules[5].layout", &Value::String("x".into()))
                .is_err()
        );

        let mut fresh = Editor::parse("version = 1\n").unwrap();
        let rule: Value = r#"v = { layout = "fkeys", when = { app-id = "foot" } }"#
            .parse::<toml::Table>()
            .unwrap()["v"]
            .clone();
        fresh.set("rules[0]", &rule).unwrap();
        fresh.set("bar.list[0]", &Value::Integer(1)).unwrap();
        assert!(fresh.text().contains("[[rules]]"), "{}", fresh.text());
        assert_eq!(
            fresh.get("bar.list").unwrap(),
            Some(Value::Array(vec![Value::Integer(1)]))
        );
        fresh.remove("bar.list").unwrap();
        assert!(fresh.check(HERE).ok(), "{:#?}", fresh.check(HERE));

        assert!(e.remove("modules.clock.actions").unwrap());
        assert!(e.remove("rules[0]").unwrap());
        assert!(!e.remove("modules.nothing.here").unwrap());
        assert_eq!(
            e.get("rules[0].layout").unwrap(),
            Some(Value::String("media".into()))
        );
        assert!(e.text().contains("# clock tweaks"));
    }

    #[test]
    fn save_validates_and_is_atomic() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("sliver-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sliver/config.toml");
        let mut e = Editor::open(&path).unwrap();
        e.set("bar.spacing", &Value::Integer(6)).unwrap();
        e.save(HERE).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("spacing = 6")
        );

        let mut e = Editor::open(&path).unwrap();
        e.set("layers.base", &Value::String("no-such-layout".into()))
            .unwrap();
        let err = e.save(HERE).unwrap_err();
        assert!(
            err.errors.iter().any(|i| i.path == "layers.base"),
            "{err:#?}"
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("no-such-layout"),
            "refused to write"
        );
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(leftovers.len(), 1, "no temp files left");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
