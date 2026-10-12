//! The output contract every module shares (customization.md §4): what a script prints, and what
//! built-ins produce too. Plain text is i3blocks-style (`text\ndetail\nclass`); JSON is one
//! object per line with waybar's fields plus the sliver extensions `icon`, `value`, `hidden`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub text: String,
    /// waybar `alt`: picks a `format-alt` / `format-icons` entry.
    pub alt: Option<String>,
    /// waybar `tooltip`.
    pub detail: Option<String>,
    pub classes: Vec<String>,
    /// 0..=100 (not clamped; the style `states` decide).
    pub percentage: Option<f64>,
    /// Icon name or path.
    pub icon: Option<String>,
    /// 0..=100, drives a `slider = true` custom module.
    pub value: Option<f64>,
    pub hidden: bool,
}

impl Output {
    /// What a failed run shows: `!` with the `error` class, the reason as detail.
    pub fn error(reason: impl Into<String>) -> Output {
        Output {
            text: "!".into(),
            classes: vec!["error".into()],
            detail: Some(reason.into()),
            ..Output::default()
        }
    }

    /// A module hidden by `exec-if` or `"hidden": true`.
    pub fn hidden() -> Output {
        Output {
            hidden: true,
            ..Output::default()
        }
    }

    /// Escape `& < >` in the text and detail (`escape = true`), so output is never markup.
    pub fn escaped(mut self) -> Output {
        self.text = escape_markup(&self.text);
        self.detail = self.detail.map(|d| escape_markup(&d));
        self
    }
}

pub fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// i3blocks-style: line 1 text, line 2 detail, line 3 class (space separated).
pub fn parse_text(s: &str) -> Output {
    let mut lines = s.lines();
    let text = lines.next().unwrap_or("").trim_end().to_string();
    let detail = lines
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string);
    let classes = lines
        .next()
        .map(|l| l.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    Output {
        text,
        detail,
        classes,
        ..Output::default()
    }
}

fn string_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Parse one JSON object. Unknown fields are ignored; wrong types for known fields are an error.
pub fn parse_json(line: &str) -> Result<Output, String> {
    let v: Value = serde_json::from_str(line.trim()).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = v.as_object().ok_or("expected a JSON object")?;
    let mut o = Output::default();
    if let Some(t) = obj.get("text") {
        o.text = string_of(t).ok_or("`text` must be a string")?;
    }
    if let Some(a) = obj.get("alt").filter(|a| !a.is_null()) {
        o.alt = Some(string_of(a).ok_or("`alt` must be a string")?);
    }
    // waybar's `tooltip` may be `false`: no tooltip.
    match obj.get("tooltip") {
        Some(Value::Bool(_)) | Some(Value::Null) | None => {}
        Some(t) => o.detail = Some(string_of(t).ok_or("`tooltip` must be a string")?),
    }
    match obj.get("class") {
        Some(Value::String(s)) => o.classes = s.split_whitespace().map(str::to_string).collect(),
        Some(Value::Array(a)) => {
            o.classes = a
                .iter()
                .filter_map(|c| c.as_str())
                .map(str::to_string)
                .collect();
        }
        Some(Value::Null) | None => {}
        Some(_) => return Err("`class` must be a string or an array".into()),
    }
    let num = |k: &str| -> Result<Option<f64>, String> {
        match obj.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_f64()),
            Some(Value::String(s)) => s
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| format!("`{k}` must be a number")),
            Some(_) => Err(format!("`{k}` must be a number")),
        }
    };
    o.percentage = num("percentage")?;
    o.value = num("value")?;
    o.icon = obj
        .get("icon")
        .and_then(|i| i.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    o.hidden = obj.get("hidden").and_then(Value::as_bool).unwrap_or(false);
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Script output is untrusted: no line may panic the parsers (fixed-seed random lines).
    #[test]
    fn random_lines_never_panic() {
        const BITS: &[&str] = &[
            "{",
            "}",
            "[",
            "]",
            "\"",
            ":",
            ",",
            "\"text\"",
            "\"alt\"",
            "\"class\"",
            "\"percentage\"",
            "\"tooltip\"",
            "\"value\"",
            "\"icon\"",
            "null",
            "true",
            "1e999",
            "-1",
            "NaN",
            "\"\\u0000\"",
            "\"\\ud800\"",
            "<b>",
            "&amp;",
            "&",
            "\t",
            "\u{0}",
            "\u{1f600}",
            "x",
            " ",
            "999999999999999999999999",
        ];
        let mut x: u64 = 0x5eed_0005;
        let mut next = || {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        for _ in 0..20_000 {
            let n = next() % 16 + 1;
            let line: String = (0..n)
                .map(|_| BITS[(next() % BITS.len() as u64) as usize])
                .collect();
            let r = std::panic::catch_unwind(|| {
                let _ = parse_json(&line);
                let _ = parse_text(&line);
            });
            assert!(r.is_ok(), "panicked on {line:?}");
        }
    }

    #[test]
    fn text_lines() {
        let o = parse_text("up 3d\nsince monday\ngood fast\n");
        assert_eq!(o.text, "up 3d");
        assert_eq!(o.detail.as_deref(), Some("since monday"));
        assert_eq!(o.classes, ["good", "fast"]);
        assert_eq!(parse_text(""), Output::default());
        assert_eq!(parse_text("only").detail, None);
    }

    #[test]
    fn json_fields() {
        let o = parse_json(
            r#"{"text":"vpn","alt":"on","tooltip":"connected","class":["a","b"],"percentage":"42","icon":"network-vpn","value":7.5,"hidden":false,"extra":1}"#,
        )
        .unwrap();
        assert_eq!(o.text, "vpn");
        assert_eq!(o.alt.as_deref(), Some("on"));
        assert_eq!(o.detail.as_deref(), Some("connected"));
        assert_eq!(o.classes, ["a", "b"]);
        assert_eq!((o.percentage, o.value), (Some(42.0), Some(7.5)));
        assert_eq!(o.icon.as_deref(), Some("network-vpn"));
        assert!(!o.hidden);
        assert_eq!(
            parse_json(r#"{"class":"x y","tooltip":false,"text":5}"#)
                .unwrap()
                .classes,
            ["x", "y"]
        );
        assert_eq!(parse_json(r#"{"text":5}"#).unwrap().text, "5");
        assert!(parse_json(r#"{"hidden":true}"#).unwrap().hidden);
    }

    #[test]
    fn json_errors() {
        assert!(
            parse_json("not json")
                .unwrap_err()
                .starts_with("invalid JSON")
        );
        assert!(parse_json("[1]").is_err());
        assert!(parse_json(r#"{"percentage":"x"}"#).is_err());
        assert!(parse_json(r#"{"class":3}"#).is_err());
        assert!(parse_json(r#"{"text":{}}"#).is_err());
    }

    #[test]
    fn escape_and_error() {
        let o = Output {
            text: "a<b>&".into(),
            detail: Some("<x>".into()),
            ..Default::default()
        }
        .escaped();
        assert_eq!(
            (o.text.as_str(), o.detail.as_deref()),
            ("a&lt;b&gt;&amp;", Some("&lt;x&gt;"))
        );
        let e = Output::error("boom");
        assert_eq!(
            (e.text.as_str(), e.classes.clone()),
            ("!", vec!["error".to_string()])
        );
        assert!(Output::hidden().hidden);
    }
}
