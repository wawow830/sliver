//! Finding and reading config files, with `include` (customization.md §1).
//!
//! Precedence, lowest to highest: the built-in defaults, then the last include, ..., the first
//! include, then the including file itself. Tables deep-merge and arrays are replaced.
//! Loading never fails outright: problems are errors in the diagnostics, and a caller with a
//! live config must keep it when `diagnostics.ok()` is false.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::diag::Diagnostics;
use crate::model::{BUILTIN, Config};
use crate::resolve::deep_merge;

pub const MAX_INCLUDE_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    pub diagnostics: Diagnostics,
    /// Every file that went into this config, to watch for hot reload.
    pub files: Vec<PathBuf>,
    /// The merged raw table (built-ins + includes + file) that `config` was parsed from. Editors
    /// use it to copy effective values (e.g. a built-in layout's zones) into the user's file.
    pub merged: Table,
}

/// The config search path, first found wins: `$XDG_CONFIG_HOME/sliver`, `~/.config/sliver`,
/// `/etc/xdg/sliver`, `/usr/share/sliver`.
pub fn search_path() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        v.push(PathBuf::from(x).join("sliver/config.toml"));
    }
    if let Some(h) = std::env::var_os("HOME") {
        let p = PathBuf::from(h).join(".config/sliver/config.toml");
        if !v.contains(&p) {
            v.push(p);
        }
    }
    v.push("/etc/xdg/sliver/config.toml".into());
    v.push("/usr/share/sliver/config.toml".into());
    v
}

pub fn find_config() -> Option<PathBuf> {
    search_path().into_iter().find(|p| p.is_file())
}

/// Loads the first config on the search path, or just the built-ins if there is none.
pub fn load() -> Loaded {
    match find_config() {
        Some(p) => load_file(&p),
        None => finish(Table::new(), Diagnostics::default(), Vec::new()),
    }
}

pub fn load_file(path: &Path) -> Loaded {
    let mut d = Diagnostics::default();
    let mut files = Vec::new();
    let tree = read_tree(path, &mut Vec::new(), &mut files, &mut d);
    finish(tree, d, files)
}

/// Loads config text that isn't a file yet (settings preview, tests). `base_dir` resolves includes.
pub fn load_str(text: &str, base_dir: &Path) -> Loaded {
    let mut d = Diagnostics::default();
    let mut files = Vec::new();
    let tree = match text.parse::<Table>() {
        Ok(t) => merge_includes(t, base_dir, "<config>", &mut vec![], &mut files, &mut d),
        Err(e) => {
            d.error("", format!("TOML syntax: {e}"));
            Table::new()
        }
    };
    finish(tree, d, files)
}

fn finish(tree: Table, mut d: Diagnostics, files: Vec<PathBuf>) -> Loaded {
    let mut merged: Table = BUILTIN.parse().expect("builtin.toml parses (tested)");
    deep_merge(&mut merged, &tree);
    let (config, cd) = Config::from_table(&merged);
    d.extend(cd);
    Loaded {
        config,
        diagnostics: d,
        files,
        merged,
    }
}

fn read_tree(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    files: &mut Vec<PathBuf>,
    d: &mut Diagnostics,
) -> Table {
    let shown = path.display().to_string();
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&canonical) {
        d.error(&shown, "include cycle");
        return Table::new();
    }
    if stack.len() >= MAX_INCLUDE_DEPTH {
        d.error(
            &shown,
            format!("includes nest deeper than {MAX_INCLUDE_DEPTH}"),
        );
        return Table::new();
    }
    files.push(path.to_path_buf());
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            d.error(&shown, format!("can't read: {e}"));
            return Table::new();
        }
    };
    let table = match text.parse::<Table>() {
        Ok(t) => t,
        Err(e) => {
            d.error(&shown, format!("TOML syntax: {e}"));
            return Table::new();
        }
    };
    stack.push(canonical);
    let dir = path.parent().unwrap_or(Path::new("."));
    let t = merge_includes(table, dir, &shown, stack, files, d);
    stack.pop();
    t
}

fn merge_includes(
    mut own: Table,
    dir: &Path,
    shown: &str,
    stack: &mut Vec<PathBuf>,
    files: &mut Vec<PathBuf>,
    d: &mut Diagnostics,
) -> Table {
    let includes: Vec<String> = match own.remove("include") {
        None => Vec::new(),
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(a)) => a
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(_) => {
            d.error(
                format!("{shown}: include"),
                "include is a path or an array of paths",
            );
            Vec::new()
        }
    };
    let mut merged = Table::new();
    // Earlier includes win, so merge from the last one to the first, then the file itself.
    for inc in includes.iter().rev() {
        let paths = expand(inc, dir);
        if paths.is_empty() {
            d.warn(
                format!("{shown}: include"),
                format!("{inc:?} matched no files"),
            );
        }
        for p in paths.iter().rev() {
            let t = read_tree(p, stack, files, d);
            deep_merge(&mut merged, &t);
        }
    }
    deep_merge(&mut merged, &own);
    merged
}

/// `~`, relative paths, and `*`/`?` wildcards in the file name (sorted, like a shell).
fn expand(pattern: &str, dir: &Path) -> Vec<PathBuf> {
    let p = match pattern.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| rest.into()),
        None => dir.join(pattern),
    };
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.contains(['*', '?']) {
        return if p.exists() { vec![p] } else { Vec::new() };
    }
    let parent = p.parent().unwrap_or(Path::new("."));
    let mut out: Vec<PathBuf> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| wildcard(&name, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

fn wildcard(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    go(&p, &s)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            // Under target/, not /tmp: /tmp is a small shared tmpfs on this machine.
            let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-tmp")
                .join(format!("sliver-config-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn write(&self, name: &str, body: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn include_precedence() {
        let dir = TempDir::new("prec");
        dir.write(
            "a.toml",
            "[modules.clock]\nformat = \"A\"\nlocale = \"en_AU\"\n",
        );
        dir.write("b.toml", "[modules.clock]\nformat = \"B\"\ninterval = 30\n");
        dir.write("mods/x.toml", "[modules.\"custom/x\"]\nformat = \"x\"\n");
        let main = dir.write(
            "config.toml",
            "include = [\"a.toml\", \"b.toml\", \"mods/*.toml\", \"missing.toml\"]\n[bar]\nspacing = 4\n[modules.clock]\nmax-length = 9\n",
        );
        let l = load_file(&main);
        assert!(l.diagnostics.ok(), "{:#?}", l.diagnostics);
        let clock = &l.config.modules[&crate::ModuleId::parse("clock").unwrap()];
        assert_eq!(clock["format"].as_str(), Some("A"), "earlier include wins");
        assert_eq!(
            clock["interval"].as_integer(),
            Some(30),
            "tables deep-merge"
        );
        assert_eq!(
            clock["max-length"].as_integer(),
            Some(9),
            "including file merged"
        );
        assert!(
            l.config
                .modules
                .contains_key(&crate::ModuleId::parse("custom/x").unwrap()),
            "glob"
        );
        assert_eq!(l.config.bar.spacing, 4);
        assert_eq!(l.files.len(), 4);
        assert!(
            l.diagnostics
                .warnings
                .iter()
                .any(|w| w.message.contains("missing.toml"))
        );
        assert!(
            l.config.layouts.contains_key("fkeys"),
            "built-ins underneath"
        );
    }

    #[test]
    fn cycles_and_syntax_errors() {
        let dir = TempDir::new("cyc");
        dir.write("a.toml", "include = \"b.toml\"\n");
        dir.write("b.toml", "include = \"a.toml\"\n");
        let l = load_file(&dir.0.join("a.toml"));
        assert!(
            l.diagnostics
                .errors
                .iter()
                .any(|e| e.message == "include cycle"),
            "{:#?}",
            l.diagnostics
        );

        let bad = dir.write("bad.toml", "[bar\nspacing = 1\n");
        let l = load_file(&bad);
        assert!(!l.diagnostics.ok());
        assert!(l.diagnostics.errors[0].message.starts_with("TOML syntax"));
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*.toml", "a.toml"));
        assert!(wildcard("m?d.toml", "mod.toml"));
        assert!(!wildcard("*.toml", "a.json"));
    }
}
