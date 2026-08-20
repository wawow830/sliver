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
    /// Empty space. `flex` shares leftover width across all flex widgets.
    Spacer { #[serde(default = "one")] flex: f64 },
}

fn one() -> f64 {
    1.0
}
fn default_clock_fmt() -> String {
    "%H:%M".into()
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

struct Placed<'a> {
    cfg: &'a WidgetCfg,
    x: f64,
    w: f64,
}

fn layout(cfg: &Config) -> Vec<Placed<'_>> {
    let mut fixed = PAD * 2.0;
    let mut flex_total = 0.0;
    for w in &cfg.widgets {
        match w {
            WidgetCfg::Label { width, .. } | WidgetCfg::Clock { width, .. } => {
                match width {
                    Some(px) => fixed += px,
                    None => flex_total += 1.0, // text widgets default to flex 1
                }
            }
            WidgetCfg::Spacer { flex } => flex_total += flex,
        }
        fixed += GAP;
    }
    fixed -= GAP; // no gap after the last widget
    let flex_px = (STRIP_W - fixed).max(0.0) / flex_total.max(1.0);

    let mut placed = Vec::new();
    let mut x = PAD;
    for w in &cfg.widgets {
        let wpx = match w {
            WidgetCfg::Label { width: Some(px), .. }
            | WidgetCfg::Clock { width: Some(px), .. } => *px,
            WidgetCfg::Label { .. } | WidgetCfg::Clock { .. } => flex_px,
            WidgetCfg::Spacer { flex } => flex * flex_px,
        };
        placed.push(Placed { cfg: w, x, w: wpx });
        x += wpx + GAP;
    }
    placed
}

fn widget_text(cfg: &WidgetCfg) -> Option<(String, Option<&str>)> {
    match cfg {
        WidgetCfg::Label { text, color, .. } => Some((text.clone(), color.as_deref())),
        WidgetCfg::Clock { format, color, .. } => Some((
            chrono::Local::now().format(format).to_string(),
            color.as_deref(),
        )),
        WidgetCfg::Spacer { .. } => None,
    }
}

/// Render the strip to any cairo surface sized STRIP_W x STRIP_H.
pub fn render(cfg: &Config, cr: &cairo::Context) -> Result<()> {
    let (r, g, b) = hex(&cfg.background);
    cr.set_source_rgb(r, g, b);
    cr.paint()?;

    let layout_ctx = pangocairo::functions::create_context(cr);
    let font = pango::FontDescription::from_string("Sans 24");
    layout_ctx.set_font_description(Some(&font));

    for p in layout(cfg) {
        if let Some((text, color)) = widget_text(p.cfg) {
            let layout = pango::Layout::new(&layout_ctx);
            layout.set_text(&text);

            let (r, g, b) = hex(color.unwrap_or("#ffffff"));
            cr.set_source_rgb(r, g, b);

            let (tw, th) = layout.pixel_size();
            let x = p.x + (p.w - f64::from(tw)).max(0.0) / 2.0;
            let y = (STRIP_H - f64::from(th)).max(0.0) / 2.0;
            cr.move_to(x, y);
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
    render(cfg, &cr)?;
    let mut f = std::fs::File::create(path)?;
    surface.write_to_png(&mut f)?;
    Ok(())
}
