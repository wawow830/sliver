//! Diagnostics collected while reading a config, plus a small key-tracking table reader.
//!
//! Errors mean "don't apply this config". Warnings never block (customization.md §1).

use std::collections::BTreeSet;
use std::fmt;

use toml::{Table, Value};

/// One problem found in a config, located by a dotted key path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

/// Errors block applying the config. Warnings are shown but never block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

impl Diagnostics {
    pub fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(Issue {
            path: path.into(),
            message: message.into(),
        });
    }

    pub fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.warnings.push(Issue {
            path: path.into(),
            message: message.into(),
        });
    }

    /// True when the config may be applied.
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn extend(&mut self, other: Diagnostics) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }
}

/// Joins a key path the way users write it: `modules."custom/vpn".interval`.
pub(crate) fn join(path: &str, key: &str) -> String {
    let needs_quotes = !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let key = if needs_quotes {
        format!("\"{key}\"")
    } else {
        key.to_owned()
    };
    if path.is_empty() {
        key
    } else {
        format!("{path}.{key}")
    }
}

/// Reads typed values out of a table, remembers which keys were consumed, and reports the rest
/// as unknown-key warnings.
pub(crate) struct Reader<'a> {
    table: &'a Table,
    path: String,
    seen: BTreeSet<&'a str>,
}

impl<'a> Reader<'a> {
    pub fn new(table: &'a Table, path: impl Into<String>) -> Self {
        Self {
            table,
            path: path.into(),
            seen: BTreeSet::new(),
        }
    }

    pub fn path(&self, key: &str) -> String {
        join(&self.path, key)
    }

    /// Marks `key` as known and returns its value.
    pub fn get(&mut self, key: &'a str) -> Option<&'a Value> {
        let (k, v) = self.table.get_key_value(key)?;
        self.seen.insert(k.as_str());
        Some(v)
    }

    /// Marks a key as known without reading it (handled elsewhere).
    pub fn skip(&mut self, key: &'a str) {
        if let Some((k, _)) = self.table.get_key_value(key) {
            self.seen.insert(k.as_str());
        }
    }

    pub fn string(&mut self, key: &'a str, d: &mut Diagnostics) -> Option<String> {
        match self.get(key)? {
            Value::String(s) => Some(s.clone()),
            other => {
                d.error(
                    self.path(key),
                    format!("expected a string, got {}", other.type_str()),
                );
                None
            }
        }
    }

    pub fn bool(&mut self, key: &'a str, d: &mut Diagnostics) -> Option<bool> {
        match self.get(key)? {
            Value::Boolean(b) => Some(*b),
            other => {
                d.error(
                    self.path(key),
                    format!("expected true/false, got {}", other.type_str()),
                );
                None
            }
        }
    }

    pub fn uint(&mut self, key: &'a str, max: u64, d: &mut Diagnostics) -> Option<u64> {
        match self.get(key)? {
            Value::Integer(i) if *i >= 0 && (*i as u64) <= max => Some(*i as u64),
            Value::Integer(i) => {
                d.error(self.path(key), format!("{i} is out of range 0..={max}"));
                None
            }
            other => {
                d.error(
                    self.path(key),
                    format!("expected a number, got {}", other.type_str()),
                );
                None
            }
        }
    }

    pub fn number(&mut self, key: &'a str, d: &mut Diagnostics) -> Option<f64> {
        match self.get(key)? {
            Value::Integer(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            other => {
                d.error(
                    self.path(key),
                    format!("expected a number, got {}", other.type_str()),
                );
                None
            }
        }
    }

    /// A string or an array of strings (e.g. `class`).
    pub fn strings(&mut self, key: &'a str, d: &mut Diagnostics) -> Vec<String> {
        match self.get(key) {
            None => Vec::new(),
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    other => {
                        d.error(
                            self.path(key),
                            format!("expected strings, got {}", other.type_str()),
                        );
                        None
                    }
                })
                .collect(),
            Some(other) => {
                d.error(
                    self.path(key),
                    format!("expected a string or array, got {}", other.type_str()),
                );
                Vec::new()
            }
        }
    }

    pub fn table(&mut self, key: &'a str, d: &mut Diagnostics) -> Option<&'a Table> {
        match self.get(key)? {
            Value::Table(t) => Some(t),
            other => {
                d.error(
                    self.path(key),
                    format!("expected a table, got {}", other.type_str()),
                );
                None
            }
        }
    }

    /// Reports every key that wasn't read as an unknown-key warning.
    pub fn finish(self, d: &mut Diagnostics) {
        for k in self.table.keys() {
            if !self.seen.contains(k.as_str()) {
                d.warn(join(&self.path, k), "unknown key (ignored)");
            }
        }
    }
}
