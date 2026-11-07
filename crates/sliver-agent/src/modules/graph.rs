//! The `graph` module: its samples, taken by the agent while the bar is drawn.

use super::*;

/// `graph` defaults: a sample every 2 s, 30 kept.
pub const GRAPH_EVERY: std::time::Duration = std::time::Duration::from_secs(2);
pub const GRAPH_SAMPLES: usize = 30;

/// A graph's sampling period and history length (`interval`, `samples`).
pub fn graph_params(m: &ModuleInstance) -> (std::time::Duration, usize) {
    let every = match m.common.interval {
        Some(sliver_config::Interval::Every(d)) => d,
        _ => GRAPH_EVERY,
    }
    .max(std::time::Duration::from_millis(100));
    let samples = m.int("samples").map_or(GRAPH_SAMPLES, |n| n as usize);
    (every, samples)
}

/// The value a `graph` plots now, scaled to 0..=1 by `min`/`max` (default 0 and 100): from its
/// own script (`exec`: a number, or JSON `percentage`), or else another module's `percentage`
/// (`source = "cpu"`, `"memory"`, `"battery"`, `"custom/x"`…). `None`: nothing to plot yet.
pub fn graph_sample(
    config: &sliver_config::Config,
    m: &ModuleInstance,
    world: &World,
) -> Option<f32> {
    use sliver_config::ModuleRef;
    let block = m;
    let from_output = |out: &crate::scripts::Output| {
        out.percentage.or_else(|| {
            out.text
                .lines()
                .next()?
                .trim()
                .trim_end_matches('%')
                .trim()
                .parse()
                .ok()
        })
    };
    let raw = if block.has("exec") {
        from_output(world.outputs.get(&m.id.to_string())?)?
    } else {
        let src = block.module_ref("source")?.clone();
        if src.kind == "custom" {
            from_output(world.outputs.get(&src.to_string())?)?
        } else {
            let inst = config.module(&ModuleRef::Named(src));
            let at = At {
                m: &inst,
                key: "",
                world,
                placed: &Placed::default(),
                ui: &UiState::default(),
                item: None,
            };
            spec(&at)?.percentage?
        }
    };
    let num = |k: &str, d: f64| block.number(k).unwrap_or(d);
    let (lo, hi) = (num("min", 0.0), num("max", 100.0));
    if !raw.is_finite() || hi <= lo {
        return None;
    }
    Some(((raw - lo) / (hi - lo)).clamp(0.0, 1.0) as f32)
}
