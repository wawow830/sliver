//! The Bar page preview: the agent's own pipeline (`sliver_agent::Agent`) rendering the user's
//! config for a simulated context. It's the same code path that draws the real bar, so the
//! preview is WYSIWYG (ux.md §1.1).

use std::path::Path;

use chrono::Local;
use sliver_agent::Agent;
use sliver_agent::icons::Icons;
use sliver_agent::modules::World;
use sliver_config::{Config, Context};
use sliver_ipc::proto::Layer;
use sliver_render::{Fonts, Pixmap, Renderer, Stylesheet};

/// The bar's size in pixels: 2008x60 on Macs with a physical Esc key, 2170x60 on those without
/// (docs/hardware.md). The preview renders at the real size, so it shows what the bar will.
pub fn bar_size(hw: sliver_config::Hardware) -> (u32, u32) {
    if hw.physical_esc {
        (2008, 60)
    } else {
        (2170, 60)
    }
}

pub struct Preview {
    agent: Agent,
    /// The agent slot the last render went to.
    slot: Layer,
}

impl Preview {
    pub fn new(config: Config, sheet: Stylesheet, (w, h): (u32, u32)) -> Self {
        let agent = Agent::new(
            config,
            sheet,
            Renderer::new(Fonts::system()),
            Icons::system(),
            w,
            h,
        );
        Self {
            agent,
            slot: Layer::Base,
        }
    }

    /// The user's stylesheet on top of the default theme, plus any problems for the Status page.
    pub fn stylesheet(config: &Config, config_dir: Option<&Path>) -> (Stylesheet, Vec<String>) {
        let mut problems = Vec::new();
        let (mut sheet, warning) = Stylesheet::theme(config.theme.as_deref(), None);
        problems.extend(warning);
        // Like the agent (sliver-reload): `stylesheet = …`, else `style.css` beside the config.
        let path = match (&config.stylesheet, config_dir) {
            (Some(s), Some(d)) => d.join(s),
            (Some(s), None) => s.into(),
            (None, Some(d)) if d.join("style.css").is_file() => d.join("style.css"),
            (None, _) => return (sheet, problems),
        };
        match Stylesheet::load(&path) {
            Ok((user, diags)) => {
                problems.extend(diags.iter().map(|d| d.to_string()));
                sheet.extend(&user);
            }
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
        (sheet, problems)
    }

    /// Renders config layer `layer` for `ctx`. Custom layers are shown the way the agent shows
    /// them: opened over base.
    pub fn render(&mut self, layer: &str, ctx: Context) -> Result<Pixmap, String> {
        self.agent.fixed_ctx = Some(ctx);
        self.agent.layers = match layer {
            "base" | "fn" => Vec::new(),
            custom => vec![custom.to_owned()],
        };
        // Sample values for data modules, and outlined placeholders for empty app slots (T44).
        self.agent.render(&World::sample(Local::now()));
        if let Some(e) = self.agent.errors.first() {
            return Err(e.clone());
        }
        let slot = if layer == "fn" {
            Layer::Fn
        } else {
            Layer::Base
        };
        self.slot = slot;
        self.agent
            .frame(slot)
            .map(|f| f.image.clone())
            .ok_or_else(|| format!("the agent produced no {layer} frame"))
    }

    /// What's under bar x-coordinate `x` (px) in the last render: the zone index and the module
    /// key path (`clock`, `group/control-strip/key/Mute`), plus how many cells before it in the
    /// same zone have the same top-level module (to tell repeated modules apart).
    pub fn hit(&self, x: f32) -> Option<Hit> {
        let f = self.agent.frame(self.slot)?;
        let i = sliver_render::hit_test(&f.layout.items, x)?;
        let rect = f.layout.items.get(i)?;
        let center = rect.x + rect.w / 2.0;
        let zone = f
            .layout
            .zones
            .iter()
            .position(|z| center >= z.x && center <= z.x + z.w)?;
        let in_zone = |j: usize| {
            f.layout.items.get(j).is_some_and(|r| {
                let c = r.x + r.w / 2.0;
                let z = &f.layout.zones[zone];
                c >= z.x && c <= z.x + z.w
            })
        };
        let keys: Vec<String> = (0..f.cells.len())
            .filter(|j| in_zone(*j))
            .map(|j| f.cells[j].key.clone())
            .collect();
        let key = f.cells.get(i)?.key.clone();
        Some(Hit {
            zone,
            key,
            rect: (rect.x, rect.w),
            before: keys,
        })
    }

    /// The cell whose right edge is within a few pixels of bar x-coordinate `x` (the width
    /// handle), if it is a whole top-level module (not inside a group).
    pub fn edge_at(&self, x: f32) -> Option<Hit> {
        const GRAB: f32 = 10.0;
        let f = self.agent.frame(self.slot)?;
        let r = f
            .layout
            .items
            .iter()
            .find(|r| (r.x + r.w - x).abs() <= GRAB)?;
        let h = self.hit(r.x + r.w / 2.0)?;
        // A group's children are cells of one module; their widths are the group's business.
        (!h.key.starts_with("group/")).then_some(h)
    }

    /// Where the caret goes for a drop at bar x-coordinate `x`: after the whole top-level module
    /// left of it (a group's last child, not the middle of the group), or at the zone's start.
    pub fn insertion_x(&self, x: f32) -> Option<f32> {
        let (zone, left) = self.drop_target(x)?;
        let f = self.agent.frame(self.slot)?;
        let z = f.layout.zones[zone];
        let Some(left) = left else {
            return Some(z.x);
        };
        // A group's cells share the `group/<name>` prefix of their key.
        let top = |k: &str| -> String {
            match k.strip_prefix("group/") {
                Some(rest) => format!("group/{}", rest.split('/').next().unwrap_or("")),
                None => k.to_owned(),
            }
        };
        let want = top(&left.key);
        f.layout
            .items
            .iter()
            .zip(&f.cells)
            .filter(|(r, c)| {
                let mid = r.x + r.w / 2.0;
                mid >= z.x && mid <= z.x + z.w && top(&c.key) == want
            })
            .map(|(r, _)| r.x + r.w)
            .reduce(f32::max)
    }

    /// Overflowing zones and too-narrow touch targets in the last render. Only items that react
    /// to touch count (a plain clock isn't a target).
    pub fn feedback(&self) -> Feedback {
        let Some(f) = self.agent.frame(self.slot) else {
            return Feedback::default();
        };
        let overflow = f
            .layout
            .overflow
            .iter()
            .enumerate()
            .filter(|(_, o)| **o)
            .filter_map(|(i, _)| {
                let z = f.layout.zones.get(i)?;
                let name = f
                    .scene
                    .zones
                    .get(i)
                    .and_then(|z| z.name.clone())
                    .unwrap_or_else(|| format!("zone {}", i + 1));
                Some((name, z.x, z.w))
            })
            .collect();
        let narrow = f
            .layout
            .items
            .iter()
            .zip(&f.cells)
            .filter(|(r, c)| {
                r.w > 0.0
                    && r.w < COMFORT_PX
                    && (!matches!(c.touch, sliver_agent::modules::Touch::None)
                        || c.drawer.is_some())
            })
            .map(|(r, c)| (c.key.clone(), (r.x, r.w), r.w < MIN_PX))
            .collect();
        Feedback { overflow, narrow }
    }

    /// Where a drop at bar x-coordinate `x` goes: the zone index, and the cell just left of `x` in
    /// that zone (insert after its module), or `None` to insert first.
    pub fn drop_target(&self, x: f32) -> Option<(usize, Option<Hit>)> {
        let f = self.agent.frame(self.slot)?;
        let zone = f
            .layout
            .zones
            .iter()
            .position(|z| x >= z.x && x <= z.x + z.w)
            .or_else(|| {
                // Between zones: the nearest one.
                f.layout
                    .zones
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        (a.x - x)
                            .abs()
                            .min((a.x + a.w - x).abs())
                            .total_cmp(&(b.x - x).abs().min((b.x + b.w - x).abs()))
                    })
                    .map(|(i, _)| i)
            })?;
        let z = f.layout.zones[zone];
        let left = f
            .layout
            .items
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let c = r.x + r.w / 2.0;
                c >= z.x && c <= z.x + z.w && c < x
            })
            .map(|(i, _)| i)
            .next_back();
        let hit = left.and_then(|i| {
            let r = f.layout.items[i];
            self.hit(r.x + r.w / 2.0)
        });
        Some((zone, hit))
    }
}

/// Touch targets (ux.md §4 "Size limits", from the real panel: 2008 px over the digitizer's
/// 230 mm, ≈ 8.7 px/mm). Narrower items still work; the editor only warns.
pub const COMFORT_PX: f32 = 80.0;
pub const MIN_PX: f32 = 60.0;
pub const PX_PER_MM: f32 = 2008.0 / 230.0;

/// Size problems in the last render (ux.md §4 "Size limits" and "Overflow").
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Feedback {
    /// Zones whose items wanted more room than they got: `(name, x, w)` in bar px.
    pub overflow: Vec<(String, f32, f32)>,
    /// Touchable items narrower than [`COMFORT_PX`]: module key, `(x, w)` in bar px, and
    /// whether it's below [`MIN_PX`] too.
    pub narrow: Vec<(String, (f32, f32), bool)>,
}

impl Feedback {
    /// One line for under the preview, or `None` when all is well.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.overflow.is_empty() {
            let zones: Vec<&str> = self.overflow.iter().map(|(n, ..)| n.as_str()).collect();
            parts.push(format!(
                "{} {} too full: items are squeezed and labels cut short. Set a Fill item to Auto, \
                 remove one, or move some into a group. Saving still works.",
                zones.join(", "),
                if zones.len() == 1 { "is" } else { "are" },
            ));
        }
        // Under COMFORT_PX only gets the amber mark on the preview and the inspector's note: the
        // stock bar has 76-79 px keys (the theme's min-width), and nagging about those in text
        // would make the line meaningless. Words are for what's really hard to hit.
        let strong: Vec<&(String, (f32, f32), bool)> = self.narrow.iter().filter(|n| n.2).collect();
        if !strong.is_empty() {
            // `group/fkeys/key/F1` reads as `key/F1`; at most three names.
            let name = |k: &str| match k.strip_prefix("group/") {
                Some(rest) => rest.split_once('/').map_or(k, |(_, m)| m).to_owned(),
                None => k.to_owned(),
            };
            let mut list: Vec<String> = strong
                .iter()
                .take(3)
                .map(|(k, (_, w), _)| format!("{} ({w:.0} px)", name(k)))
                .collect();
            if strong.len() > 3 {
                list.push(format!("{} more", strong.len() - 3));
            }
            parts.push(format!(
                "{} narrower than {MIN_PX:.0} px (≈{:.0} mm), very hard to hit: {}.",
                if strong.len() == 1 {
                    "1 item is".to_owned()
                } else {
                    format!("{} items are", strong.len())
                },
                MIN_PX / PX_PER_MM,
                list.join(", "),
            ));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

/// The inspector's note for an item `w` bar px wide, if it's small for a finger.
pub fn width_note(w: f32) -> Option<String> {
    let mm = w / PX_PER_MM;
    if w < MIN_PX {
        Some(format!(
            "Only {w:.0} px (≈{mm:.1} mm) wide: very hard to hit. {COMFORT_PX:.0} px or more is comfortable."
        ))
    } else if w < COMFORT_PX {
        Some(format!(
            "{w:.0} px (≈{mm:.1} mm) wide: a bit small to hit reliably. {COMFORT_PX:.0} px or more is comfortable."
        ))
    } else {
        None
    }
}

/// See [`Preview::hit`].
pub struct Hit {
    pub zone: usize,
    pub key: String,
    /// The cell's left edge and width, in bar pixels.
    pub rect: (f32, f32),
    before: Vec<String>,
}

impl Hit {
    /// The index of the clicked module among `zone_ids` (the zone's top-level module ids, in order).
    pub fn module_index(&self, zone_ids: &[String]) -> Option<usize> {
        let top = |key: &str| {
            zone_ids
                .iter()
                .find(|id| key == id.as_str() || key.starts_with(&format!("{id}/")))
                .cloned()
        };
        // Count occurrences of each top-level id as we walk the zone's cells. A group's children
        // are consecutive cells that belong to one occurrence.
        let mut count: std::collections::HashMap<String, usize> = Default::default();
        let mut prev: Option<String> = None;
        for k in &self.before {
            let t = top(k)?;
            let group_child = *k != t;
            if !(group_child && prev.as_ref() == Some(&t)) {
                *count.entry(t.clone()).or_default() += 1;
            }
            if *k == self.key {
                let nth = count[&t] - 1;
                return zone_ids
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| **id == t)
                    .map(|(i, _)| i)
                    .nth(nth);
            }
            prev = Some(t);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(toml: &str) -> Preview {
        let loaded = sliver_config::load_str(toml, Path::new("."));
        assert!(loaded.diagnostics.ok(), "{:?}", loaded.diagnostics);
        let mut p = Preview::new(loaded.config, Stylesheet::sliver(), (2008, 60));
        p.render("base", Context::new()).unwrap();
        p
    }

    #[test]
    fn narrow_touch_targets_only() {
        let p = preview(
            r#"
            [[layouts.default.zones]]
            name = "main"
            width = "fill"
            modules = ["custom/tiny", "custom/small", "custom/fine", "clock"]
            [modules."custom/tiny"]
            format = "T"
            width = 50
            on-tap = "true"
            [modules."custom/small"]
            format = "S"
            width = 70
            on-tap = "true"
            [modules."custom/fine"]
            format = "F"
            width = 120
            on-tap = "true"
            [modules.clock]
            width = 40
            "#,
        );
        let fb = p.feedback();
        let got: Vec<(&str, bool)> = fb.narrow.iter().map(|(k, _, s)| (k.as_str(), *s)).collect();
        assert_eq!(
            got,
            [("custom/tiny", true), ("custom/small", false)],
            "{fb:?}"
        );
        assert!(fb.overflow.is_empty());
        let s = fb.summary().unwrap();
        assert!(
            s.contains("1 item is narrower than 60 px") && s.contains("custom/tiny"),
            "{s}"
        );
        assert!(
            !s.contains("custom/small"),
            "under 80 px is only marked, not listed: {s}"
        );
    }

    #[test]
    fn overflowing_zone() {
        // 12 x 200 px fixed: squeezed to ~140 px each. (Fixed widths used to be counted by their
        // content only, so this wasn't reported.)
        let mods: Vec<String> = (0..12).map(|i| format!("\"custom/m{i}\"")).collect();
        let blocks: String = (0..12)
            .map(|i| format!("[modules.\"custom/m{i}\"]\nformat = \"item {i}\"\nwidth = 200\non-tap = \"true\"\n"))
            .collect();
        let p = preview(&format!(
            "[[layouts.default.zones]]\nname = \"main\"\nwidth = \"fill\"\nmodules = [{}]\n{blocks}",
            mods.join(", ")
        ));
        let fb = p.feedback();
        assert_eq!(fb.overflow.len(), 1, "{fb:?}");
        assert_eq!(fb.overflow[0].0, "main");
        assert!(fb.summary().unwrap().starts_with("main is too full"));
    }

    #[test]
    fn the_stock_bar_is_quiet() {
        // A 2008 px bar is a Mac with a physical Esc (the preview's size).
        let loaded = sliver_config::load_str("", Path::new("."));
        let mut p = Preview::new(loaded.config, Stylesheet::sliver(), (2008, 60));
        p.render("base", Context::new().with("physical_esc", true))
            .unwrap();
        let fb = p.feedback();
        assert!(fb.overflow.is_empty(), "{fb:?}");
        assert_eq!(fb.summary(), None, "{fb:?}");
    }

    #[test]
    fn width_notes() {
        assert_eq!(width_note(120.0), None);
        assert!(width_note(70.0).unwrap().contains("a bit small"));
        assert!(width_note(40.0).unwrap().contains("very hard"));
        assert!(
            width_note(70.0).unwrap().contains("≈8.0 mm"),
            "{:?}",
            width_note(70.0)
        );
    }
}
