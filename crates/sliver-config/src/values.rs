//! Small value types shared across the config: durations, widths, alignment, tap timing.

use std::time::Duration;

use toml::Value;

use crate::diag::Diagnostics;

/// Parses `"250ms"`, `"5s"`, `"2m"`, `"1h"`, or a bare number of seconds (waybar compat).
pub fn parse_duration(v: &Value) -> Result<Duration, String> {
    match v {
        Value::Integer(i) if *i >= 0 => Ok(Duration::from_secs(*i as u64)),
        Value::Float(f) if *f >= 0.0 && f.is_finite() => Ok(Duration::from_secs_f64(*f)),
        Value::String(s) => parse_duration_str(s),
        other => Err(format!(
            "expected a duration like \"5s\", got {}",
            other.type_str()
        )),
    }
}

fn parse_duration_str(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("bad duration {s:?}"))?;
    let secs = match unit.trim() {
        "ms" => n / 1000.0,
        "" | "s" => n,
        "m" | "min" => n * 60.0,
        "h" => n * 3600.0,
        u => {
            return Err(format!(
                "bad duration unit {u:?} in {s:?} (use ms, s, m, h)"
            ));
        }
    };
    Ok(Duration::from_secs_f64(secs))
}

/// How wide a module or zone is (Q4/H6). The renderer turns this into pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Width {
    /// Fits the content.
    #[default]
    Auto,
    /// Exactly this many px.
    Fixed(u32),
    /// Shares leftover space by weight (tiny-dfr `Stretch = N` is `Fill(N)`).
    Fill(u32),
}

impl Width {
    pub fn parse(v: &Value) -> Result<Width, String> {
        match v {
            Value::Integer(px) if *px >= 0 && *px <= 4096 => Ok(Width::Fixed(*px as u32)),
            Value::String(s) => match s.as_str() {
                "auto" => Ok(Width::Auto),
                "fill" => Ok(Width::Fill(1)),
                s if s.ends_with("px") => s[..s.len() - 2]
                    .trim()
                    .parse()
                    .map(Width::Fixed)
                    .map_err(|_| format!("bad width {s:?}")),
                s => Err(format!(
                    "bad width {s:?} (use \"auto\", \"fill\", a px number or {{ fill = N }})"
                )),
            },
            Value::Table(t) => match (t.len(), t.get("fill")) {
                (1, Some(Value::Integer(n))) if *n >= 1 && *n <= 1000 => Ok(Width::Fill(*n as u32)),
                _ => Err("a width table must be { fill = N } with N >= 1".into()),
            },
            other => Err(format!("bad width ({})", other.type_str())),
        }
    }

    pub fn to_value(self) -> Value {
        match self {
            Width::Auto => Value::String("auto".into()),
            Width::Fixed(px) => Value::Integer(px.into()),
            Width::Fill(1) => Value::String("fill".into()),
            Width::Fill(n) => {
                let mut t = toml::Table::new();
                t.insert("fill".into(), Value::Integer(n.into()));
                Value::Table(t)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    pub fn parse(s: &str) -> Result<Align, String> {
        match s {
            "start" | "left" => Ok(Align::Start),
            "center" => Ok(Align::Center),
            "end" | "right" => Ok(Align::End),
            _ => Err(format!("bad align {s:?} (use start, center, end)")),
        }
    }
}

/// Whether a tap fires when the finger goes down or when it lifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FireOn {
    Press,
    #[default]
    Release,
}

impl FireOn {
    pub fn parse(s: &str) -> Result<FireOn, String> {
        match s {
            "press" => Ok(FireOn::Press),
            "release" => Ok(FireOn::Release),
            _ => Err(format!("bad fire-on {s:?} (use press or release)")),
        }
    }
}

/// How often a polling module updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
    Once,
    Every(Duration),
}

/// The lowest poll interval we accept (invariant 3: cheap on power).
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

impl Interval {
    pub fn parse(v: &Value, path: &str, d: &mut Diagnostics) -> Option<Interval> {
        if v.as_str() == Some("once") {
            return Some(Interval::Once);
        }
        match parse_duration(v) {
            Ok(dur) if dur < MIN_INTERVAL => {
                d.warn(
                    path,
                    format!("interval {dur:?} is below the 100ms floor; using 100ms"),
                );
                Some(Interval::Every(MIN_INTERVAL))
            }
            Ok(dur) => {
                if dur < Duration::from_secs(1) {
                    d.warn(path, "intervals under 1s cost battery");
                }
                Some(Interval::Every(dur))
            }
            Err(e) => {
                d.error(path, e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let p = |s: &str| parse_duration(&Value::String(s.into())).unwrap();
        assert_eq!(p("250ms"), Duration::from_millis(250));
        assert_eq!(p("5s"), Duration::from_secs(5));
        assert_eq!(p("2m"), Duration::from_secs(120));
        assert_eq!(p("1.5"), Duration::from_millis(1500));
        assert_eq!(
            parse_duration(&Value::Integer(30)).unwrap(),
            Duration::from_secs(30)
        );
        assert!(parse_duration(&Value::String("5 parsecs".into())).is_err());
    }

    #[test]
    fn widths_round_trip() {
        for w in [
            Width::Auto,
            Width::Fixed(120),
            Width::Fill(1),
            Width::Fill(3),
        ] {
            assert_eq!(Width::parse(&w.to_value()), Ok(w));
        }
        assert_eq!(
            Width::parse(&Value::String("90px".into())),
            Ok(Width::Fixed(90))
        );
        assert!(Width::parse(&Value::String("wide".into())).is_err());
    }

    #[test]
    fn interval_floor() {
        let mut d = Diagnostics::default();
        let i = Interval::parse(&Value::String("10ms".into()), "x", &mut d);
        assert_eq!(i, Some(Interval::Every(MIN_INTERVAL)));
        assert_eq!(d.warnings.len(), 1);
        assert_eq!(
            Interval::parse(&Value::String("once".into()), "x", &mut d),
            Some(Interval::Once)
        );
    }
}
