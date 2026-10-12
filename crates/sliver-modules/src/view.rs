//! From a module's raw [`Output`] to what the bar draws: the module's `format`, `format-icons`,
//! `states` and `hide-empty-text` applied through the shared format engine
//! (`sliver_config::format`), the same one built-in modules use.

use crate::Output;
use sliver_config::{ModuleInstance, State, ValueMap, format_module};

/// A module ready to render.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub text: String,
    /// Image icon: `format-icons` entry if it chose an image, else the output's own `icon`.
    pub icon: Option<String>,
    pub detail: Option<String>,
    /// Output classes plus the active state's, for the stylesheet.
    pub classes: Vec<String>,
    /// 0..=100 slider position for `slider = true` modules.
    pub value: Option<f64>,
    pub hidden: bool,
}

/// Apply `inst`'s formatting to `out`. `show_alt` is the user's `format-alt` toggle.
pub fn view(inst: &ModuleInstance, out: &Output, show_alt: bool) -> View {
    let mut values = ValueMap::new()
        .with("", out.text.as_str())
        .with("text", out.text.as_str());
    if let Some(a) = &out.alt {
        values = values.with("alt", a.as_str());
    }
    if let Some(d) = &out.detail {
        values = values.with("tooltip", d.as_str());
    }
    let st = State {
        percentage: out.percentage,
        alt: out.alt.as_deref(),
        show_alt,
        ..State::default()
    };
    let f = format_module(&inst.common, "{}", &st, &values);
    let mut classes = out.classes.clone();
    for c in f.classes {
        if !classes.contains(&c) {
            classes.push(c);
        }
    }
    View {
        text: f.text,
        icon: f.icon.map(|i| i.0).or_else(|| out.icon.clone()),
        detail: out.detail.clone(),
        classes,
        value: out.value,
        hidden: out.hidden || f.hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(block: &str) -> ModuleInstance {
        let src = format!(
            "[[layouts.default.zones]]\nname = \"main\"\nmodules = [\"custom/x\"]\n[modules.\"custom/x\"]\n{block}\n"
        );
        let loaded = sliver_config::load_str(&src, std::path::Path::new("."));
        let r = loaded
            .config
            .resolve("base", &sliver_config::Context::new())
            .unwrap();
        r.zones
            .into_iter()
            .flat_map(|z| z.modules)
            .find(|m| m.id.kind == "custom")
            .unwrap()
    }

    #[test]
    fn default_format_is_the_text() {
        let v = view(
            &instance("exec = \"x\""),
            &Output {
                text: "hi".into(),
                ..Default::default()
            },
            false,
        );
        assert_eq!(v.text, "hi");
    }

    #[test]
    fn format_states_icons_and_alt() {
        let inst = instance(
            "exec = \"x\"\nformat = \"{icon} {text} {percentage}%\"\nformat-alt = \"{alt}!\"\nformat-icons = [\"a\", \"b\", \"c\"]\nstates = { warning = 70, critical = 90 }",
        );
        let out = Output {
            text: "cpu".into(),
            alt: Some("busy".into()),
            percentage: Some(95.0),
            classes: vec!["mine".into()],
            ..Default::default()
        };
        let v = view(&inst, &out, false);
        assert_eq!(v.text, "c cpu 95%");
        assert_eq!(v.classes, ["mine", "critical"]);
        assert_eq!(view(&inst, &out, true).text, "busy!");
    }

    #[test]
    fn own_icon_and_hidden() {
        let inst = instance("exec = \"x\"\nhide-empty-text = true");
        let out = Output {
            icon: Some("network-vpn".into()),
            ..Default::default()
        };
        let v = view(&inst, &out, false);
        assert_eq!(v.icon.as_deref(), Some("network-vpn"));
        assert!(v.hidden, "no text and hide-empty-text");
        assert!(view(&inst, &Output::hidden(), false).hidden);
    }
}
