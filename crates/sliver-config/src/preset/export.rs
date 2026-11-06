//! Collecting a setup into a bundle: the config, what it includes and points at, with files from
//! outside the config directory moved in and the references rewritten.

use super::*;

/// Where a referenced file goes in the bundle.
pub(super) struct Placer {
    root: PathBuf,
    home: Option<PathBuf>,
    /// canonical source → bundle name
    placed: BTreeMap<PathBuf, String>,
    /// bundle name → canonical source, in placement order
    sources: Vec<(String, PathBuf)>,
}

impl Placer {
    fn resolve(&self, s: &str, dir: &Path) -> Option<PathBuf> {
        let p = match (s.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => dir.join(s),
        };
        p.canonicalize().ok().filter(|p| p.is_file())
    }

    fn is_system(&self, p: &Path) -> bool {
        let s = p.to_string_lossy();
        !p.starts_with(&self.root) && SYSTEM_ROOTS.iter().any(|r| s.starts_with(r))
    }

    /// The bundle name for `p`, placing it on first sight (`None` = a system file, not copied).
    fn place(&mut self, p: &Path, kind: &str) -> Option<String> {
        if let Some(n) = self.placed.get(p) {
            return Some(n.clone());
        }
        if self.is_system(p) {
            return None;
        }
        let name = match p.strip_prefix(&self.root) {
            Ok(rel) if check_name(&rel.to_string_lossy()).is_ok() => {
                rel.to_string_lossy().into_owned()
            }
            _ => {
                let base = p.file_name()?.to_string_lossy().into_owned();
                let (stem, ext) = match base.rsplit_once('.') {
                    Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
                    _ => (base.clone(), String::new()),
                };
                let mut n = format!("{kind}/{base}");
                let mut i = 2;
                while self.sources.iter().any(|(x, _)| *x == n) {
                    n = format!("{kind}/{stem}-{i}{ext}");
                    i += 1;
                }
                n
            }
        };
        self.placed.insert(p.to_path_buf(), name.clone());
        self.sources.push((name.clone(), p.to_path_buf()));
        Some(name)
    }
}

/// `target` (a bundle name) as seen from the directory of `from` (a bundle name).
pub(super) fn relative(from: &str, target: &str) -> String {
    let up = from.matches('/').count();
    format!("{}{target}", "../".repeat(up))
}

/// Bundles the config at `config` (its directory is the bundle root). Refuses a config that
/// doesn't load.
pub fn export(config: &Path, name: Option<&str>) -> Result<Export, String> {
    let loaded = load_file(config);
    if !loaded.diagnostics.ok() {
        let errors: Vec<String> = loaded
            .diagnostics
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect();
        return Err(format!(
            "fix the config before exporting it:\n  {}",
            errors.join("\n  ")
        ));
    }
    let config = config
        .canonicalize()
        .map_err(|e| format!("{}: {e}", config.display()))?;
    let root = config.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut placer = Placer {
        root: root.clone(),
        home: std::env::var_os("HOME").map(PathBuf::from),
        placed: BTreeMap::new(),
        sources: Vec::new(),
    };
    placer.placed.insert(config.clone(), CONFIG.into());
    placer.sources.push((CONFIG.into(), config.clone()));
    let mut notes = Vec::new();
    let mut files = BTreeMap::new();
    let mut total = 0;
    // `stylesheet` absent = `style.css` next to the config.
    if loaded.config.stylesheet.is_none()
        && let Some(css) = placer.resolve("style.css", &root)
    {
        placer.place(&css, "style");
    }
    let mut i = 0;
    while i < placer.sources.len() {
        let (name, src) = placer.sources[i].clone();
        i += 1;
        let data = std::fs::read(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        total += data.len();
        if data.len() > MAX_FILE || total > MAX_TOTAL || placer.sources.len() > MAX_FILES {
            return Err(format!(
                "{}: the preset would be too large (limits: {} MiB per file, {} MiB in all, \
                 {MAX_FILES} files)",
                src.display(),
                MAX_FILE >> 20,
                MAX_TOTAL >> 20
            ));
        }
        let dir = src.parent().unwrap_or(Path::new("/")).to_path_buf();
        let data = if name.ends_with(".toml") {
            export_toml(&data, &name, &dir, &mut placer, &mut notes)?
        } else if name.ends_with(".css") {
            scan_css(&data, &name, &dir, &mut placer, &mut notes);
            data
        } else {
            data
        };
        files.insert(name, data);
    }
    let bundle = Bundle {
        name: name.map(str::to_owned),
        files,
    };
    if let Some(h) = placer
        .home
        .as_ref()
        .map(|h| h.to_string_lossy().into_owned())
    {
        for r in bundle.runs() {
            if r.what.contains(&h) || r.what.contains("~/") {
                notes.push(format!(
                    "{} refers to your home directory, which may not exist elsewhere: {}",
                    r.at, r.what
                ));
            }
        }
    }
    Ok(Export { bundle, notes })
}

/// Rewrites a TOML file's file references to their bundle names and drops `env` tables.
pub(super) fn export_toml(
    data: &[u8],
    name: &str,
    dir: &Path,
    placer: &mut Placer,
    notes: &mut Vec<String>,
) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(data).map_err(|_| format!("{name}: not UTF-8"))?;
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{name}: {e}"))?;
    let mut ctx = Rewrite {
        name,
        dir,
        placer,
        notes,
    };
    ctx.table(doc.as_table_mut(), "");
    Ok(doc.to_string().into_bytes())
}

pub(super) struct Rewrite<'a> {
    name: &'a str,
    dir: &'a Path,
    placer: &'a mut Placer,
    notes: &'a mut Vec<String>,
}

impl Rewrite<'_> {
    fn table(&mut self, t: &mut dyn toml_edit::TableLike, path: &str) {
        if t.contains_key("env") && !path.is_empty() {
            t.remove("env");
            self.notes.push(format!(
                "{}: {}: env left out (it may hold secrets)",
                self.name,
                join(path, "env")
            ));
        }
        for (k, item) in t.iter_mut() {
            let p = join(path, &k);
            let is_ref = PATH_KEYS.contains(&k.get());
            self.item(item, &p, is_ref);
        }
    }

    fn item(&mut self, i: &mut toml_edit::Item, path: &str, is_ref: bool) {
        match i {
            toml_edit::Item::Table(t) => self.table(t, path),
            toml_edit::Item::ArrayOfTables(a) => {
                for (n, t) in a.iter_mut().enumerate() {
                    self.table(t, &format!("{path}[{n}]"));
                }
            }
            toml_edit::Item::Value(v) => self.value(v, path, is_ref),
            toml_edit::Item::None => {}
        }
    }

    fn value(&mut self, v: &mut toml_edit::Value, path: &str, is_ref: bool) {
        match v {
            toml_edit::Value::InlineTable(t) => self.table(t, path),
            toml_edit::Value::Array(a) => {
                for (n, x) in a.iter_mut().enumerate() {
                    self.value(x, &format!("{path}[{n}]"), is_ref);
                }
            }
            toml_edit::Value::String(s) if is_ref => {
                let orig = s.value().clone();
                let Some(src) = self.placer.resolve(&orig, self.dir) else {
                    return;
                };
                let kind = if path.ends_with("include") || path.contains("include[") {
                    "include"
                } else {
                    "assets"
                };
                match self.placer.place(&src, kind) {
                    Some(target) => {
                        let new = relative(self.name, &target);
                        // A relative reference that still points at the same file from the
                        // referrer's place in the bundle stays as written.
                        let still_valid = Path::new(&orig).is_relative()
                            && !orig.starts_with("~/")
                            && self.dir.join(&new).canonicalize().ok().as_ref() == Some(&src);
                        if !still_valid {
                            let decor = s.decor().clone();
                            let mut nv = toml_edit::Value::from(new);
                            *nv.decor_mut() = decor;
                            *v = nv;
                        }
                    }
                    None => self.notes.push(format!(
                        "{}: {path}: {} is a system file; it's referenced, not copied",
                        self.name,
                        src.display()
                    )),
                }
            }
            _ => {}
        }
    }
}

/// Bundles a stylesheet's `@import`s and `url()`s that live under the config directory. Others
/// are noted, not rewritten (CSS paths stay as written).
pub(super) fn scan_css(
    data: &[u8],
    name: &str,
    dir: &Path,
    placer: &mut Placer,
    notes: &mut Vec<String>,
) {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut refs = Vec::new();
    for (i, _) in text.match_indices("@import") {
        let rest = text[i + 7..].trim_start();
        let rest = rest.strip_prefix("url(").unwrap_or(rest);
        if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'')
            && let Some(end) = rest[1..].find(q)
        {
            refs.push(rest[1..1 + end].to_owned());
        }
    }
    for (i, _) in text.match_indices("url(") {
        let rest = text[i + 4..].trim_start();
        let (q, rest) = match rest.chars().next() {
            Some(c @ ('"' | '\'')) => (c, &rest[1..]),
            _ => (')', rest),
        };
        if let Some(end) = rest.find(q) {
            refs.push(rest[..end].trim().to_owned());
        }
    }
    refs.sort();
    refs.dedup();
    for r in refs {
        if r.starts_with("sliver:") || r.contains("://") {
            continue;
        }
        let Some(src) = placer.resolve(&r, dir) else {
            continue;
        };
        if src.starts_with(&placer.root) {
            placer.place(&src, "assets");
        } else if !placer.is_system(&src) {
            notes.push(format!(
                "{name}: {r} is outside the config directory; it isn't included"
            ));
        }
    }
}

// ---- base64 (RFC 4648, padded) ----

pub(super) const B64: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(super) fn base64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= c.len() {
                s.push(B64[(n >> shift & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub(super) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let clean: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if clean.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() / 4 * 3);
    for c in clean.chunks(4) {
        let mut n = 0u32;
        let mut pad = 0;
        for &b in c {
            let v = match b {
                b'=' => {
                    pad += 1;
                    0
                }
                _ if pad > 0 => return None,
                _ => B64.iter().position(|x| *x == b)? as u32,
            };
            n = n << 6 | v;
        }
        if pad > 2 {
            return None;
        }
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    Some(out)
}
