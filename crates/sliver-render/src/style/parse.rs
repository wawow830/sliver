//! CSS syntax: comments, at-rules, rules and declarations, with positions for diagnostics.
//! Values are kept as raw text here and interpreted in `value.rs`.

use super::Diagnostic;

/// A source file (or embedded sheet) being parsed. Positions are byte offsets into `text`.
pub(crate) struct Source<'a> {
    pub name: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> Source<'a> {
    pub fn new(name: &'a str, text: &'a str) -> Source<'a> {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Source { name, line_starts }
    }

    pub fn diag(&self, pos: usize, message: impl Into<String>) -> Diagnostic {
        let line = self.line_starts.partition_point(|&s| s <= pos);
        let col = pos - self.line_starts[line - 1] + 1;
        Diagnostic {
            source: self.name.to_string(),
            line: line as u32,
            column: col as u32,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Item<'a> {
    /// `@name prelude;`
    AtStatement {
        name: &'a str,
        prelude: &'a str,
        pos: usize,
    },
    /// `@name prelude { ... }`: not supported, reported and skipped.
    AtBlock { name: &'a str, pos: usize },
    Rule {
        selectors: &'a str,
        selectors_pos: usize,
        decls: Vec<RawDecl<'a>>,
    },
}

#[derive(Debug)]
pub(crate) struct RawDecl<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub important: bool,
    pub pos: usize,
}

/// Replaces comments with spaces (keeping newlines and offsets), respecting strings.
fn strip_comments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut quote = None;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                out.push(c);
                if c == b'\\' && i + 1 < b.len() {
                    out.push(b[i + 1]);
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == b'/' && b.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                out.extend(
                    b[i..end]
                        .iter()
                        .map(|&c| if c == b'\n' { b'\n' } else { b' ' }),
                );
                i = end;
                continue;
            }
            None => {
                if c == b'"' || c == b'\'' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
        i += 1;
    }
    // Only ASCII bytes were replaced by ASCII, so this stays valid UTF-8.
    String::from_utf8(out).expect("comment stripping keeps UTF-8")
}

/// Finds the first of `stops` at nesting depth 0 (outside strings, parens and brackets).
fn scan_to(text: &str, from: usize, stops: &[u8]) -> Option<usize> {
    let b = text.as_bytes();
    let (mut depth, mut quote, mut i) = (0i32, None, from);
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                _ if depth <= 0 && stops.contains(&c) => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Index just past the `}` matching the `{` at `open`.
fn block_end(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    loop {
        let j = scan_to(text, i, b"{}")?;
        if text.as_bytes()[j] == b'{' {
            depth += 1;
        } else {
            depth -= 1;
            if depth == 0 {
                return Some(j + 1);
            }
        }
        i = j + 1;
    }
}

/// Parses a whole sheet. `clean` must be the comment-stripped text of `src` (same offsets).
pub(crate) fn parse_sheet<'a>(
    src: &Source<'_>,
    clean: &'a str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Item<'a>> {
    let mut items = Vec::new();
    let mut i = 0;
    let b = clean.as_bytes();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        if b[i] == b'@' {
            let name_end = clean[i + 1..]
                .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
                .map_or(clean.len(), |e| i + 1 + e);
            let name = &clean[i + 1..name_end];
            match scan_to(clean, name_end, b";{") {
                Some(j) if b[j] == b';' => {
                    items.push(Item::AtStatement {
                        name,
                        prelude: clean[name_end..j].trim(),
                        pos: start,
                    });
                    i = j + 1;
                }
                Some(j) => {
                    items.push(Item::AtBlock { name, pos: start });
                    match block_end(clean, j) {
                        Some(e) => i = e,
                        None => {
                            diags.push(src.diag(j, "unclosed '{'"));
                            break;
                        }
                    }
                }
                None => {
                    diags.push(src.diag(start, format!("'@{name}' is missing ';'")));
                    break;
                }
            }
            continue;
        }
        let Some(open) = scan_to(clean, i, b"{};") else {
            diags.push(src.diag(start, "expected '{' after selector"));
            break;
        };
        if b[open] != b'{' {
            diags.push(src.diag(open, format!("unexpected '{}'", b[open] as char)));
            i = open + 1;
            continue;
        }
        let Some(end) = block_end(clean, open) else {
            diags.push(src.diag(open, "unclosed '{'"));
            break;
        };
        let body = open + 1..end - 1;
        if let Some(nested) = scan_to(&clean[..body.end], body.start, b"{") {
            diags.push(src.diag(nested, "nested rules are not supported; rule skipped"));
        } else {
            items.push(Item::Rule {
                selectors: clean[start..open].trim(),
                selectors_pos: start,
                decls: parse_decls(src, clean, body.start, body.end, diags),
            });
        }
        i = end;
    }
    items
}

fn parse_decls<'a>(
    src: &Source<'_>,
    clean: &'a str,
    mut i: usize,
    end: usize,
    diags: &mut Vec<Diagnostic>,
) -> Vec<RawDecl<'a>> {
    let mut decls = Vec::new();
    let body = &clean[..end];
    while i < end {
        let stop = scan_to(body, i, b";").unwrap_or(end);
        let text = &clean[i..stop];
        if !text.trim().is_empty() {
            let pos = i + (text.len() - text.trim_start().len());
            match text.split_once(':') {
                Some((name, value)) => {
                    let mut value = value.trim();
                    let mut important = false;
                    if let Some(v) = value.strip_suffix("!important") {
                        value = v.trim_end();
                        important = true;
                    }
                    decls.push(RawDecl {
                        name: name.trim(),
                        value,
                        important,
                        pos,
                    });
                }
                None => diags.push(src.diag(
                    pos,
                    format!("expected 'property: value', got '{}'", text.trim()),
                )),
            }
        }
        i = stop + 1;
    }
    decls
}

pub(crate) fn clean(text: &str) -> String {
    strip_comments(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Vec<String>, Vec<String>) {
        let src = Source::new("t.css", text);
        let clean = clean(text);
        let mut diags = Vec::new();
        let items = parse_sheet(&src, &clean, &mut diags);
        let items = items
            .iter()
            .map(|i| match i {
                Item::AtStatement { name, prelude, .. } => format!("@{name} {prelude}"),
                Item::AtBlock { name, .. } => format!("@{name} {{}}"),
                Item::Rule {
                    selectors, decls, ..
                } => {
                    let d: Vec<String> = decls
                        .iter()
                        .map(|d| {
                            format!(
                                "{}={}{}",
                                d.name,
                                d.value,
                                if d.important { "!" } else { "" }
                            )
                        })
                        .collect();
                    format!("{selectors} {{{}}}", d.join(";"))
                }
            })
            .collect();
        (items, diags.iter().map(|d| d.to_string()).collect())
    }

    #[test]
    fn rules_at_rules_and_comments() {
        let (items, diags) = parse(
            "/* hi { } */ @define-color accent #f00;\n\
             @import \"x.css\";\n\
             #battery.critical, bar > zone { color: red; background: url(\"a;b\") !important }\n\
             @keyframes blink { from { opacity: 0 } }\n\
             module{font-family:\"A /* not a comment */\", sans}",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            items,
            [
                "@define-color accent #f00",
                "@import \"x.css\"",
                "#battery.critical, bar > zone {color=red;background=url(\"a;b\")!}",
                "@keyframes {}",
                "module {font-family=\"A /* not a comment */\", sans}",
            ]
        );
    }

    #[test]
    fn errors_have_positions_and_recover() {
        let (items, diags) =
            parse("a { color red; size: 1px }\n\n  b { x { } }\nc { color: blue }");
        assert_eq!(items, ["a {size=1px}", "c {color=blue}"]);
        assert_eq!(
            diags,
            [
                "t.css:1:5: expected 'property: value', got 'color red'",
                "t.css:3:9: nested rules are not supported; rule skipped",
            ]
        );
        let (_, diags) = parse("a { color: red;\n");
        assert_eq!(diags, ["t.css:1:3: unclosed '{'"]);
    }
}
