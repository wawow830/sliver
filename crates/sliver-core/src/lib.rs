//! sliver-core: the widget brain.
//!
//! One renderer, shared by the daemon (real strip) and the customizer
//! (preview), so what you see in the editor is what lands under your finger.

use anyhow::{Context, Result};
use serde::Deserialize;

/// The touchbar's real estate, after rotation.
pub const STRIP_W: f64 = 2008.0;
pub const STRIP_H: f64 = 60.0;

const PAD: f64 = 16.0;
const GAP: f64 = 8.0;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Background color, "#rrggbb".
    #[serde(default = "default_bg")]
    pub background: String,
    pub widgets: Vec<WidgetCfg>,
}

fn default_bg() -> String {
    "#000000".into()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WidgetCfg {
    /// Static text.
    Label {
        text: String,
        #[serde(default)]
        color: Option<String>,
        width: Option<f64>,
    },
    /// A strftime clock. Ticks are the daemon's problem; the core just draws.
    Clock {
        #[serde(default = "default_clock_fmt")]
        format: String,
        #[serde(default)]
        color: Option<String>,
        width: Option<f64>,
    },
    /// Live battery level from /sys/class/power_supply. Colors itself by
    /// level unless told otherwise: mint, amber, red.
    Battery {
        #[serde(default = "default_battery_fmt")]
        format: String,
        #[serde(default)]
        color: Option<String>,
        width: Option<f64>,
    },
    /// Empty space. `flex` shares leftover width across all flex widgets.
    Spacer { #[serde(default = "one")] flex: f64 },
}

fn one() -> f64 {
    1.0
}
fn default_clock_fmt() -> String {
    "%H:%M".into()
}
fn default_battery_fmt() -> String {
    "{capacity}%".into()
}

pub fn load_config(path: &std::path::Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn hex(color: &str) -> (f64, f64, f64) {
    let c = color.trim_start_matches('#');
    let p = |i: usize| f64::from(u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0xff)) / 255.0;
    if c.len() == 6 { (p(0), p(2), p(4)) } else { (1.0, 1.0, 1.0) }
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
            WidgetCfg::Label { width, .. }
            | WidgetCfg::Clock { width, .. }
            | WidgetCfg::Battery { width, .. } => match width {
                Some(px) => fixed += px,
                None => flex_total += 1.0,
            },
            WidgetCfg::Spacer { flex } => flex_total += flex,
        }
        fixed += GAP;
    }
    fixed -= GAP; // no gap after the last widget
    let flex_px = (STRIP_W - fixed).max(0.0) / flex_total.max(1.0);

    let mut rects = Vec::new();
    let mut x = PAD;
    for w in &cfg.widgets {
        let wpx = match w {
            WidgetCfg::Label { width: Some(px), .. }
            | WidgetCfg::Clock { width: Some(px), .. }
            | WidgetCfg::Battery { width: Some(px), .. } => *px,
            WidgetCfg::Label { .. } | WidgetCfg::Clock { .. } | WidgetCfg::Battery { .. } => flex_px,
            WidgetCfg::Spacer { flex } => flex * flex_px,
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

/// Render the strip to any cairo surface sized STRIP_W x STRIP_H.
/// `pressed` highlights one widget's rect — the daemon's way of saying
/// "yes, I felt that."
pub fn render(cfg: &Config, cr: &cairo::Context, pressed: Option<usize>) -> Result<()> {
    let (r, g, b) = hex(&cfg.background);
    cr.set_source_rgb(r, g, b);
    cr.paint()?;

    let layout_ctx = pangocairo::functions::create_context(cr);
    let font = pango::FontDescription::from_string("Sans 24");
    layout_ctx.set_font_description(Some(&font));

    for (i, ((x, w), widget)) in layout_rects(cfg).iter().zip(cfg.widgets.iter()).enumerate() {
        if pressed == Some(i) {
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.18);
            cr.rectangle(x + 2.0, 4.0, w - 4.0, STRIP_H - 8.0);
            cr.fill()?;
        }
        if let Some((text, color)) = widget_text(widget) {
            let layout = pango::Layout::new(&layout_ctx);
            layout.set_text(&text);

            let (r, g, b) = hex(color.as_deref().unwrap_or("#ffffff"));
            cr.set_source_rgb(r, g, b);

            let (tw, th) = layout.pixel_size();
            let tx = x + (w - f64::from(tw)).max(0.0) / 2.0;
            let ty = (STRIP_H - f64::from(th)).max(0.0) / 2.0;
            cr.move_to(tx, ty);
            pangocairo::functions::show_layout(cr, &layout);
        }
    }
    Ok(())
}

/// Milestone zero: render straight to a PNG, no hardware required.
pub fn render_preview(cfg: &Config, path: &std::path::Path) -> Result<()> {
    let surface = cairo::ImageSurface::create(
        cairo::Format::ARgb32,
        STRIP_W as i32,
        STRIP_H as i32,
    )?;
    let cr = cairo::Context::new(&surface)?;
    render(cfg, &cr, None)?;
    let mut f = std::fs::File::create(path)?;
    surface.write_to_png(&mut f)?;
    Ok(())
}
