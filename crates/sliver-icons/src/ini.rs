//! A tiny INI reader for `index.theme` and `.desktop` files: `[section]`, `key=value`,
//! `#` comments. Localized keys (`Name[de]`) are dropped; we only need the C-locale values.

use std::collections::HashMap;

#[derive(Debug, Default)]
pub(crate) struct Ini {
    /// Section names in file order.
    pub order: Vec<String>,
    pub sections: HashMap<String, HashMap<String, String>>,
}

impl Ini {
    pub fn parse(text: &str) -> Ini {
        let mut ini = Ini::default();
        let mut cur: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                if !ini.sections.contains_key(name) {
                    ini.order.push(name.to_string());
                }
                ini.sections.entry(name.to_string()).or_default();
                cur = Some(name.to_string());
                continue;
            }
            let (Some(sec), Some((k, v))) = (&cur, line.split_once('=')) else {
                continue;
            };
            let k = k.trim();
            if k.contains('[') {
                continue;
            }
            // First definition wins, like the glib key-file readers in practice.
            ini.sections
                .get_mut(sec)
                .expect("section exists")
                .entry(k.to_string())
                .or_insert_with(|| v.trim().to_string());
        }
        ini
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.get(key).map(String::as_str)
    }

    pub fn list(&self, section: &str, key: &str) -> Vec<String> {
        self.get(section, key)
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_skips_localized() {
        let i = Ini::parse(
            "# c\n[A]\nName=x\nName[de]=y\nK = v \n\n[B]\nL=1, 2 ,,3\nK=first\nK=second\n",
        );
        assert_eq!(i.order, ["A", "B"]);
        assert_eq!(i.get("A", "Name"), Some("x"));
        assert_eq!(i.get("A", "K"), Some("v"));
        assert_eq!(i.get("B", "K"), Some("first"));
        assert_eq!(i.list("B", "L"), ["1", "2", "3"]);
        assert_eq!(i.get("C", "K"), None);
    }
}
