//! Specs for self-contained widgets: user module output, graphs, images, the clock, emoji, timers.

use super::*;

/// A user module's latest output, as values like any module's: `{text}` (also `{}`), `{alt}`,
/// `{tooltip}` and `{percentage}`, its classes and icon, and `value` as a slider's level. Its
/// text is markup already (`escape = true` escaped it in the runtime).
pub(super) fn custom_output(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let out = &world.outputs[&m.id.to_string()];
    if out.hidden {
        return None;
    }
    let text = FmtValue::Text(out.text.clone());
    let mut v = ValueMap::new().with("", text.clone()).with("text", text);
    if let Some(a) = &out.alt {
        v = v.with("alt", a.as_str());
    }
    if let Some(d) = &out.detail {
        v = v.with("tooltip", d.as_str());
    }
    let mut sp = Spec::new(v, "{}");
    sp.percentage = out.percentage;
    sp.alt = out.alt.clone();
    sp.extra_classes = out.classes.clone();
    sp.default_icon = out.icon.clone();
    sp.level = out
        .value
        .map(|value| (value / 100.0).clamp(0.0, 1.0) as f32);
    Some(sp)
}

/// The agent samples (Agent::render); here only what it has. Hidden until the first one.
pub(super) fn graph(m: &ModuleInstance, world: &World, ui: &UiState) -> Option<Spec> {
    let (samples, cap) = match ui.graph.clone() {
        Some((s, cap)) if s.len() >= 2 || (!world.preview && !s.is_empty()) => (s, cap),
        // Previews (settings, `--render`) have no history yet: a made-up one, so the
        // editor shows what a graph looks like.
        _ if world.preview => {
            let cap = graph_params(m).1;
            let wave = (0..cap)
                .map(|i| {
                    let t = i as f32 / cap.max(2) as f32 * std::f32::consts::TAU * 1.5;
                    0.45 + 0.25 * t.sin() + 0.1 * (t * 2.7).cos()
                })
                .collect();
            (wave, cap)
        }
        _ => return None,
    };
    if samples.is_empty() {
        return None;
    }
    let mut sp = Spec::new(ValueMap::new(), "");
    sp.percentage = samples.last().map(|v| f64::from(*v) * 100.0);
    sp.face = Face::Graph {
        values: samples,
        capacity: cap,
    };
    // Wake for the next sample (never while the bar is off: Agent::next_change).
    // `interval` is a common key, so it's parsed into `common` here.
    let every = match m.common.interval {
        Some(sliver_config::Interval::Every(d)) => d.max(std::time::Duration::from_millis(100)),
        _ => GRAPH_EVERY,
    };
    sp.redraw(world.now.wall, every);
    Some(sp)
}

/// waybar's `cava`: the spectrum the agent's `cava` process reports. Until its first frame every
/// bar rests at 0, so the module keeps its size; previews get a made-up shape.
pub(super) fn cava(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let n = super::cava_bars(m);
    let levels = if world.spectrum.len() == n {
        world.spectrum.clone()
    } else if world.preview {
        (0..n)
            .map(|i| {
                let t = i as f32 / n as f32 * std::f32::consts::PI;
                0.2 + 0.6 * t.sin() * (0.6 + 0.4 * (t * 3.0).cos().abs())
            })
            .collect()
    } else {
        vec![0.0; n]
    };
    let mut sp = Spec::new(ValueMap::new(), "");
    sp.face = Face::Bars(levels);
    Some(sp)
}

/// waybar's `image`: `exec` prints the path (first line), else `path`. Hidden without
/// one, like waybar.
pub(super) fn image(m: &ModuleInstance, world: &World) -> Option<Spec> {
    let path = match world.outputs.get(&m.id.to_string()) {
        Some(out) if out.hidden => return None,
        Some(out) => out.text.lines().next().unwrap_or("").trim().to_owned(),
        None => m.text("path").unwrap_or("").trim().to_owned(),
    };
    if path.is_empty() {
        return None;
    }
    let mut sp = Spec::new(ValueMap::new(), "");
    sp.face = Face::Image(path);
    // A fixed `path` with an `interval` is read again then (a script rewrites the file);
    // with `exec`, the script's runs rebuild the cell.
    if !m.has("exec")
        && let Some(sliver_config::Interval::Every(d)) = m.common.interval
    {
        sp.redraw(world.now.wall, d);
    }
    Some(sp)
}

pub(super) fn clock(at: &At) -> Option<Spec> {
    let now = at.world.now.wall;
    let (clock, zones) = Clock::new(at.m, now, at.ui.tz);
    let mut sp = Spec::new(clock, CLOCK_DEFAULT);
    // Redrawn exactly when what its format shows changes (each second, or minute, or day).
    let format = at.m.common.format.as_deref().unwrap_or(CLOCK_DEFAULT);
    sp.next_change = Some(next_clock_tick(now, format));
    if zones > 1 {
        let step = |by| Act::Ui {
            key: at.key.to_owned(),
            change: UiChange::Zone(by),
        };
        sp = sp.does(Gesture::SwipeLeft, "tz-next", step(1)).does(
            Gesture::SwipeRight,
            "tz-prev",
            step(-1),
        );
    }
    Some(sp)
}

/// The picker's entry button: opens the `emoji` layer (the strip lives there).
pub(super) fn emoji() -> Option<Spec> {
    let open = Action::Layer {
        name: "emoji".into(),
        mode: sliver_config::LayerMode::Toggle,
    };
    Some(Spec::new(ValueMap::new(), "😀").does(Gesture::Tap, "open", Do::Run(open)))
}

pub(super) fn timer(at: &At) -> Option<Spec> {
    let (m, world, ui) = (at.m, at.world, at.ui);
    let total = m
        .duration("duration")
        .unwrap_or(std::time::Duration::from_secs(25 * 60));
    let (left, status) = match &ui.timer {
        None => (total, "idle"),
        Some(t) if t.finished => (std::time::Duration::ZERO, "finished"),
        Some(t) if t.running_since.is_some() => (t.left(world.now.mono_us), "running"),
        Some(t) => (t.remaining, "paused"),
    };
    let pct = 100.0 * (1.0 - left.as_secs_f64() / total.as_secs_f64());
    let v = ValueMap::new()
        .with("remaining", clock_text(left))
        .with("minutes", (left.as_secs() / 60) as i64)
        .with("seconds", (left.as_secs() % 60) as i64)
        .with("", clock_text(left));
    let change = |change| Act::Ui {
        key: at.key.to_owned(),
        change,
    };
    let mut sp = Spec::new(v, "{remaining}")
        .does(Gesture::Tap, "toggle", change(UiChange::Timer { total }))
        .does(Gesture::Hold, "reset", change(UiChange::TimerReset));
    sp.percentage = Some(pct.clamp(0.0, 100.0));
    sp.status = Some(status);
    sp.checked = status == "running";
    if status == "running" {
        sp.redraw(world.now.wall, std::time::Duration::from_secs(1));
    }
    Some(sp)
}
