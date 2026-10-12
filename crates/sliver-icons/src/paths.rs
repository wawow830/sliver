//! Where icons and applications live.

use std::path::PathBuf;

/// XDG base directories, injectable for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchPaths {
    pub home: Option<PathBuf>,
    pub data_home: Option<PathBuf>,
    pub data_dirs: Vec<PathBuf>,
    pub config_home: Option<PathBuf>,
}

impl SearchPaths {
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let set = |k: &str| get(k).filter(|v| !v.is_empty());
        let home = set("HOME").map(PathBuf::from);
        let data_home = set("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        let config_home = set("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        let mut data_dirs: Vec<PathBuf> = set("XDG_DATA_DIRS")
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into())
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        // $XDG_DATA_DIRS is often duplicated by session scripts.
        let mut seen = std::collections::HashSet::new();
        data_dirs.retain(|d| seen.insert(d.clone()));
        Self {
            home,
            data_home,
            data_dirs,
            config_home,
        }
    }

    /// `$HOME/.icons`, `$XDG_DATA_HOME/icons`, `$XDG_DATA_DIRS/icons`, in spec priority order.
    pub fn icon_dirs(&self) -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Some(h) = &self.home {
            v.push(h.join(".icons"));
        }
        if let Some(d) = &self.data_home {
            v.push(d.join("icons"));
        }
        v.extend(self.data_dirs.iter().map(|d| d.join("icons")));
        v
    }

    /// `<data dir>/pixmaps`: the spec's last-resort unthemed icons.
    pub fn pixmap_dirs(&self) -> Vec<PathBuf> {
        self.data_dirs.iter().map(|d| d.join("pixmaps")).collect()
    }

    /// `<data home or dir>/applications`.
    pub fn application_dirs(&self) -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Some(d) = &self.data_home {
            v.push(d.join("applications"));
        }
        v.extend(self.data_dirs.iter().map(|d| d.join("applications")));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_defaults_and_dedup() {
        let p = SearchPaths::from_env(|k| match k {
            "HOME" => Some("/h".into()),
            "XDG_DATA_DIRS" => Some("/a:/b::/a".into()),
            _ => None,
        });
        assert_eq!(p.data_home, Some("/h/.local/share".into()));
        assert_eq!(p.data_dirs, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(
            p.icon_dirs(),
            ["/h/.icons", "/h/.local/share/icons", "/a/icons", "/b/icons"].map(PathBuf::from)
        );
        let d = SearchPaths::from_env(|_| None);
        assert_eq!(
            d.data_dirs,
            [
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
    }
}
