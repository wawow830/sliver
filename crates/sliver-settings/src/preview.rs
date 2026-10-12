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

pub const BAR: (u32, u32) = (2008, 60);

pub struct Preview {
    agent: Agent,
    /// The agent slot the last render went to.
    slot: Layer,
}

impl Preview {
    pub fn new(config: Config, sheet: Stylesheet) -> Self {
        let agent = Agent::new(
            config,
            sheet,
            Renderer::new(Fonts::system()),
            Icons::system(),
            BAR.0,
            BAR.1,
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
