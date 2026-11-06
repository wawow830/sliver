//! The modules layouts place, read whatever the context: what the module runtime and the
//! watchers need to run.

use super::*;

/// Every module some layout places (group members included, each id once), parsed whatever
/// the context ([`sliver_config::Config::module`]).
pub fn placed_modules(config: &sliver_config::Config) -> Vec<ModuleInstance> {
    use sliver_config::{ModuleId, ModuleRef};
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut todo: Vec<ModuleRef> = Vec::new();
    for name in config.layouts.keys() {
        if let Ok(zones) = config.layout_zones(name) {
            todo.extend(zones.into_iter().flat_map(|z| z.modules));
        }
    }
    while let Some(r) = todo.pop() {
        if !seen.insert(r.id().to_string()) {
            continue;
        }
        let m = config.module(&r);
        if m.id.kind == "group" {
            let members = m.get("modules").and_then(|v| v.as_array());
            todo.extend(
                members
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                    .filter_map(|s| ModuleId::parse(s).ok())
                    .map(ModuleRef::Named),
            );
            continue;
        }
        out.push(m);
    }
    out
}

/// Does some layout place a module of this `kind`? (Sources that cost something to watch,
/// such as failed systemd units, start only then.)
pub fn uses_kind(config: &sliver_config::Config, kind: &str) -> bool {
    placed_modules(config).iter().any(|m| m.id.kind == kind)
}

/// Runtime specs for every `custom/*`, `image` and `graph` module with an `exec`, in any layout
/// (including group members), for the feeds' module runtime. Problems come back as messages.
pub fn user_module_specs(
    config: &sliver_config::Config,
) -> (Vec<crate::scripts::Spec>, Vec<String>) {
    let (mut specs, mut errors) = (Vec::new(), Vec::new());
    for m in placed_modules(config) {
        if !matches!(m.id.kind.as_str(), "custom" | "image" | "graph") || !m.has("exec") {
            continue;
        }
        match crate::scripts::Spec::from_instance(&m) {
            Ok(s) => specs.push(s),
            Err(e) => errors.push(format!("{}: {e}", m.id)),
        }
    }
    specs.sort_by(|a, b| a.key.cmp(&b.key));
    (specs, errors)
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    /// Every module type the config accepts has values or is composed, and nothing else does.
    #[test]
    fn sources_are_the_configs_module_types() {
        // Composed in `cells_in`.
        let composed = ["group", "app", "app-controls", "status-items"];
        let mut ours: Vec<&str> = SOURCES.iter().map(|(k, _)| *k).chain(composed).collect();
        ours.sort();
        let mut config: Vec<&str> = sliver_config::MODULE_TYPES
            .iter()
            .map(|(k, _)| *k)
            .collect();
        config.sort();
        assert_eq!(ours, config);
    }

    #[test]
    fn finds_exec_modules_everywhere() {
        let l = sliver_config::load_str(
            r#"
            [[layouts.default.zones]]
            name = "main"
            modules = ["custom/a", "group/g", "custom/static"]
            [modules."group/g"]
            modules = ["custom/b"]
            [modules."custom/a"]
            exec = "echo a"
            [modules."custom/b"]
            exec = "echo b"
            interval = 5
            [modules."custom/static"]
            format = "hi"
            "#,
            std::path::Path::new("."),
        );
        let (specs, errors) = user_module_specs(&l.config);
        assert!(errors.is_empty(), "{errors:?}");
        let keys: Vec<&str> = specs.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["custom/a", "custom/b"]);
        // `interval` is a common key: the runtime runs it on that period, not as a stream.
        use crate::scripts::Mode;
        assert_eq!(specs[0].mode, Mode::Continuous);
        assert_eq!(
            specs[1].mode,
            Mode::Every(std::time::Duration::from_secs(5))
        );
    }
}
