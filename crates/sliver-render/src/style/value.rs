//! Property values: colors (CSS + GTK's `@name`, `alpha()`, `shade()`, `mix()`), lengths, fonts.

use std::collections::HashMap;
use std::str::FromStr;

use crate::Color;

/// A length: `px`, `pt`, `em` (relative to font size) or a bare number (px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Length {
    Px(f32),
    Em(f32),
}

impl Length {
    pub fn px(self, font_size: f32) -> f32 {
        match self {
            Length::Px(v) => v,
            Length::Em(v) => v * font_size,
        }
    }
}

pub(crate) fn length(s: &str) -> Result<Length, String> {
    let s = s.trim();
    let num = |n: &str| n.trim().parse::<f32>().ok().filter(|v| v.is_finite());
    if let Some(v) = s.strip_suffix("px").and_then(num) {
        Ok(Length::Px(v))
    } else if let Some(v) = s.strip_suffix("pt").and_then(num) {
        Ok(Length::Px(v * 4.0 / 3.0))
    } else if let Some(v) = s.strip_suffix("em").and_then(num) {
        Ok(Length::Em(v))
    } else if let Some(v) = num(s) {
        Ok(Length::Px(v))
    } else {
        Err(format!("expected a length like '8px', got '{s}'"))
    }
}

pub(crate) fn non_negative(l: Length) -> Result<Length, String> {
    match l {
        Length::Px(v) | Length::Em(v) if v < 0.0 => Err("must not be negative".into()),
        l => Ok(l),
    }
}

/// Splits on whitespace at nesting depth 0 (so `rgb(1, 2, 3)` stays one token).
pub(crate) fn tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0, None);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if c.is_whitespace() && depth == 0 {
            if let Some(st) = start.take() {
                out.push(&s[st..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

/// Splits on commas at nesting depth 0, outside quotes.
pub(crate) fn commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quote, mut start) = (0, None, 0);
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// 1 to 4 lengths, CSS order: top, right, bottom, left.
pub(crate) fn sides(s: &str) -> Result<[Length; 4], String> {
    let v: Vec<Length> = tokens(s)
        .into_iter()
        .map(length)
        .collect::<Result<_, _>>()?;
    Ok(match v[..] {
        [a] => [a, a, a, a],
        [a, b] => [a, b, a, b],
        [a, b, c] => [a, b, c, b],
        [a, b, c, d] => [a, b, c, d],
        _ => return Err("expected 1 to 4 lengths".into()),
    })
}

pub(crate) fn font_weight(s: &str) -> Result<u16, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "normal" => Ok(400),
        "bold" => Ok(700),
        "lighter" => Ok(300),
        "bolder" => Ok(800),
        n => n
            .parse::<u16>()
            .ok()
            .filter(|w| (1..=1000).contains(w))
            .ok_or_else(|| format!("expected a font weight (normal, bold, 100..900), got '{s}'")),
    }
}

pub(crate) fn font_family(s: &str) -> Result<Vec<String>, String> {
    let v: Vec<String> = commas(s)
        .into_iter()
        .map(|f| f.trim_matches(|c| c == '"' || c == '\'').trim().to_string())
        .filter(|f| !f.is_empty())
        .collect();
    if v.is_empty() {
        Err("expected a font family".into())
    } else {
        Ok(v)
    }
}

pub(crate) fn number_or_percent(s: &str) -> Result<f32, String> {
    let s = s.trim();
    let v = match s.strip_suffix('%') {
        Some(p) => p.trim().parse::<f32>().map(|v| v / 100.0),
        None => s.parse::<f32>(),
    };
    v.ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("expected a number, got '{s}'"))
}

/// Parses a color. `named` holds `@define-color` definitions.
pub(crate) fn color(s: &str, named: &HashMap<String, Color>) -> Result<Color, String> {
    color_depth(s, named, 0)
}

fn color_depth(s: &str, named: &HashMap<String, Color>, depth: u32) -> Result<Color, String> {
    let s = s.trim();
    if depth > 16 {
        return Err("color definitions nest too deeply".into());
    }
    if let Some(name) = s.strip_prefix('@') {
        return named
            .get(name)
            .copied()
            .ok_or_else(|| format!("unknown color '@{name}' (define it with @define-color)"));
    }
    if s.eq_ignore_ascii_case("transparent") || s.eq_ignore_ascii_case("none") {
        return Ok(Color::TRANSPARENT);
    }
    if let Some((func, rest)) = s.split_once('(') {
        let func = func.trim().to_ascii_lowercase();
        if matches!(
            func.as_str(),
            "alpha" | "shade" | "mix" | "lighter" | "darker"
        ) {
            let args = rest
                .strip_suffix(')')
                .ok_or_else(|| format!("missing ')' in '{s}'"))?;
            let args = commas(args);
            let c = |i: usize| color_depth(args.get(i).copied().unwrap_or(""), named, depth + 1);
            let n = |i: usize| number_or_percent(args.get(i).copied().unwrap_or(""));
            let want = |k: usize| {
                if args.len() == k {
                    Ok(())
                } else {
                    Err(format!("{func}() takes {k} arguments"))
                }
            };
            return match func.as_str() {
                "alpha" => {
                    want(2)?;
                    let (col, a) = (c(0)?, n(1)?);
                    Ok(Color::rgba(
                        col.r,
                        col.g,
                        col.b,
                        (col.a as f32 * a.clamp(0.0, 1.0)).round() as u8,
                    ))
                }
                "shade" => {
                    want(2)?;
                    Ok(shade(c(0)?, n(1)?))
                }
                "lighter" => {
                    want(1)?;
                    Ok(shade(c(0)?, 1.3))
                }
                "darker" => {
                    want(1)?;
                    Ok(shade(c(0)?, 0.7))
                }
                _ => {
                    want(3)?;
                    Ok(mix(c(0)?, c(1)?, n(2)?.clamp(0.0, 1.0)))
                }
            };
        }
    }
    let c = svgtypes::Color::from_str(s).map_err(|_| format!("expected a color, got '{s}'"))?;
    Ok(Color::rgba(c.red, c.green, c.blue, c.alpha))
}

pub(crate) fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

/// GTK's `shade()`: scales lightness (and saturation) by `k`.
fn shade(c: Color, k: f32) -> Color {
    // Lightness scaling in HLS, like GTK.
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    let s = if d == 0.0 {
        0.0
    } else {
        d / (1.0 - (2.0 * l - 1.0).abs())
    };
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    let (l, s) = ((l * k).clamp(0.0, 1.0), (s * k).clamp(0.0, 1.0));
    let ch = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = ch * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - ch / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (ch, x, 0.0),
        1 => (x, ch, 0.0),
        2 => (0.0, ch, x),
        3 => (0.0, x, ch),
        4 => (x, 0.0, ch),
        _ => (ch, 0.0, x),
    };
    let u = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::rgba(u(r), u(g), u(b), c.a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        let mut named = HashMap::new();
        named.insert("accent".to_string(), Color::rgb(255, 0, 0));
        let c = |s: &str| color(s, &named);
        assert_eq!(c("#333"), Ok(Color::rgb(0x33, 0x33, 0x33)));
        assert_eq!(c("#11223380"), Ok(Color::rgba(0x11, 0x22, 0x33, 0x80)));
        assert_eq!(c("rgba(255, 0, 0, 0.5)"), Ok(Color::rgba(255, 0, 0, 128)));
        assert_eq!(c("white"), Ok(Color::WHITE));
        assert_eq!(c("transparent"), Ok(Color::TRANSPARENT));
        assert_eq!(c("@accent"), Ok(Color::rgb(255, 0, 0)));
        assert_eq!(c("alpha(@accent, 0.5)"), Ok(Color::rgba(255, 0, 0, 128)));
        assert_eq!(c("mix(black, white, 0.5)"), Ok(Color::rgb(128, 128, 128)));
        assert_eq!(c("shade(#808080, 0.5)"), Ok(Color::rgb(64, 64, 64)));
        assert!(c("@nope").unwrap_err().contains("unknown color"));
        assert!(c("blurple").is_err());
        assert!(c("alpha(red)").is_err());
    }

    #[test]
    fn lengths_and_lists() {
        assert_eq!(length("8px"), Ok(Length::Px(8.0)));
        assert_eq!(length("0"), Ok(Length::Px(0.0)));
        assert_eq!(length("1.5em"), Ok(Length::Em(1.5)));
        assert_eq!(length("12pt"), Ok(Length::Px(16.0)));
        assert!(length("wide").is_err());
        assert_eq!(
            sides("1px 2px").unwrap(),
            [
                Length::Px(1.0),
                Length::Px(2.0),
                Length::Px(1.0),
                Length::Px(2.0)
            ]
        );
        assert!(sides("1px 2px 3px 4px 5px").is_err());
        assert_eq!(
            tokens("2px solid rgba(0, 0, 0, 1)"),
            ["2px", "solid", "rgba(0, 0, 0, 1)"]
        );
        assert_eq!(
            font_family("\"Noto Sans\", 'A, B', sans-serif").unwrap(),
            ["Noto Sans", "A, B", "sans-serif"]
        );
        assert_eq!(font_weight("bold"), Ok(700));
        assert!(font_weight("heavy").is_err());
    }
}
