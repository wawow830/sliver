//! A Pango-markup subset for format strings (customization.md G4).
//!
//! Waybar labels are Pango markup. The renderer isn't Pango (ADR-0002), so this parses the part
//! that matters on a status bar into styled text runs that `cosmic-text` can shape:
//!
//! * tags `<b> <i> <u> <s> <big> <small> <sub> <sup> <tt> <span …>` (and `<strong>`/`<em>`
//!   spellings are *not* Pango, so they warn like any unknown tag);
//! * `<span>` attributes `foreground`/`fgcolor`/`color`, `weight`/`font_weight`,
//!   `style`/`font_style`, `size`/`font_size` (keywords, `NN%`, `smaller`/`larger`),
//!   `underline`, `strikethrough`, `font_family`/`face`;
//! * the entities `&amp; &lt; &gt; &quot; &apos; &#N; &#xN;`.
//!
//! Parsing never fails: bad markup keeps its text, loses its styling where it can't be
//! understood, and reports a warning, so a broken script can't blank a module. Cases Pango
//! supports that we don't (background colours, baseline shift, font descriptions) warn and are
//! ignored; `<sub>`/`<sup>` only shrink the text.

use std::fmt::Write;

#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    /// CSS weight 100–900; `None` inherits the label's weight.
    pub weight: Option<u16>,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// RGBA; `None` inherits the item's colour.
    pub color: Option<[u8; 4]>,
    /// Multiplies the label's font size.
    pub scale: f32,
    pub family: Option<String>,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            weight: None,
            italic: false,
            underline: false,
            strikethrough: false,
            color: None,
            scale: 1.0,
            family: None,
        }
    }
}

/// A run of text with one style.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Markup {
    pub spans: Vec<Span>,
    pub warnings: Vec<String>,
}

impl Markup {
    /// The text without any tags, entities resolved.
    pub fn plain(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// Number of characters (`char`s) of plain text.
    pub fn len_chars(&self) -> usize {
        self.spans.iter().map(|s| s.text.chars().count()).sum()
    }

    /// The first `n` characters, styles kept.
    pub fn truncated(&self, n: usize) -> Markup {
        let mut left = n;
        let mut spans = Vec::new();
        for s in &self.spans {
            if left == 0 {
                break;
            }
            let count = s.text.chars().count();
            if count <= left {
                spans.push(s.clone());
                left -= count;
            } else {
                let cut: String = s.text.chars().take(left).collect();
                spans.push(Span {
                    text: cut,
                    style: s.style.clone(),
                });
                left = 0;
            }
        }
        Markup {
            spans,
            warnings: Vec::new(),
        }
    }

    /// Back to markup text (explicit `<span>` attributes only), so a truncated label can go
    /// through the same shaping path and cache as the original.
    pub fn to_markup(&self) -> String {
        let mut out = String::new();
        for s in &self.spans {
            let d = Style::default();
            let st = &s.style;
            let mut attrs = String::new();
            if let Some(w) = st.weight {
                let _ = write!(attrs, " weight=\"{w}\"");
            }
            if st.italic {
                attrs.push_str(" style=\"italic\"");
            }
            if st.underline {
                attrs.push_str(" underline=\"single\"");
            }
            if st.strikethrough {
                attrs.push_str(" strikethrough=\"true\"");
            }
            if let Some([r, g, b, a]) = st.color {
                let _ = write!(attrs, " foreground=\"#{r:02x}{g:02x}{b:02x}{a:02x}\"");
            }
            if (st.scale - d.scale).abs() > f32::EPSILON {
                let _ = write!(attrs, " size=\"{}%\"", (st.scale * 100.0).round());
            }
            if let Some(f) = &st.family {
                let _ = write!(attrs, " font_family=\"{}\"", escape(f));
            }
            if attrs.is_empty() {
                out.push_str(&escape(&s.text));
            } else {
                let _ = write!(out, "<span{attrs}>{}</span>", escape(&s.text));
            }
        }
        out
    }
}

/// Escape `& < > "` for use as markup text or an attribute value.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

struct Frame {
    tag: String,
    style: Style,
}

struct Parser {
    stack: Vec<Frame>,
    spans: Vec<Span>,
    warnings: Vec<String>,
}

impl Parser {
    fn current(&self) -> Style {
        self.stack
            .last()
            .map(|f| f.style.clone())
            .unwrap_or_default()
    }

    fn text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        let style = self.current();
        match self.spans.last_mut() {
            Some(last) if last.style == style => last.text.push_str(t),
            _ => self.spans.push(Span {
                text: t.to_string(),
                style,
            }),
        }
    }

    fn warn(&mut self, m: impl Into<String>) {
        let m = m.into();
        if !self.warnings.contains(&m) {
            self.warnings.push(m);
        }
    }
}

/// Parse `src`. Always succeeds; see the module docs.
pub fn parse(src: &str) -> Markup {
    let mut p = Parser {
        stack: Vec::new(),
        spans: Vec::new(),
        warnings: Vec::new(),
    };
    let mut rest = src;
    let mut buf = String::new();
    while let Some(c) = rest.chars().next() {
        match c {
            '<' if !starts_tag(rest) => {
                p.warn("a `<` that does not start a tag was kept as text (write &lt;)");
                buf.push('<');
                rest = &rest[1..];
            }
            '<' => match rest.find('>') {
                Some(end) => {
                    p.text(&std::mem::take(&mut buf));
                    tag(&mut p, &rest[1..end]);
                    rest = &rest[end + 1..];
                }
                None => {
                    p.warn("a `<` without a closing `>` was kept as text");
                    buf.push('<');
                    rest = &rest[1..];
                }
            },
            '&' => match entity(rest) {
                Some((ch, len)) => {
                    buf.push(ch);
                    rest = &rest[len..];
                }
                None => {
                    p.warn("a `&` that is not a known entity was kept as text (write &amp;)");
                    buf.push('&');
                    rest = &rest[1..];
                }
            },
            c => {
                buf.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    p.text(&buf);
    for f in std::mem::take(&mut p.stack) {
        p.warn(format!("<{}> is never closed", f.tag));
    }
    Markup {
        spans: p.spans,
        warnings: p.warnings,
    }
}

/// `<b`, `</b`: a letter (optionally after `/`) must follow, so "1 < 2 and 3 > 2" stays text.
fn starts_tag(s: &str) -> bool {
    let after = s[1..].strip_prefix('/').unwrap_or(&s[1..]);
    after
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
}

fn entity(s: &str) -> Option<(char, usize)> {
    let semi = s.find(';').filter(|&i| i <= 10)?;
    let name = &s[1..semi];
    let ch = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let num = name.strip_prefix('#')?;
            let n = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => num.parse().ok()?,
            };
            char::from_u32(n).filter(|c| *c != '\0')?
        }
    };
    Some((ch, semi + 1))
}

fn tag(p: &mut Parser, inner: &str) {
    let inner = inner.trim();
    if let Some(name) = inner.strip_prefix('/') {
        close(p, name.trim());
        return;
    }
    let self_closing = inner.ends_with('/');
    let inner = inner.trim_end_matches('/').trim();
    let (name, attrs) = inner.split_once(char::is_whitespace).unwrap_or((inner, ""));
    let mut style = p.current();
    match name {
        "b" => style.weight = Some(700),
        "i" => style.italic = true,
        "u" => style.underline = true,
        "s" => style.strikethrough = true,
        "big" => style.scale *= 1.2,
        "small" => style.scale /= 1.2,
        "sub" | "sup" => {
            style.scale *= 0.75;
            p.warn("<sub>/<sup> only shrink the text (no baseline shift)");
        }
        "tt" => style.family = Some("monospace".into()),
        "span" => span_attrs(p, &mut style, attrs),
        other => p.warn(format!("unknown tag <{other}> ignored")),
    }
    if !self_closing {
        p.stack.push(Frame {
            tag: name.to_string(),
            style,
        });
    }
}

fn close(p: &mut Parser, name: &str) {
    match p.stack.iter().rposition(|f| f.tag == name) {
        Some(i) => {
            if i + 1 != p.stack.len() {
                p.warn(format!("</{name}> closes tags that were opened after it"));
            }
            p.stack.truncate(i);
        }
        None => p.warn(format!("</{name}> has no matching opening tag")),
    }
}

/// `key="value"` / `key='value'` pairs.
fn attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim_start();
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let body = &after[1..];
        let Some(end) = body.find(q) else { break };
        out.push((key, body[..end].to_string()));
        rest = body[end + 1..].trim_start();
    }
    out
}

fn span_attrs(p: &mut Parser, st: &mut Style, attrs: &str) {
    for (k, v) in attributes(attrs) {
        match k.as_str() {
            "foreground" | "fgcolor" | "color" => match color(&v) {
                Some(c) => st.color = Some(c),
                None => p.warn(format!("unknown colour {v:?}")),
            },
            "weight" | "font_weight" => match weight(&v) {
                Some(w) => st.weight = Some(w),
                None => p.warn(format!("unknown weight {v:?}")),
            },
            "style" | "font_style" => match v.as_str() {
                "italic" | "oblique" => st.italic = true,
                "normal" => st.italic = false,
                _ => p.warn(format!("unknown style {v:?}")),
            },
            "size" | "font_size" => match size(&v) {
                Some(Size::Abs(f)) => st.scale = f,
                Some(Size::Rel(f)) => st.scale *= f,
                None => p.warn(format!(
                    "unsupported size {v:?} (use a keyword or a percentage)"
                )),
            },
            "underline" => match v.as_str() {
                "none" => st.underline = false,
                "single" | "double" | "low" | "error" => st.underline = true,
                _ => p.warn(format!("unknown underline {v:?}")),
            },
            "strikethrough" => st.strikethrough = v == "true",
            "font_family" | "face" => st.family = Some(v),
            "background" | "bgcolor" | "background_color" => {
                p.warn("background colours in markup are not supported; use a stylesheet");
            }
            other => p.warn(format!("unsupported <span> attribute {other:?} ignored")),
        }
    }
}

fn weight(v: &str) -> Option<u16> {
    Some(match v {
        "thin" => 100,
        "ultralight" => 200,
        "light" => 300,
        "semilight" => 350,
        "book" | "normal" => 400,
        "medium" => 500,
        "semibold" => 600,
        "bold" => 700,
        "ultrabold" => 800,
        "heavy" | "ultraheavy" => 900,
        n => return n.parse::<u16>().ok().filter(|w| (100..=1000).contains(w)),
    })
}

enum Size {
    Abs(f32),
    Rel(f32),
}

fn size(v: &str) -> Option<Size> {
    Some(match v {
        "xx-small" => Size::Abs(0.579),
        "x-small" => Size::Abs(0.644),
        "small" => Size::Abs(0.833),
        "medium" => Size::Abs(1.0),
        "large" => Size::Abs(1.2),
        "x-large" => Size::Abs(1.44),
        "xx-large" => Size::Abs(1.728),
        "smaller" => Size::Rel(1.0 / 1.2),
        "larger" => Size::Rel(1.2),
        p => {
            let pct: f32 = p.strip_suffix('%')?.trim().parse().ok()?;
            Size::Abs((pct / 100.0).clamp(0.1, 8.0))
        }
    })
}

pub fn color(v: &str) -> Option<[u8; 4]> {
    if let Some(hex) = v.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let nib = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|n| n * 17);
        return match hex.len() {
            3 => Some([nib(0)?, nib(1)?, nib(2)?, 255]),
            4 => Some([nib(0)?, nib(1)?, nib(2)?, nib(3)?]),
            6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
            8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
            _ => None,
        };
    }
    Some(match v.to_ascii_lowercase().as_str() {
        "black" => [0, 0, 0, 255],
        "white" => [255, 255, 255, 255],
        "red" => [255, 0, 0, 255],
        "green" => [0, 128, 0, 255],
        "lime" => [0, 255, 0, 255],
        "blue" => [0, 0, 255, 255],
        "yellow" => [255, 255, 0, 255],
        "cyan" | "aqua" => [0, 255, 255, 255],
        "magenta" | "fuchsia" => [255, 0, 255, 255],
        "gray" | "grey" => [128, 128, 128, 255],
        "orange" => [255, 165, 0, 255],
        "purple" => [128, 0, 128, 255],
        "pink" => [255, 192, 203, 255],
        "brown" => [165, 42, 42, 255],
        "transparent" => [0, 0, 0, 0],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(m: &Markup) -> Vec<(&str, Option<u16>, bool)> {
        m.spans
            .iter()
            .map(|s| (s.text.as_str(), s.style.weight, s.style.italic))
            .collect()
    }

    #[test]
    fn plain_text_is_one_span() {
        let m = parse("hello");
        assert_eq!(texts(&m), [("hello", None, false)]);
        assert!(m.warnings.is_empty());
        assert_eq!(parse("").spans.len(), 0);
    }

    #[test]
    fn basic_tags_nest() {
        let m = parse("a<b>b<i>c</i>d</b>e");
        assert_eq!(
            texts(&m),
            [
                ("a", None, false),
                ("b", Some(700), false),
                ("c", Some(700), true),
                ("d", Some(700), false),
                ("e", None, false)
            ]
        );
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        assert_eq!(m.plain(), "abcde");
    }

    #[test]
    fn span_attributes() {
        let m = parse(
            r##"<span foreground="#ff8000" weight='light' style="italic" size="large" underline="single" strikethrough="true" font_family="Mono">x</span>"##,
        );
        let s = &m.spans[0].style;
        assert_eq!(s.color, Some([255, 128, 0, 255]));
        assert_eq!(
            (s.weight, s.italic, s.underline, s.strikethrough),
            (Some(300), true, true, true)
        );
        assert!((s.scale - 1.2).abs() < 1e-6);
        assert_eq!(s.family.as_deref(), Some("Mono"));
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn colors() {
        assert_eq!(color("#fff"), Some([255, 255, 255, 255]));
        assert_eq!(color("#0f08"), Some([0, 255, 0, 136]));
        assert_eq!(color("#102030"), Some([16, 32, 48, 255]));
        assert_eq!(color("#10203040"), Some([16, 32, 48, 64]));
        assert_eq!(color("Red"), Some([255, 0, 0, 255]));
        assert_eq!(color("#12"), None);
        assert_eq!(color("#ggg"), None);
        assert_eq!(color("nope"), None);
    }

    #[test]
    fn sizes_combine() {
        let m = parse(
            r#"<big>a<span size="smaller">b</span></big><small>c</small><span size="50%">d</span>"#,
        );
        let scales: Vec<f32> = m
            .spans
            .iter()
            .map(|s| (s.style.scale * 1000.0).round() / 1000.0)
            .collect();
        assert_eq!(scales, [1.2, 1.0, 0.833, 0.5]);
    }

    #[test]
    fn entities() {
        assert_eq!(
            parse("a &lt;b&gt; &amp; &quot;c&quot; &#65;&#x42; &apos;").plain(),
            "a <b> & \"c\" AB '"
        );
        let m = parse("R&D");
        assert_eq!(m.plain(), "R&D");
        assert_eq!(m.warnings.len(), 1);
        assert_eq!(parse("&#0;").plain(), "&#0;", "NUL is not a character");
    }

    #[test]
    fn lenient_with_warnings() {
        let m = parse("a<blink>b</blink> <b>c");
        assert_eq!(m.plain(), "ab c");
        assert!(m.warnings.iter().any(|w| w.contains("unknown tag <blink>")));
        assert!(m.warnings.iter().any(|w| w.contains("<b> is never closed")));
        let m = parse("x</i>y");
        assert_eq!(m.plain(), "xy");
        assert!(m.warnings[0].contains("no matching"));
        let m = parse("<b><i>x</b>y");
        assert!(m.warnings.iter().any(|w| w.contains("closes tags")));
        assert_eq!(
            m.spans.last().unwrap().style.weight,
            None,
            "</b> closed the italic frame too"
        );
        let m = parse("1 < 2");
        assert_eq!(m.plain(), "1 < 2");
        assert_eq!(parse("1 < 2 and 3 > 2").plain(), "1 < 2 and 3 > 2");
        let m = parse(r#"<span foreground="nope" bogus="1" background="red">z</span>"#);
        assert_eq!(m.plain(), "z");
        assert_eq!(m.warnings.len(), 3);
    }

    #[test]
    fn self_closing_and_merging() {
        let m = parse("a<b/>b");
        assert_eq!(
            m.spans.len(),
            1,
            "an empty <b/> styles nothing: equal neighbours merge"
        );
        let m = parse("<b>a</b><b>b</b>");
        assert_eq!(m.spans.len(), 1);
    }

    #[test]
    fn truncate_and_roundtrip() {
        let m = parse(r##"ab<b>cd</b><span foreground="#ff0000">ef</span>"##);
        assert_eq!(m.len_chars(), 6);
        let t = m.truncated(3);
        assert_eq!(t.plain(), "abc");
        assert_eq!(t.spans[1].style.weight, Some(700));
        assert_eq!(m.truncated(0).spans.len(), 0);
        assert_eq!(m.truncated(99).plain(), "abcdef");
        let again = parse(&m.to_markup());
        assert_eq!(
            again.spans, m.spans,
            "to_markup parses back to the same runs"
        );
        assert_eq!(parse(&parse("a<b&amp;").to_markup()).plain(), "a<b&");
    }
}
