//! waybar's `rewrite` for window titles: regex → replacement rules.
//!
//! Every rule whose regex matches the *whole* original text rewrites the result, in order, as in
//! waybar. Replacements use `$1`/`${name}` for captures. Two forms:
//!
//! ```toml
//! rewrite = { "(.*) — Mozilla Firefox" = "🌎 $1" }            # a table: applied in key order
//! rewrite = [{ match = "(.*) - Code", replace = "✎ $1" }]     # an array: applied as written
//! ```
//!
//! TOML tables have no order here, so use the array form when one rule must run before another.

use regex_lite::Regex;
use toml::Value;

use crate::diag::{Diagnostics, join};

#[derive(Debug, Clone, Default)]
pub struct Rewrite(Vec<(Regex, String)>);

impl Rewrite {
    /// The rules in `v`. Bad entries are reported in `d` and skipped.
    pub fn parse(v: &Value, path: &str, d: &mut Diagnostics) -> Rewrite {
        let mut rules = Vec::new();
        let mut add = |pattern: &str, replace: &str, p: &str, d: &mut Diagnostics| match Regex::new(
            &format!("^(?:{pattern})$"),
        ) {
            Ok(re) => rules.push((re, replace.to_owned())),
            Err(e) => d.warn(p, format!("bad regex, rule ignored: {e}")),
        };
        match v {
            Value::Table(t) => {
                for (pattern, replace) in t {
                    let p = join(path, pattern);
                    match replace.as_str() {
                        Some(r) => add(pattern, r, &p, d),
                        None => d.warn(p, "the replacement must be a string"),
                    }
                }
            }
            Value::Array(a) => {
                for (i, rule) in a.iter().enumerate() {
                    let p = format!("{path}[{i}]");
                    let get = |k| rule.get(k).and_then(Value::as_str);
                    match (get("match"), get("replace")) {
                        (Some(m), Some(r)) => add(m, r, &p, d),
                        _ => d.warn(p, "a rule is { match = \"regex\", replace = \"text\" }"),
                    }
                }
            }
            _ => d.warn(
                path,
                "rewrite is a table of regex = replacement, or an array of rules",
            ),
        }
        Rewrite(rules)
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (re, replace) in &self.0 {
            if re.is_match(text) {
                out = re.replace(&out, replace.as_str()).into_owned();
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(src: &str) -> (Rewrite, Diagnostics) {
        let v: toml::Table = src.parse().unwrap();
        let mut d = Diagnostics::default();
        (Rewrite::parse(&v["rewrite"], "rewrite", &mut d), d)
    }

    #[test]
    fn whole_matches_rewrite_in_order() {
        let (r, d) = rules(
            r#"rewrite = [
                { match = "(.*) - Mozilla Firefox", replace = "🌎 $1" },
                { match = ".*Firefox", replace = "[web] $0" },
                { match = ".*Docs.*", replace = "📄 $0" },
            ]"#,
        );
        assert!(d.ok() && d.warnings.is_empty(), "{d:?}");
        // Each rule is tested against the original title and replaces in the result so far,
        // as in waybar: the second rule matched the title but not "🌎 Docs"; the third matches both.
        assert_eq!(r.apply("Docs - Mozilla Firefox"), "📄 🌎 Docs");
        assert_eq!(
            r.apply("Firefox Nightly"),
            "Firefox Nightly",
            "must match the whole title"
        );
        assert_eq!(r.apply("Terminal"), "Terminal");
    }

    #[test]
    fn table_form_and_bad_rules() {
        let (r, d) = rules(r#"rewrite = { "(.*) - Code" = "✎ $1", "(" = "x", "y" = 1 }"#);
        assert_eq!(r.apply("main.rs - Code"), "✎ main.rs");
        assert_eq!(d.warnings.len(), 2, "{d:?}");
        assert!(d.warnings.iter().any(|w| w.message.contains("bad regex")));
    }
}
