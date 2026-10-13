//! The system's fonts, mapped so that nothing reads whole font files into the page cache.
//!
//! A plain mapping reads ahead around each page fault, and with the usual 4 MB `read_ahead_kb`
//! that's most of a font file per fault. fontdb maps every system font that way to parse it, so
//! its `load_system_fonts` read ~214 MB (540 fonts) into the page cache, which the cgroup counts:
//! systemd showed the agent's MemoryPeak at ~250 MB with 20 MB of RSS. cosmic-text then maps the
//! fonts it draws with, or all of them when a glyph is in no font, the same way.
//!
//! Here each font file is mapped once, with `MADV_RANDOM` (no read-ahead), and the mapping stays
//! in the database as a `SharedFile`, which cosmic-text uses rather than mapping the file again.
//! So the scan reads only the tables it parses (~14 MB in all), and a missing-glyph search reads
//! each font's cmap rather than the whole file. [`release_pages`] drops the pages a mapping has
//! touched from our RSS (they stay in the shared page cache) after the scan and on each trim.
//!
//! The directories and generic families come from the fontconfig config files, like fontdb does
//! (its `load_fontconfig`, MIT), without the C library.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cosmic_text::fontdb::{Database, Source};

/// Loads the system fonts into `db`: the fontconfig dirs (or the usual dirs without a config),
/// each file kept mapped without read-ahead.
pub fn load_system_fonts(db: &mut Database) {
    let mut seen = HashSet::new();
    for dir in fontconfig_dirs(db).unwrap_or_else(default_dirs) {
        load_dir(db, &dir, &mut seen);
    }
    release_pages(db);
}

fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts/"),
        PathBuf::from("/usr/local/share/fonts/"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".fonts"));
        dirs.push(Path::new(&home).join(".local/share/fonts"));
    }
    dirs
}

/// The `<dir>`s of the fontconfig config, and its generic family aliases applied to `db`.
/// `None` without a config that lists dirs.
fn fontconfig_dirs(db: &mut Database) -> Option<Vec<PathBuf>> {
    let mut fc = fontconfig_parser::FontConfig::default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(file) = std::env::var_os("FONTCONFIG_FILE") {
        let _ = fc.merge_config(Path::new(&file));
    } else {
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        let read_global = match config_home {
            Some(p) => fc.merge_config(&p.join("fontconfig/fonts.conf")).is_err(),
            None => true,
        };
        if read_global {
            let _ = fc.merge_config(Path::new("/etc/fonts/local.conf"));
        }
        let _ = fc.merge_config(Path::new("/etc/fonts/fonts.conf"));
    }
    for a in &fc.aliases {
        let Some(name) = a.prefer.first().or(a.accept.first()).or(a.default.first()) else {
            continue;
        };
        match a.alias.to_lowercase().as_str() {
            "serif" => db.set_serif_family(name),
            "sans-serif" | "sans serif" => db.set_sans_serif_family(name),
            "monospace" => db.set_monospace_family(name),
            "cursive" => db.set_cursive_family(name),
            "fantasy" => db.set_fantasy_family(name),
            _ => {}
        }
    }
    if fc.dirs.is_empty() {
        return None;
    }
    let dirs = fc
        .dirs
        .into_iter()
        .filter_map(|d| match d.path.strip_prefix("~") {
            Ok(rest) => home.as_ref().map(|h| h.join(rest)),
            Err(_) => Some(d.path),
        });
    Some(dirs.collect())
}

/// Every font under `dir`, recursively, each real file once (symlinks are followed but a file
/// or dir reached twice is skipped).
fn load_dir(db: &mut Database, dir: &Path, seen: &mut HashSet<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(path) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            load_dir(db, &path, seen);
        } else if meta.is_file() && is_font(&path) {
            load_file(db, path);
        }
    }
}

fn is_font(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    ["ttf", "ttc", "otf", "otc"]
        .iter()
        .any(|e| ext.eq_ignore_ascii_case(e))
}

/// Maps one font file without read-ahead and parses its faces into `db`, keeping the mapping.
fn load_file(db: &mut Database, path: PathBuf) {
    let Ok(file) = std::fs::File::open(&path) else {
        return;
    };
    // SAFETY: a read-only mapping of a font file, as fontdb and cosmic-text make themselves.
    // Package updates replace font files (a new inode), which leaves this mapping valid; a file
    // truncated in place while mapped would fault, as it would for fontdb.
    let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else {
        return;
    };
    let _ = map.advise(memmap2::Advice::Random);
    db.load_font_source(Source::SharedFile(path, Arc::new(map)));
}

/// Drops the pages that the font mappings in `db` have touched from this process (they stay in
/// the page cache, and come back without I/O when touched again).
pub fn release_pages(db: &Database) {
    let mut done = HashSet::new();
    for face in db.faces() {
        let Source::SharedFile(_, data) = &face.source else {
            continue;
        };
        let bytes: &[u8] = (**data).as_ref();
        if bytes.is_empty() || !done.insert(bytes.as_ptr()) {
            continue;
        }
        // SAFETY: `bytes` is a whole read-only file mapping (page-aligned start, from mmap), so
        // MADV_DONTNEED only drops page table entries; reads fault the same bytes back in.
        unsafe {
            let _ = rustix::mm::madvise(
                bytes.as_ptr() as *mut _,
                bytes.len(),
                rustix::mm::Advice::LinuxDontNeed,
            );
        }
    }
}

/// How many font files cosmic-text has loaded: every font it loads holds a clone of the file's
/// mapping, on top of the database's one per face.
pub fn loaded_files(db: &Database) -> usize {
    let mut faces: std::collections::HashMap<*const u8, (usize, usize)> = Default::default();
    for face in db.faces() {
        if let Source::SharedFile(_, data) = &face.source {
            let bytes: &[u8] = (**data).as_ref();
            let e = faces
                .entry(bytes.as_ptr())
                .or_insert((0, Arc::strong_count(data)));
            e.0 += 1;
        }
    }
    faces.values().filter(|(n, strong)| strong > n).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::fontdb;

    /// The tests here map system fonts, and one measures this process's font pages.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Resident pages of this process's font file mappings, in kB.
    fn font_rss_kb() -> u64 {
        let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap_or_default();
        let (mut font, mut kb) = (false, 0);
        for l in smaps.lines() {
            let first = l.split_whitespace().next().unwrap_or("");
            if first.contains('-') && !first.ends_with(':') {
                font = l.contains("/fonts/");
            } else if font && let Some(v) = l.strip_prefix("Rss:") {
                kb += v
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(0);
            }
        }
        kb
    }

    #[test]
    fn fallback_searches_and_new_styles_leave_few_fonts_behind() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut fonts = crate::Fonts::system();
        let family = fonts.pick_family(&["sans-serif".into()]);
        // New weights (a match list each, reading every font's fvar), then a private-use
        // codepoint (a search through every font that may or may not find it).
        for (text, weight) in [
            ("Song 09:41", 400),
            ("Song 09:41", 700),
            ("x \u{10fffd}", 400),
        ] {
            crate::text::shape(&mut fonts, text, &family, weight, 20.0);
            let loaded = loaded_files(fonts.system.db());
            assert!(
                loaded <= crate::text::LOADED_FONTS_MAX,
                "{text}: {loaded} fonts loaded"
            );
            // The fonts in use: a few hundred kB (no system has 4 MB of them resident).
            let kb = font_rss_kb();
            assert!(
                kb < 4096,
                "{text} at {weight}: {kb} kB of font pages resident"
            );
        }
    }

    #[test]
    fn same_faces_as_fontdb() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let mut ours = Database::new();
        load_system_fonts(&mut ours);
        let mut theirs = Database::new();
        theirs.load_system_fonts();
        let key = |f: &fontdb::FaceInfo| {
            let path = match &f.source {
                Source::File(p) | Source::SharedFile(p, _) => p.clone(),
                Source::Binary(_) => PathBuf::new(),
            };
            (path, f.index, f.post_script_name.clone())
        };
        // fontdb loads a file again when a second fontconfig dir reaches it; this scan doesn't.
        let a: std::collections::BTreeSet<_> = ours.faces().map(key).collect();
        let b: std::collections::BTreeSet<_> = theirs.faces().map(key).collect();
        assert_eq!(a.len(), ours.len(), "a file was loaded twice");
        assert_eq!(
            a.difference(&b).collect::<Vec<_>>(),
            Vec::<&(PathBuf, u32, String)>::new()
        );
        assert_eq!(
            b.difference(&a).collect::<Vec<_>>(),
            Vec::<&(PathBuf, u32, String)>::new()
        );
        assert!(
            ours.faces()
                .all(|f| matches!(f.source, Source::SharedFile(..)))
        );
        for family in [
            fontdb::Family::Serif,
            fontdb::Family::SansSerif,
            fontdb::Family::Monospace,
        ] {
            assert_eq!(ours.family_name(&family), theirs.family_name(&family));
        }
    }
}
