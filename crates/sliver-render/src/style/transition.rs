//! CSS `transition`: which paint properties animate, how long, and how they ease.
//!
//! Paint properties animate: `color`, `background-color`, `border-color` and `opacity`. So does
//! the box itself, named `width` (or `min-width`): when the layout resizes or moves an element
//! (a collapsed slider opening, neighbours making room), it glides there. `all` covers the paint
//! properties only, so a pasted `transition: all 0.3s` theme doesn't start animating layout
//! (each clock tick would move things). Other property names parse (so pasted GTK/waybar themes
//! load without noise) and are ignored.

use std::time::Duration;

use super::value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionProp {
    All,
    Color,
    Background,
    BorderColor,
    Opacity,
    /// The element's box (`width`, `min-width`): position and size after a re-layout.
    Geometry,
}

/// A CSS timing function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timing {
    Linear,
    /// `cubic-bezier(x1, y1, x2, y2)`; `ease`, `ease-in`, `ease-out`, `ease-in-out` are presets.
    Bezier(f32, f32, f32, f32),
}

impl Timing {
    pub const EASE: Timing = Timing::Bezier(0.25, 0.1, 0.25, 1.0);
    pub const EASE_IN: Timing = Timing::Bezier(0.42, 0.0, 1.0, 1.0);
    pub const EASE_OUT: Timing = Timing::Bezier(0.0, 0.0, 0.58, 1.0);
    pub const EASE_IN_OUT: Timing = Timing::Bezier(0.42, 0.0, 0.58, 1.0);

    /// Eased progress for linear progress `t` in `0..=1`.
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Timing::Linear => t,
            Timing::Bezier(x1, y1, x2, y2) => {
                let bez = |a: f32, b: f32, u: f32| {
                    let v = 1.0 - u;
                    3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
                };
                // Solve x(u) = t by bisection; monotonic for x1, x2 in 0..=1.
                let (mut lo, mut hi) = (0.0f32, 1.0f32);
                for _ in 0..24 {
                    let mid = (lo + hi) / 2.0;
                    if bez(x1, x2, mid) < t {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                bez(y1, y2, (lo + hi) / 2.0)
            }
        }
    }
}

/// One entry of a `transition` list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    pub prop: TransitionProp,
    pub duration: Duration,
    pub delay: Duration,
    pub timing: Timing,
}

/// How the bar changes when its layer changes (`-sliver-layer-transition` on `bar`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerEffect {
    /// The new layer pushes the old one out sideways: from the right when going into a layer
    /// (a media layer, a group), from the left when coming back.
    Slide,
    /// Cross-fade.
    Fade,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerTransition {
    pub effect: LayerEffect,
    pub duration: Duration,
    pub timing: Timing,
}

/// `none` or `slide|fade <time> [timing]` (any order).
pub(crate) fn parse_layer(raw: &str) -> Result<Option<LayerTransition>, String> {
    if raw.trim().eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (mut effect, mut duration, mut tf) = (None, None, Timing::EASE_OUT);
    for t in value::tokens(raw) {
        let tl = t.to_ascii_lowercase();
        match tl.as_str() {
            "slide" => effect = Some(LayerEffect::Slide),
            "fade" => effect = Some(LayerEffect::Fade),
            _ => {
                if let Some(d) = time(&tl) {
                    duration = Some(d);
                } else if let Some(f) = timing(&tl) {
                    tf = f;
                } else {
                    return Err(format!(
                        "unexpected '{t}': want none, or slide|fade <time> [timing]"
                    ));
                }
            }
        }
    }
    let effect = effect.ok_or("needs 'slide' or 'fade' (or 'none')")?;
    let duration = duration.ok_or("needs a duration like 200ms")?;
    Ok((!duration.is_zero()).then_some(LayerTransition {
        effect,
        duration,
        timing: tf,
    }))
}

/// A time: `200ms`, `0.2s`, `.5s`.
fn time(s: &str) -> Option<Duration> {
    let (num, scale) = match (s.strip_suffix("ms"), s.strip_suffix('s')) {
        (Some(n), _) => (n, 0.001),
        (None, Some(n)) => (n, 1.0),
        _ => return None,
    };
    let v: f64 = num.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0)
        .then(|| Duration::from_micros(((v * scale).min(60.0) * 1e6).round() as u64))
}

fn timing(s: &str) -> Option<Timing> {
    Some(match s {
        "linear" => Timing::Linear,
        "ease" => Timing::EASE,
        "ease-in" => Timing::EASE_IN,
        "ease-out" => Timing::EASE_OUT,
        "ease-in-out" => Timing::EASE_IN_OUT,
        _ => {
            let inner = s.strip_prefix("cubic-bezier(")?.strip_suffix(')')?;
            let n: Vec<f32> = inner
                .split(',')
                .filter_map(|p| p.trim().parse().ok())
                .collect();
            match n[..] {
                [x1, y1, x2, y2] if (0.0..=1.0).contains(&x1) && (0.0..=1.0).contains(&x2) => {
                    Timing::Bezier(x1, y1, x2, y2)
                }
                _ => return None,
            }
        }
    })
}

/// Properties we know about but do not animate.
const UNANIMATED: &[&str] = &[
    "min-height",
    "padding",
    "margin",
    "border-width",
    "border-radius",
    "font-size",
    "gap",
    "transform",
    "height",
    "background-position",
    "box-shadow",
    "text-shadow",
];

/// Parses a `transition` value: `none`, or a comma list of
/// `[property] <duration> [timing-function] [delay]` (any order, like CSS).
pub(crate) fn parse(raw: &str) -> Result<Vec<Transition>, String> {
    if raw.trim().eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in value::commas(raw) {
        let mut prop = None;
        let mut times = Vec::new();
        let mut tf = Timing::EASE;
        for t in value::tokens(entry) {
            let tl = t.to_ascii_lowercase();
            if let Some(d) = time(&tl) {
                times.push(d);
            } else if let Some(f) = timing(&tl) {
                tf = f;
            } else if prop.is_none() {
                prop = Some(match tl.as_str() {
                    "all" => Some(TransitionProp::All),
                    "color" => Some(TransitionProp::Color),
                    "background" | "background-color" => Some(TransitionProp::Background),
                    "border-color" | "border" => Some(TransitionProp::BorderColor),
                    "opacity" => Some(TransitionProp::Opacity),
                    "width" | "min-width" => Some(TransitionProp::Geometry),
                    p if UNANIMATED.contains(&p) => None,
                    p => return Err(format!("unknown transition token '{p}'")),
                });
            } else {
                return Err(format!("unexpected '{t}' in transition"));
            }
        }
        let Some(&duration) = times.first() else {
            return Err(format!(
                "transition '{}' needs a duration like 200ms",
                entry.trim()
            ));
        };
        let delay = times.get(1).copied().unwrap_or_default();
        match prop {
            // an unanimated property: nothing to record
            Some(None) => {}
            Some(Some(p)) => out.push(Transition {
                prop: p,
                duration,
                delay,
                timing: tf,
            }),
            None => out.push(Transition {
                prop: TransitionProp::All,
                duration,
                delay,
                timing: tf,
            }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn parses_lists_and_orders() {
        let t = parse("background 200ms ease-in-out, color .1s linear 50ms").unwrap();
        assert_eq!(
            t,
            [
                Transition {
                    prop: TransitionProp::Background,
                    duration: ms(200),
                    delay: ms(0),
                    timing: Timing::EASE_IN_OUT
                },
                Transition {
                    prop: TransitionProp::Color,
                    duration: ms(100),
                    delay: ms(50),
                    timing: Timing::Linear
                },
            ]
        );
        // GTK style: no property means all, any order
        let t = parse("ease-out 0.3s").unwrap();
        assert_eq!(
            (t[0].prop, t[0].duration, t[0].timing),
            (TransitionProp::All, ms(300), Timing::EASE_OUT)
        );
        assert_eq!(
            parse("opacity 1s cubic-bezier(0.1, 0.2, 0.3, 1)").unwrap()[0].timing,
            Timing::Bezier(0.1, 0.2, 0.3, 1.0)
        );
        assert!(parse("none").unwrap().is_empty());
    }

    #[test]
    fn layer_transitions() {
        assert_eq!(
            parse_layer("slide 200ms").unwrap(),
            Some(LayerTransition {
                effect: LayerEffect::Slide,
                duration: ms(200),
                timing: Timing::EASE_OUT
            })
        );
        assert_eq!(
            parse_layer("linear .1s fade").unwrap().unwrap().timing,
            Timing::Linear
        );
        assert_eq!(parse_layer("none").unwrap(), None);
        assert_eq!(parse_layer("fade 0s").unwrap(), None);
        assert!(parse_layer("spin 1s").is_err());
        assert!(parse_layer("slide").is_err());
    }

    #[test]
    fn unanimated_and_bad() {
        assert!(
            parse("min-height 200ms, transform 1s").unwrap().is_empty(),
            "parsed, not animated"
        );
        let geo = parse("min-width 200ms, width 1s").unwrap();
        assert!(
            geo.iter().all(|t| t.prop == TransitionProp::Geometry),
            "{geo:?}"
        );
        assert!(parse("color").unwrap_err().contains("duration"));
        assert!(parse("sparkle 1s").is_err());
        assert!(parse("color 1s opacity").is_err());
    }

    #[test]
    fn timing_curves() {
        for t in [
            Timing::Linear,
            Timing::EASE,
            Timing::EASE_IN,
            Timing::EASE_OUT,
            Timing::EASE_IN_OUT,
        ] {
            assert!(t.at(0.0).abs() < 1e-3 && (t.at(1.0) - 1.0).abs() < 1e-3);
            let mut prev = 0.0;
            for i in 0..=20 {
                let v = t.at(i as f32 / 20.0);
                assert!(v + 1e-4 >= prev, "monotonic");
                prev = v;
            }
        }
        assert!(Timing::EASE_IN.at(0.5) < 0.5, "ease-in starts slow");
        assert!(Timing::EASE_OUT.at(0.5) > 0.5, "ease-out starts fast");
        assert_eq!(Timing::Linear.at(0.25), 0.25);
    }
}
