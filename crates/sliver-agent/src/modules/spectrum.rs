//! Modules that plot live data: `graph` samples and the `cava` spectrum, which `spectrum = true`
//! also draws under any module.

use super::*;

/// Is this a `graph` module kind (`graph`, waybar-style `custom-graph/<name>`)?
pub fn is_graph(kind: &str) -> bool {
    matches!(kind, "graph" | "custom-graph")
}

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
    let samples = m
        .config
        .get("samples")
        .and_then(|v| v.as_integer())
        .map_or(GRAPH_SAMPLES, |n| n.clamp(2, 600) as usize);
    (every, samples)
}

/// `cava` defaults: 20 bars at 60 frames a second, the panel's refresh and the most the bar
/// draws while animating.
pub const CAVA_BARS: usize = 20;
pub const CAVA_FPS: i64 = 60;

/// How many bars a `cava` module draws (`bars`, 1–128).
pub fn cava_bars(block: &toml::Table) -> usize {
    block
        .get("bars")
        .and_then(|v| v.as_integer())
        .map_or(CAVA_BARS, |n| n.clamp(1, 128) as usize)
}

/// The config `cava` runs with for this module: waybar's `cava` keys in cava's own sections,
/// and raw output (one line of `bars` numbers, 0–1000, per frame) that the agent reads. Keys of
/// the wrong type, and strings that would break out of their line, are left out.
pub fn cava_config(block: &toml::Table) -> String {
    const KEYS: &[(&str, &str, &str)] = &[
        ("general", "autosens", "autosens"),
        ("general", "sensitivity", "sensitivity"),
        ("general", "lower_cutoff_freq", "lower_cutoff_freq"),
        ("general", "higher_cutoff_freq", "higher_cutoff_freq"),
        ("input", "method", "method"),
        ("input", "source", "source"),
        ("output", "reverse", "reverse"),
        ("smoothing", "monstercat", "monstercat"),
        ("smoothing", "waves", "waves"),
        ("smoothing", "noise_reduction", "noise_reduction"),
    ];
    let value = |v: &toml::Value| match v {
        toml::Value::Boolean(b) => Some(u8::from(*b).to_string()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::String(s) if !s.contains(['\n', '\r', '[', ';', '#']) => Some(s.clone()),
        _ => None,
    };
    let fps = block
        .get("framerate")
        .and_then(|v| v.as_integer())
        .map_or(CAVA_FPS, |n| n.clamp(1, CAVA_FPS));
    // A second of silence puts cava to sleep: no frames, and no work, until sound comes back.
    let sleep = block
        .get("sleep_timer")
        .and_then(|v| v.as_integer())
        .unwrap_or(1);
    let stereo = block
        .get("stereo")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut sections: Vec<(&str, Vec<String>)> = vec![
        (
            "general",
            vec![
                format!("bars = {}", cava_bars(block)),
                format!("framerate = {fps}"),
                format!("sleep_timer = {sleep}"),
            ],
        ),
        ("input", Vec::new()),
        (
            "output",
            vec![
                "method = raw".into(),
                "raw_target = /dev/stdout".into(),
                "data_format = ascii".into(),
                "ascii_max_range = 1000".into(),
                "bar_delimiter = 32".into(),
                "frame_delimiter = 10".into(),
                format!("channels = {}", if stereo { "stereo" } else { "mono" }),
            ],
        ),
        ("smoothing", Vec::new()),
    ];
    for (section, key, cava_key) in KEYS {
        if let Some(v) = block.get(*key).and_then(value)
            && let Some((_, lines)) = sections.iter_mut().find(|(s, _)| s == section)
        {
            lines.push(format!("{cava_key} = {v}"));
        }
    }
    sections
        .iter()
        .map(|(s, lines)| format!("[{s}]\n{}\n", lines.join("\n")))
        .collect()
}

/// `spectrum = true`: the live spectrum under the module's content (`cava`'s keys apply), at
/// rest until cava's first frame. Empty: none.
pub(super) fn spectrum(m: &ModuleInstance, world: &World) -> Vec<f32> {
    if m.config.get("spectrum").and_then(|v| v.as_bool()) != Some(true) {
        return Vec::new();
    }
    let n = cava_bars(&m.config);
    if world.spectrum.len() == n {
        world.spectrum.clone()
    } else {
        vec![0.0; n]
    }
}

/// Does this cell show the spectrum (a `cava` module, or any with `spectrum = true`)?
pub fn shows_spectrum(c: &Cell) -> bool {
    c.kind == "cava" || !c.look.spectrum.is_empty()
}

/// The value a `graph` plots now, scaled to 0..=1 by `min`/`max` (default 0 and 100): from its
/// own script (`exec`: a number, or JSON `percentage`), or else another module's `percentage`
/// (`source = "cpu"`, `"memory"`, `"battery"`, `"custom/x"`…). `None`: nothing to plot yet.
pub fn graph_sample(
    config: &sliver_config::Config,
    m: &ModuleInstance,
    world: &World,
) -> Option<f32> {
    use sliver_config::{ModuleId, ModuleRef};
    let block = &m.config;
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
    let raw = if block.contains_key("exec") {
        from_output(world.outputs.get(&m.id.to_string())?)?
    } else {
        let src = ModuleId::parse(block.get("source")?.as_str()?).ok()?;
        if src.kind == "custom" {
            from_output(world.outputs.get(&src.to_string())?)?
        } else {
            let inst = config.module(&ModuleRef::Named(src));
            let at = At {
                m: &inst,
                key: "",
                world,
                ui: &UiState::default(),
                item: None,
            };
            spec(&at)?.percentage?
        }
    };
    let num = |k: &str, d: f64| {
        block
            .get(k)
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(d)
    };
    let (lo, hi) = (num("min", 0.0), num("max", 100.0));
    if !raw.is_finite() || hi <= lo {
        return None;
    }
    Some(((raw - lo) / (hi - lo)).clamp(0.0, 1.0) as f32)
}
