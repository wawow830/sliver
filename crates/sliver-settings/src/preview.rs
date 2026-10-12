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
        Self { agent }
    }

    /// The user's stylesheet on top of the default theme, plus any problems for the Status page.
    pub fn stylesheet(config: &Config, config_dir: Option<&Path>) -> (Stylesheet, Vec<String>) {
        let mut sheet = Stylesheet::sliver();
        let mut problems = Vec::new();
        let Some(name) = &config.stylesheet else {
            return (sheet, problems);
        };
        let path = config_dir
            .map(|d| d.join(name))
            .unwrap_or_else(|| name.into());
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
        self.agent.ctx = ctx;
        self.agent.layers = match layer {
            "base" | "fn" => Vec::new(),
            custom => vec![custom.to_owned()],
        };
        self.agent.render(&World { now: Local::now() });
        if let Some(e) = self.agent.errors.first() {
            return Err(e.clone());
        }
        let slot = if layer == "fn" {
            Layer::Fn
        } else {
            Layer::Base
        };
        self.agent
            .frame(slot)
            .map(|f| f.image.clone())
            .ok_or_else(|| format!("the agent produced no {layer} frame"))
    }
}
