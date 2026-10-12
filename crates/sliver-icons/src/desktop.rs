//! `.desktop` entries: from a window's `app_id` / class to a name and an icon.

use crate::ini::Ini;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// Desktop file id: the path under `applications/` with `/` replaced by `-`, no `.desktop`.
    pub id: String,
    pub name: String,
    /// `Icon=`: a theme name or an absolute path; feed it to [`crate::IconLoader`].
    pub icon: Option<String>,
    pub exec: Option<String>,
    pub startup_wm_class: Option<String>,
    pub no_display: bool,
    pub path: PathBuf,
}

impl Entry {
    /// First word of `Exec=`, as a bare program name.
    pub fn exec_program(&self) -> Option<&str> {
        let word = self
            .exec
            .as_deref()?
            .split_whitespace()
            .next()?
            .trim_matches('"');
        Path::new(word).file_name()?.to_str()
    }
}

#[derive(Debug, Default)]
pub struct DesktopEntries {
    entries: Vec<Entry>,
}

fn parse_entry(id: String, path: &Path, text: &str) -> Option<Entry> {
    let ini = Ini::parse(text);
    let sec = "Desktop Entry";
    if ini.get(sec, "Type").is_some_and(|t| t != "Application") {
        return None;
    }
    if ini.get(sec, "Hidden") == Some("true") {
        return None;
    }
    let get = |k: &str| {
        ini.get(sec, k)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Some(Entry {
        name: get("Name").unwrap_or_else(|| id.clone()),
        icon: get("Icon"),
        exec: get("Exec"),
        startup_wm_class: get("StartupWMClass"),
        no_display: ini.get(sec, "NoDisplay") == Some("true"),
        id,
        path: path.to_path_buf(),
    })
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Entry>, depth: u8) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = rd.filter_map(Result::ok).collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let p = e.path();
        if p.is_dir() {
            if depth < 4 {
                walk(root, &p, out, depth + 1);
            }
        } else if p.extension().is_some_and(|x| x == "desktop") {
            let rel = p.strip_prefix(root).unwrap_or(&p).with_extension("");
            let id = rel.to_string_lossy().replace('/', "-");
            if let Some(text) = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| parse_entry(id, &p, &t))
            {
                out.push(text);
            }
        }
    }
}

impl DesktopEntries {
    /// Scan `applications` directories in priority order; the first definition of an id wins.
    pub fn scan(dirs: &[PathBuf]) -> Self {
        let mut all = Vec::new();
        for d in dirs {
            walk(d, d, &mut all, 0);
        }
        let mut seen = std::collections::HashSet::new();
        all.retain(|e| seen.insert(e.id.clone()));
        Self { entries: all }
    }

    pub fn from_paths(paths: &crate::SearchPaths) -> Self {
        Self::scan(&paths.application_dirs())
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        let id = id.strip_suffix(".desktop").unwrap_or(id);
        self.entries.iter().find(|e| e.id == id)
    }

    /// Best entry for a window's `app_id` (Wayland) or `WM_CLASS` class (X11), by decreasing
    /// confidence: exact id, id ignoring case, `StartupWMClass`, last segment of a
    /// reverse-DNS id (`org.mozilla.firefox` for `firefox`), the `Exec` program name.
    pub fn find_by_app_id(&self, app_id: &str) -> Option<&Entry> {
        let app = app_id.strip_suffix(".desktop").unwrap_or(app_id);
        if app.is_empty() {
            return None;
        }
        let eq = |a: &str| a.eq_ignore_ascii_case(app);
        let passes: [&dyn Fn(&Entry) -> bool; 5] = [
            &|e| e.id == app,
            &|e| eq(&e.id),
            &|e| e.startup_wm_class.as_deref().is_some_and(eq),
            &|e| {
                e.id.rsplit('.')
                    .next()
                    .is_some_and(|last| e.id.contains('.') && eq(last))
            },
            &|e| e.exec_program().is_some_and(eq),
        ];
        passes
            .iter()
            .find_map(|pass| self.entries.iter().find(|e| pass(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn resolves_app_ids() {
        let root = std::env::temp_dir().join(format!("sliver-desk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let hi = root.join("hi/applications");
        let lo = root.join("lo/applications");
        write(
            &hi,
            "org.mozilla.firefox.desktop",
            "[Desktop Entry]\nType=Application\nName=Firefox\nName[de]=Feuerfuchs\nIcon=firefox\nExec=firefox %u\n",
        );
        write(
            &lo,
            "org.mozilla.firefox.desktop",
            "[Desktop Entry]\nType=Application\nName=Shadowed\n",
        );
        write(
            &lo,
            "kitty.desktop",
            "[Desktop Entry]\nType=Application\nName=kitty\nIcon=kitty\nExec=/usr/bin/kitty\nStartupWMClass=kitty\n",
        );
        write(
            &lo,
            "gimp-2.10.desktop",
            "[Desktop Entry]\nType=Application\nName=GIMP\nIcon=gimp\nExec=gimp-2.10 %U\nStartupWMClass=Gimp-2.10\nNoDisplay=true\n",
        );
        write(
            &lo,
            "sub/dir-app.desktop",
            "[Desktop Entry]\nType=Application\nName=Nested\n",
        );
        write(
            &lo,
            "hidden.desktop",
            "[Desktop Entry]\nType=Application\nName=Hidden\nHidden=true\n",
        );
        write(
            &lo,
            "link.desktop",
            "[Desktop Entry]\nType=Link\nName=Link\n",
        );
        write(
            &lo,
            "mpv.desktop",
            "[Desktop Entry]\nType=Application\nName=mpv\nExec=env X=1 mpv\n",
        );

        let d = DesktopEntries::scan(&[hi, lo]);
        assert_eq!(
            d.get("org.mozilla.firefox").unwrap().name,
            "Firefox",
            "first dir wins, locale dropped"
        );
        assert_eq!(
            d.find_by_app_id("org.mozilla.firefox")
                .unwrap()
                .icon
                .as_deref(),
            Some("firefox")
        );
        assert_eq!(
            d.find_by_app_id("firefox").unwrap().id,
            "org.mozilla.firefox",
            "reverse-DNS last segment"
        );
        assert_eq!(
            d.find_by_app_id("Firefox").unwrap().id,
            "org.mozilla.firefox",
            "case-insensitive"
        );
        assert_eq!(d.find_by_app_id("kitty").unwrap().name, "kitty");
        let gimp = d.find_by_app_id("Gimp-2.10").unwrap();
        assert_eq!(gimp.id, "gimp-2.10");
        assert!(
            gimp.no_display,
            "hidden-from-menus apps still match windows"
        );
        assert_eq!(d.get("sub-dir-app").unwrap().name, "Nested");
        assert_eq!(d.get("hidden"), None);
        assert_eq!(d.get("link"), None);
        assert_eq!(d.find_by_app_id("kitty.desktop").unwrap().id, "kitty");
        assert_eq!(d.find_by_app_id("nonexistent"), None);
        assert_eq!(d.find_by_app_id(""), None);
        // Exec program fallback (here `env` is not it; the word after env is not parsed)
        assert_eq!(d.find_by_app_id("env").unwrap().id, "mpv");
        fs::remove_dir_all(&root).unwrap();
    }
}
