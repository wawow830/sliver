//! Config → `sliver_render::Scene` with *placeholder* module content, for the preview when no
//! agent is running.
//!
//! THROWAWAY: the agent's mapping (T24, ash) replaces this once it's a shared lib function, and
//! with a running agent the preview should come from the agent's own render (cedar's Render).
//! Everything else here (zones, ids, classes, widths, the stylesheet) is real.

use std::path::Path;

use sliver_config::format::{State, ValueMap, format_module};
use sliver_config::{Config, ModuleInstance, Resolved, Width as CWidth};
use sliver_icons::{IconLoader, Query};
use sliver_render::{Align, Content, Icon, Item, Pixmap, Renderer, Scene, Stylesheet, Width, Zone};

pub const BAR: (u32, u32) = (2008, 60);

pub struct Preview {
    renderer: Renderer,
    icons: IconLoader,
}

impl Preview {
    pub fn new() -> Self {
        Self {
            renderer: Renderer::new(sliver_render::Fonts::system()),
            icons: IconLoader::from_env(),
        }
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

    pub fn render(&mut self, r: &Resolved, sheet: &Stylesheet) -> Pixmap {
        let scene = self.scene(r);
        self.renderer.render_new(&scene, sheet, BAR.0, BAR.1)
    }

    fn scene(&self, r: &Resolved) -> Scene {
        let mut scene = Scene {
            classes: vec![format!("layer-{}", r.layer), format!("layout-{}", r.layout)],
            zones: Vec::new(),
        };
        for z in &r.zones {
            let mut zone = Zone::new(
                &z.name,
                width(z.width),
                align(z.align),
                z.modules.iter().map(|m| self.item(m)).collect(),
            );
            zone.classes = z.class.clone();
            scene.zones.push(zone);
        }
        scene
    }

    fn item(&self, m: &ModuleInstance) -> Item {
        // Groups preview as their leader (drawers start collapsed).
        if m.id.kind == "group"
            && let Some(first) = m.children.first()
        {
            let mut it = self.item(first);
            it.id = Some(m.css_id.clone());
            return it;
        }
        let content = match m.id.kind.as_str() {
            "spacer" => Content::Spacer,
            "backlight/slider" | "kbd-backlight/slider" | "audio/slider" => {
                let icon = match m.id.kind.as_str() {
                    "audio/slider" => "volume_up",
                    "kbd-backlight/slider" => "backlight_high",
                    _ => "brightness_high",
                };
                Content::Slider {
                    icon: Icon::builtin(icon),
                    value: 0.6,
                }
            }
            "app-controls" => Content::Text("App controls".into()),
            "status-items" => Content::Text("Status".into()),
            _ => {
                let (values, state) = sample(m);
                let f = format_module(&m.common, "{}", &state, &values);
                let icon = f
                    .icon
                    .map(|i| i.0)
                    .or_else(|| m.common.icon.clone())
                    .and_then(|n| self.icon(&n, m.common.icon_theme.as_deref()));
                match (icon, f.text.is_empty()) {
                    (Some(i), true) => Content::Icon(i),
                    (Some(i), false) => Content::IconText(i, f.text),
                    (None, _) if f.text.is_empty() => {
                        Content::Text(m.id.name.clone().unwrap_or_else(|| m.id.kind.clone()))
                    }
                    (None, _) => Content::Text(f.text),
                }
            }
        };
        let mut it = Item::new(content)
            .id(m.css_id.clone())
            .width(width(m.common.width));
        it.classes = m.classes.clone();
        if matches!(m.id.kind.as_str(), "app-controls" | "status-items") {
            it.classes.push("placeholder".into());
        }
        it
    }

    fn icon(&self, name: &str, theme: Option<&str>) -> Option<Icon> {
        if let Some(dfr) = name.strip_prefix("tiny-dfr:") {
            return Icon::builtin(dfr)
                .or_else(|| Icon::load(format!("/usr/share/tiny-dfr/{dfr}.svg")).ok());
        }
        if name.starts_with('/') {
            return Icon::load(name)
                .ok()
                .map(|i| i.symbolic(name.contains("-symbolic")));
        }
        let mut q = Query::new(name, 48);
        if let Some(t) = theme {
            q = q.theme(t);
        }
        if name.ends_with("-symbolic") {
            q = q.symbolic();
        }
        let hit = self.icons.lookup(&q)?;
        Icon::load(&hit.path)
            .ok()
            .map(|i| i.symbolic(name.ends_with("-symbolic")))
    }
}

/// Plausible values so formats render something meaningful in the preview.
fn sample(m: &ModuleInstance) -> (ValueMap, State<'static>) {
    let v = ValueMap::new();
    let st = State::default();
    match m.id.kind.as_str() {
        "clock" => (v.with("", "12:34"), st),
        "battery" => (
            v.with("capacity", 76i64)
                .with("", "76%")
                .with("time", "4:10"),
            State {
                percentage: Some(76.0),
                status: Some("discharging"),
                lower_is_worse: true,
                ..st
            },
        ),
        "mpris" => (
            v.with("title", "Song title")
                .with("artist", "Artist")
                .with("status_icon", "▶")
                .with("", "Song title"),
            State {
                status: Some("playing"),
                ..st
            },
        ),
        "backlight" | "audio" | "kbd-backlight" => (
            v.with("percent", 60i64).with("volume", 60i64),
            State {
                percentage: Some(60.0),
                ..st
            },
        ),
        "workspaces" => (v.with("", "1 2 3"), st),
        "cpu" | "memory" => (
            v.with("usage", 12i64).with("percentage", 12i64),
            State {
                percentage: Some(12.0),
                ..st
            },
        ),
        "custom" if !m.config.contains_key("exec") => (v.with("text", "").with("", ""), st),
        "custom" => (v.with("text", "…").with("", "…"), st),
        "key" => (v.with("", ""), st),
        _ if m.common.icon.is_some() => (v.with("", ""), st),
        _ => (v.with("", m.id.kind.as_str()), st),
    }
}

fn width(w: CWidth) -> Width {
    match w {
        CWidth::Auto => Width::Auto,
        CWidth::Fixed(px) => Width::Fixed(px as f32),
        CWidth::Fill(n) => Width::Fill(n),
    }
}

fn align(a: sliver_config::Align) -> Align {
    match a {
        sliver_config::Align::Start => Align::Start,
        sliver_config::Align::Center => Align::Center,
        sliver_config::Align::End => Align::End,
    }
}
