//! `sliver-waybar`: turn a waybar config into a sliver `config.toml` (customization.md G8).
//!
//! Sliver reads waybar's module names, gesture keys (`on-click`, `on-scroll-up`, ...), `format`
//! strings, `custom/*` blocks and `tooltip` keys as they are, so most of the job is carrying
//! module blocks over verbatim (JSON to TOML) and building one layout from `modules-left`,
//! `modules-center` and `modules-right`. What cannot apply (bar geometry, mouse buttons, modules
//! with no equivalent) is reported, not silently dropped. The result is validated with
//! `sliver_config`, whose own warnings (unknown keys, unsupported gestures) join the report.
//!
//! The imported layout is named `waybar`, becomes the `base` layer, and keeps tiny-dfr's `esc`
//! zone, so the user never loses Escape. The `fn` layer stays the built-in F-keys.

mod jsonc;

use serde_json::{Map, Value as Json};
use sliver_config::{MODULE_TYPES, ModuleId};
use std::path::{Path, PathBuf};
use toml::{Table, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Worth knowing, nothing lost.
    Info,
    /// Something was dropped or will behave differently.
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub level: Level,
    /// Where in the waybar config (or the generated TOML for validation notes).
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Set `stylesheet = "..."` (a waybar `style.css`; sliver accepts the common GTK-isms).
    pub stylesheet: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Imported {
    /// The sliver config, ready to write to `~/.config/sliver/config.toml`.
    pub toml: String,
    pub notes: Vec<Note>,
    /// Module references that made it into the layout.
    pub modules: Vec<String>,
    /// References that were left out.
    pub dropped: Vec<String>,
}

/// Bar-level keys that are not module blocks, and why they don't apply (`None` = handled).
fn bar_option(key: &str) -> Option<Option<&'static str>> {
    Some(match key {
        "modules-left" | "modules-center" | "modules-right" | "name" | "spacing" | "include" => {
            None
        }
        "layer" | "output" | "position" | "height" | "width" | "exclusive" | "passthrough"
        | "gtk-layer-shell" | "mode" | "ipc" | "id" | "modifier-reset" | "fixed-center" => {
            Some("not applicable: sliver draws one fixed 2008x60 panel, not a Wayland surface")
        }
        k if k.starts_with("margin") => {
            Some("not applicable: use [bar] margin / spacing, or layout zones, instead")
        }
        "start_hidden" | "on-sigusr1" | "on-sigusr2" => {
            Some("not applicable: use `sliverctl bar hide|toggle`, or SIGUSR1 to toggle")
        }
        "reload_style_on_change" => {
            Some("not needed: sliver always hot-reloads its config and stylesheet")
        }
        _ => return None,
    })
}

fn why_unsupported(reference: &str) -> &'static str {
    let base = reference.split('#').next().unwrap_or(reference);
    if base == "cava" {
        "dropped: a visualizer polls constantly (power invariant); maybe an opt-in later"
    } else if base.starts_with("cffi/") {
        "dropped: in-process plugins could take the bar down; use a custom/* exec module"
    } else if base == "mpd" {
        "dropped: no mpd module; the `mpris` module covers most players (mpd needs mpd-mpris)"
    } else {
        "dropped: sliver has no module with this name yet"
    }
}

fn to_toml(v: &Json, path: &str, notes: &mut Vec<Note>) -> Option<Value> {
    Some(match v {
        Json::Null => {
            notes.push(Note {
                level: Level::Info,
                path: path.into(),
                message: "null value dropped (TOML has no null)".into(),
            });
            return None;
        }
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Float(n.as_f64()?),
        },
        Json::String(s) => Value::String(s.clone()),
        Json::Array(a) => Value::Array(
            a.iter()
                .enumerate()
                .filter_map(|(i, x)| to_toml(x, &format!("{path}[{i}]"), notes))
                .collect(),
        ),
        Json::Object(o) => {
            let mut t = Table::new();
            for (k, x) in o {
                if let Some(x) = to_toml(x, &format!("{path}.{k}"), notes) {
                    t.insert(k.clone(), x);
                }
            }
            Value::Table(t)
        }
    })
}

/// Is `reference` something sliver can place on the bar? Returns the canonical id if so.
fn resolve_ref(reference: &str) -> Option<ModuleId> {
    let id = ModuleId::parse(reference).ok()?;
    (id.kind == "custom" || MODULE_TYPES.contains(&id.kind.as_str())).then_some(id)
}

/// Keep the supported references of a module list; report and drop the rest.
fn filter_refs(
    list: &Json,
    path: &str,
    notes: &mut Vec<Note>,
    dropped: &mut Vec<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (i, item) in list.as_array().into_iter().flatten().enumerate() {
        let Some(r) = item.as_str() else {
            notes.push(Note {
                level: Level::Warn,
                path: format!("{path}[{i}]"),
                message: "module references must be strings; skipped".into(),
            });
            continue;
        };
        if resolve_ref(r).is_some() {
            out.push(r.to_string());
        } else {
            dropped.push(r.to_string());
            notes.push(Note {
                level: Level::Warn,
                path: format!("{path}[{i}]"),
                message: format!("{r}: {}", why_unsupported(r)),
            });
        }
    }
    out
}

/// Merge bar objects: keys already in `main` win over the included ones.
fn merge_missing(main: &mut Map<String, Json>, inc: Map<String, Json>) {
    for (k, v) in inc {
        if k != "include" {
            main.entry(k).or_insert(v);
        }
    }
}

fn first_bar(v: Json, notes: &mut Vec<Note>) -> Result<Map<String, Json>, String> {
    match v {
        Json::Object(o) => Ok(o),
        Json::Array(bars) => {
            let objs: Vec<Map<String, Json>> = bars
                .into_iter()
                .filter_map(|b| {
                    if let Json::Object(o) = b {
                        Some(o)
                    } else {
                        None
                    }
                })
                .collect();
            if objs.len() > 1 {
                notes.push(Note {
                    level: Level::Warn,
                    path: String::new(),
                    message: format!(
                        "{} bars found; only the first is imported (sliver has one bar: use layouts, rules and layers for the rest)",
                        objs.len()
                    ),
                });
            }
            objs.into_iter()
                .next()
                .ok_or_else(|| "the config has no bar object".to_string())
        }
        _ => Err("expected a JSON object (or an array of bar objects)".into()),
    }
}

/// Import from text. `include`d files are not followed; use [`import_path`] for that.
pub fn import_str(src: &str, opts: &Options) -> Result<Imported, String> {
    let mut notes = Vec::new();
    let bar = first_bar(jsonc::parse(src)?, &mut notes)?;
    Ok(build(bar, opts, notes))
}

fn expand(p: &str, dir: &Path) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return Path::new(&h).join(rest);
        }
    }
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    }
}

fn read_bar(path: &Path, depth: u8, notes: &mut Vec<Note>) -> Result<Map<String, Json>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bar = first_bar(
        jsonc::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        notes,
    )?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let includes: Vec<String> = bar
        .get("include")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    for inc in includes {
        if inc.contains(['*', '?']) {
            notes.push(Note {
                level: Level::Warn,
                path: "include".into(),
                message: format!("{inc}: globs are not followed; list the files"),
            });
            continue;
        }
        if depth >= 4 {
            notes.push(Note {
                level: Level::Warn,
                path: "include".into(),
                message: format!("{inc}: includes nested deeper than 4 are ignored"),
            });
            continue;
        }
        let target = expand(&inc, dir);
        match read_bar(&target, depth + 1, notes) {
            Ok(other) => merge_missing(&mut bar, other),
            Err(e) => notes.push(Note {
                level: Level::Warn,
                path: "include".into(),
                message: e,
            }),
        }
    }
    Ok(bar)
}

/// Import a waybar config file, following its `include` entries (the including file wins).
pub fn import_path(path: &Path, opts: &Options) -> Result<Imported, String> {
    let mut notes = Vec::new();
    let bar = read_bar(path, 0, &mut notes)?;
    Ok(build(bar, opts, notes))
}

fn build(bar: Map<String, Json>, opts: &Options, mut notes: Vec<Note>) -> Imported {
    let mut dropped = Vec::new();
    let mut zones = vec![zone_esc()];
    let mut placed = Vec::new();
    for (side, align) in [("left", "start"), ("center", "center"), ("right", "end")] {
        let key = format!("modules-{side}");
        let Some(list) = bar.get(&key) else { continue };
        let refs = filter_refs(list, &key, &mut notes, &mut dropped);
        if refs.is_empty() {
            continue;
        }
        placed.extend(refs.iter().cloned());
        let mut z = Table::new();
        z.insert("name".into(), side.into());
        z.insert("width".into(), "fill".into());
        z.insert("align".into(), align.into());
        z.insert(
            "modules".into(),
            Value::Array(refs.into_iter().map(Value::String).collect()),
        );
        zones.push(Value::Table(z));
    }

    // Module blocks: every object-valued key that isn't a bar option.
    let mut modules = Table::new();
    for (k, v) in &bar {
        match bar_option(k) {
            Some(None) => continue,
            Some(Some(why)) => {
                notes.push(Note {
                    level: Level::Info,
                    path: k.clone(),
                    message: why.into(),
                });
                continue;
            }
            None => {}
        }
        let Json::Object(block) = v else {
            notes.push(Note {
                level: Level::Warn,
                path: k.clone(),
                message: "unknown bar option; ignored".into(),
            });
            continue;
        };
        if resolve_ref(k).is_none() {
            dropped.push(k.clone());
            notes.push(Note {
                level: Level::Warn,
                path: k.clone(),
                message: why_unsupported(k).to_string(),
            });
            continue;
        }
        let mut t = Table::new();
        for (bk, bv) in block {
            if bk == "modules" && k.starts_with("group/") {
                let kids = filter_refs(bv, &format!("{k}.modules"), &mut notes, &mut dropped);
                t.insert(
                    bk.clone(),
                    Value::Array(kids.into_iter().map(Value::String).collect()),
                );
            } else if let Some(x) = to_toml(bv, &format!("{k}.{bk}"), &mut notes) {
                t.insert(bk.clone(), x);
            }
        }
        modules.insert(k.clone(), Value::Table(t));
    }

    let mut root = Table::new();
    root.insert("version".into(), Value::Integer(1));
    if let Some(s) = &opts.stylesheet {
        root.insert("stylesheet".into(), Value::String(s.clone()));
    }
    let mut b = Table::new();
    if let Some(n) = bar.get("name").and_then(Json::as_str) {
        b.insert("name".into(), n.into());
    }
    if let Some(sp) = bar.get("spacing").and_then(Json::as_i64) {
        b.insert("spacing".into(), Value::Integer(sp));
    }
    if !b.is_empty() {
        root.insert("bar".into(), Value::Table(b));
    }
    let mut layers = Table::new();
    layers.insert("base".into(), "waybar".into());
    root.insert("layers".into(), Value::Table(layers));
    let mut layout = Table::new();
    layout.insert("zones".into(), Value::Array(zones));
    let mut layouts = Table::new();
    layouts.insert("waybar".into(), Value::Table(layout));
    root.insert("layouts".into(), Value::Table(layouts));
    if !modules.is_empty() {
        root.insert("modules".into(), Value::Table(modules));
    }

    let text = format!(
        "# Imported from a waybar config by sliver-waybar.\n# Review the notes printed by the importer: some waybar features do not apply to a touch bar.\n{}",
        toml::to_string(&root).expect("a table of plain values serializes")
    );

    // Let the real config reader judge the result.
    let loaded = sliver_config::load_str(&text, Path::new("."));
    let mut diag = loaded.diagnostics.clone();
    diag.extend(loaded.config.validate(sliver_config::Hardware {
        physical_esc: false,
    }));
    for e in &diag.errors {
        notes.push(Note {
            level: Level::Warn,
            path: e.path.clone(),
            message: format!("error in the generated config: {}", e.message),
        });
    }
    for w in &diag.warnings {
        notes.push(Note {
            level: Level::Warn,
            path: w.path.clone(),
            message: w.message.clone(),
        });
    }
    let mut seen = std::collections::HashSet::new();
    notes.retain(|n| seen.insert((n.path.clone(), n.message.clone())));
    dropped.sort();
    dropped.dedup();
    Imported {
        toml: text,
        notes,
        modules: placed,
        dropped,
    }
}

fn zone_esc() -> Value {
    let mut z = Table::new();
    z.insert("name".into(), "esc".into());
    z.insert("modules".into(), Value::Array(vec!["key/Esc#auto".into()]));
    Value::Table(z)
}

#[cfg(test)]
mod tests;
