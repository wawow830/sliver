//! sliver-core: the widget brain.
//!
//! One renderer, shared by the daemon (real strip) and the customizer
//! (preview), so what you see in the editor is what lands under your finger.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The touchbar's real estate, after rotation.
pub const STRIP_W: f64 = 2008.0;
pub const STRIP_H: f64 = 60.0;

const PAD: f64 = 16.0;
const GAP: f64 = 8.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Background color, "#rrggbb".
    #[serde(default = "default_bg")]
    pub background: String,
    pub widgets: Vec<WidgetCfg>,
}

fn default_bg() -> String {
    "#000000".into()
}
fn default_clock_fmt() -> String {
    "%H:%M".into()
}
fn default_battery_fmt() -> String {
    "{capacity}%".into()
}
fn default_font_size() -> f64 {
    24.0
}
fn one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

/// Shared style knobs every text-bearing widget carries.
#[derive(Debug, Clone)]
pub struct Style {
    pub color: Option<String>,
    pub font_size: f64,
    pub bold: bool,
    pub bg: Option<String>,
    pub align: Align,
}

/// Mutable view of those same knobs, named instead of smuggled around as
/// an unreadable five-reference tuple.
pub struct StyleMut<'a> {
    pub color: &'a mut Option<String>,
    pub font_size: &'a mut f64,
    pub bold: &'a mut bool,
    pub bg: &'a mut Option<String>,
    pub align: &'a mut Align,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WidgetCfg {
    /// Static text. May carry an action — tap it on the glass and it runs.
    Label {
        text: String,
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        color: Option<String>,
        width: Option<f64>,
        #[serde(default = "default_font_size")]
        font_size: f64,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        bg: Option<String>,
        #[serde(default)]
        align: Align,
    },
    /// A strftime clock. Ticks are the daemon's problem; the core just draws.
    Clock {
        #[serde(default = "default_clock_fmt")]
        format: String,
        #[serde(default)]
        color: Option<String>,
        width: Option<f64>,
        #[serde(default = "default_font_size")]
        font_size: f64,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        bg: Option<String>,
        #[serde(default)]
        align: Align,
    },
    /// Live battery level from /sys/class/power_supply. Colors itself by
    /// level unless told otherwise: mint, amber, red.
    Battery {
        #[serde(default = "default_battery_fmt")]
        format: String,
        #[serde(default)]
        color: Option<String>,
        width: Option<f64>,
        #[serde(default = "default_font_size")]
        font_size: f64,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        bg: Option<String>,
        #[serde(default)]
        align: Align,
    },
    /// A tappable button: text plus a shell command, proudly pill-shaped
    /// if you give it a background.
    Button {
        text: String,
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        color: Option<String>,
        width: Option<f64>,
        #[serde(default = "default_font_size")]
        font_size: f64,
        #[serde(default)]
        bold: bool,
        #[serde(default)]
        bg: Option<String>,
        #[serde(default)]
        align: Align,
    },
    /// Empty space. `flex` shares leftover width across all flex widgets.
    Spacer {
        #[serde(default = "one")]
        flex: f64,
    },
}

impl WidgetCfg {
    pub fn style(&self) -> Option<Style> {
        match self {
            WidgetCfg::Label {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            }
            | WidgetCfg::Clock {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            }
            | WidgetCfg::Battery {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            }
            | WidgetCfg::Button {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            } => Some(Style {
                color: color.clone(),
                font_size: *font_size,
                bold: *bold,
                bg: bg.clone(),
                align: *align,
            }),
            WidgetCfg::Spacer { .. } => None,
        }
    }

    pub fn width(&self) -> Option<f64> {
        match self {
            WidgetCfg::Label { width, .. }
            | WidgetCfg::Clock { width, .. }
            | WidgetCfg::Battery { width, .. }
            | WidgetCfg::Button { width, .. } => *width,
            WidgetCfg::Spacer { .. } => None,
        }
    }

    /// Mutable handle to style fields, for the editor's benefit.
    pub fn style_mut(&mut self) -> Option<StyleMut<'_>> {
        match self {
            WidgetCfg::Label {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            }
            | WidgetCfg::Clock {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            }
            | WidgetCfg::Battery {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            }
            | WidgetCfg::Button {
                color,
                font_size,
                bold,
                bg,
                align,
                ..
            } => Some(StyleMut {
                color,
                font_size,
                bold,
                bg,
                align,
            }),
            WidgetCfg::Spacer { .. } => None,
        }
    }
}

pub fn load_config(path: &std::path::Path) -> Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Parse a config from raw TOML text — what the daemon's socket receives.
pub fn parse_config(text: &str) -> Result<Config> {
    toml::from_str(text).context("parsing config")
}

/// Where sliverd listens for live config application.
pub fn socket_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/tmp".into());
    base.join("sliver.sock")
}

impl Config {
    /// The shell command bound to a widget, if it has one.
    pub fn action_at(&self, i: usize) -> Option<&str> {
        match self.widgets.get(i) {
            Some(WidgetCfg::Label {
                action: Some(a), ..
            })
            | Some(WidgetCfg::Button {
                action: Some(a), ..
            }) => Some(a.as_str()),
            _ => None,
        }
    }
}

/// The momentary layer shown while the physical Fn/Globe key is held.
pub fn function_row_config() -> Config {
    let widgets = (1..=12)
        .map(|n| WidgetCfg::Button {
            text: format!("F{n}"),
            action: None,
            color: Some("#ffffff".into()),
            width: None,
            font_size: 20.0,
            bold: true,
            bg: Some("#24242b".into()),
            align: Align::Center,
        })
        .collect();
    Config {
        background: "#000000".into(),
        widgets,
    }
}

fn hex(color: &str) -> (f64, f64, f64) {
    let c = color.trim_start_matches('#');
    let p = |i: usize| f64::from(u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0xff)) / 255.0;
    if c.len() == 6 {
        (p(0), p(2), p(4))
    } else {
        (1.0, 1.0, 1.0)
    }
}

/// First Battery-type entry under /sys/class/power_supply, as 0..=100.
fn battery_capacity() -> Option<i64> {
    std::fs::read_dir("/sys/class/power_supply")
        .ok()?
        .filter_map(|e| e.ok())
        .find(|e| {
            std::fs::read_to_string(e.path().join("type"))
                .map(|t| t.trim() == "Battery")
                .unwrap_or(false)
        })
        .and_then(|e| {
            std::fs::read_to_string(e.path().join("capacity"))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
}

/// (x, width) of every widget in strip coordinates — used for painting,
/// hit-testing, and one day the customizer's drag handles.
pub fn layout_rects(cfg: &Config) -> Vec<(f64, f64)> {
    let mut fixed = PAD * 2.0;
    let mut flex_total = 0.0;
    for w in &cfg.widgets {
        match w {
            WidgetCfg::Spacer { flex } => flex_total += flex,
            other => match other.width() {
                Some(px) => fixed += px,
                None => flex_total += 1.0,
            },
        }
        fixed += GAP;
    }
    fixed -= GAP; // no gap after the last widget
    let flex_px = (STRIP_W - fixed).max(0.0) / flex_total.max(1.0);

    let mut rects = Vec::new();
    let mut x = PAD;
    for w in &cfg.widgets {
        let wpx = match w {
            WidgetCfg::Spacer { flex } => flex * flex_px,
            other => other.width().unwrap_or(flex_px),
        };
        rects.push((x, wpx));
        x += wpx + GAP;
    }
    rects
}

/// Which widget owns strip-x, if any. Whole-height hit boxes; the strip
/// is sixty pixels of pure horizontal intent.
pub fn hit(cfg: &Config, x: f64) -> Option<usize> {
    layout_rects(cfg)
        .iter()
        .position(|(rx, rw)| x >= *rx && x < rx + rw)
}

fn widget_text(cfg: &WidgetCfg) -> Option<(String, Option<String>)> {
    match cfg {
        WidgetCfg::Label { text, color, .. } => Some((text.clone(), color.clone())),
        WidgetCfg::Button { text, color, .. } => Some((text.clone(), color.clone())),
        WidgetCfg::Clock { format, color, .. } => Some((
            chrono::Local::now().format(format).to_string(),
            color.clone(),
        )),
        WidgetCfg::Battery { format, color, .. } => {
            let cap = battery_capacity()?;
            let text = format.replace("{capacity}", &cap.to_string());
            let auto = match cap {
                51..=100 => "#88ffcc",
                21..=50 => "#ffcc66",
                _ => "#ff6677",
            };
            Some((text, Some(color.clone().unwrap_or_else(|| auto.into()))))
        }
        WidgetCfg::Spacer { .. } => None,
    }
}

/// Rounded-rectangle path in the current cairo context.
fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
    cr.close_path();
}

/// Render the strip to any cairo surface sized STRIP_W x STRIP_H.
/// `pressed` highlights one widget's rect — the daemon's way of saying
/// "yes, I felt that."
pub fn render(cfg: &Config, cr: &cairo::Context, pressed: Option<usize>) -> Result<()> {
    let (r, g, b) = hex(&cfg.background);
    cr.set_source_rgb(r, g, b);
    cr.paint()?;

    let layout_ctx = pangocairo::functions::create_context(cr);

    for (i, ((x, w), widget)) in layout_rects(cfg).iter().zip(cfg.widgets.iter()).enumerate() {
        let (x, w) = (*x, *w);

        let Some(style) = widget.style() else {
            continue;
        };

        if let Some(c) = &style.bg {
            let (r, g, b) = hex(c);
            cr.set_source_rgb(r, g, b);
            rounded_rect(cr, x + 2.0, 6.0, w - 4.0, STRIP_H - 12.0, 14.0);
            cr.fill()?;
        }

        if pressed == Some(i) {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.18);
            rounded_rect(cr, x + 2.0, 4.0, w - 4.0, STRIP_H - 8.0, 12.0);
            cr.fill()?;
        }

        if let Some((text, color)) = widget_text(widget) {
            let layout = pango::Layout::new(&layout_ctx);
            layout.set_text(&text);
            let desc = pango::FontDescription::from_string(&format!(
                "Sans{} {}",
                if style.bold { " Bold" } else { "" },
                style.font_size as i32
            ));
            layout.set_font_description(Some(&desc));

            let (r, g, b) = hex(color.as_deref().unwrap_or("#ffffff"));
            cr.set_source_rgb(r, g, b);

            let (tw, th) = layout.pixel_size();
            let (tw, th) = (f64::from(tw), f64::from(th));
            let tx = match style.align {
                Align::Left => x + 8.0,
                Align::Center => (x + (w - tw) / 2.0).max(x),
                Align::Right => (x + w - tw - 8.0).max(x),
            };
            let ty = (STRIP_H - th).max(0.0) / 2.0;
            cr.move_to(tx, ty);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }
    Ok(())
}

/// Milestone zero: render straight to a PNG, no hardware required.
pub fn render_preview(cfg: &Config, path: &std::path::Path) -> Result<()> {
    let surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, STRIP_W as i32, STRIP_H as i32)?;
    let cr = cairo::Context::new(&surface)?;
    render(cfg, &cr, None)?;
    let mut f = std::fs::File::create(path)?;
    surface.write_to_png(&mut f)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_configs_get_modern_style_defaults() {
        let cfg = parse_config(
            r##"
            background = "#000000"
            [[widgets]]
            type = "label"
            text = "hello"
            width = 160
            "##,
        )
        .unwrap();

        let WidgetCfg::Label {
            action,
            font_size,
            bold,
            bg,
            align,
            ..
        } = &cfg.widgets[0]
        else {
            panic!("expected label")
        };
        assert_eq!(action, &None);
        assert_eq!(*font_size, 24.0);
        assert!(!bold);
        assert_eq!(bg, &None);
        assert_eq!(*align, Align::Center);
    }

    #[test]
    fn button_round_trip_preserves_action_and_style() {
        let source = r##"
            background = "#101010"
            [[widgets]]
            type = "button"
            text = "QA"
            action = "touch /tmp/fired"
            color = "#ff88aa"
            width = 160
            font_size = 30
            bold = true
            bg = "#3a2233"
            align = "right"
        "##;
        let cfg = parse_config(source).unwrap();
        assert_eq!(cfg.action_at(0), Some("touch /tmp/fired"));

        let encoded = toml::to_string(&cfg).unwrap();
        let decoded = parse_config(&encoded).unwrap();
        assert_eq!(decoded.action_at(0), Some("touch /tmp/fired"));
        let style = decoded.widgets[0].style().unwrap();
        assert_eq!(style.font_size, 30.0);
        assert!(style.bold);
        assert_eq!(style.bg.as_deref(), Some("#3a2233"));
        assert_eq!(style.align, Align::Right);
    }

    #[test]
    fn hit_testing_respects_widget_gaps() {
        let cfg = parse_config(
            r##"
            [[widgets]]
            type = "label"
            text = "first"
            width = 160

            [[widgets]]
            type = "button"
            text = "second"
            width = 160
            "##,
        )
        .unwrap();

        assert_eq!(hit(&cfg, 16.0), Some(0));
        assert_eq!(hit(&cfg, 175.0), Some(0));
        assert_eq!(hit(&cfg, 180.0), None); // eight-pixel inter-widget gap
        assert_eq!(hit(&cfg, 184.0), Some(1));
    }

    #[test]
    fn function_row_has_twelve_evenly_hit_testable_keys() {
        let cfg = function_row_config();
        assert_eq!(cfg.widgets.len(), 12);
        for (i, widget) in cfg.widgets.iter().enumerate() {
            let WidgetCfg::Button { text, action, .. } = widget else {
                panic!("function row contained a non-button")
            };
            assert_eq!(text, &format!("F{}", i + 1));
            assert!(action.is_none());
        }

        for (i, (x, width)) in layout_rects(&cfg).iter().copied().enumerate() {
            assert_eq!(hit(&cfg, x + width / 2.0), Some(i));
        }
    }
}
