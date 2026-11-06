//! Network throughput from `/proc/net/dev`, and the text sparkline.

/// Total received and transmitted bytes over all interfaces except loopback.
pub fn parse_net_dev(s: &str) -> Option<(u64, u64)> {
    let mut any = false;
    let (mut rx, mut tx) = (0u64, 0u64);
    for line in s.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        // Loopback and virtual bridges would double count traffic.
        if name == "lo"
            || name.starts_with("docker")
            || name.starts_with("veth")
            || name.starts_with("br-")
        {
            continue;
        }
        let f: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        if f.len() >= 9 {
            rx = rx.saturating_add(f[0]);
            tx = tx.saturating_add(f[8]);
            any = true;
        }
    }
    any.then_some((rx, tx))
}

/// Bytes per second, down and up.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NetRates {
    pub down: f64,
    pub up: f64,
}

/// `1536.0` → `"1.5KB/s"`. Decimal-free above 100 so it stays narrow; `bits` gives `Kb/s`.
pub fn human_rate(bytes_per_s: f64, bits: bool) -> String {
    let (mut v, unit) = if bits {
        (bytes_per_s * 8.0, 'b')
    } else {
        (bytes_per_s, 'B')
    };
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    let prefix = ["", "K", "M", "G", "T"][i];
    if v >= 100.0 || i == 0 {
        format!("{v:.0}{prefix}{unit}/s")
    } else {
        format!("{v:.1}{prefix}{unit}/s")
    }
}

/// `values` as block characters `▁▂▃▄▅▆▇█`, scaled to `max` (or to the largest value when
/// `None`). Empty input gives an empty string; an all-zero series is a flat floor.
pub fn sparkline(values: &[f64], max: Option<f64>) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let top = max
        .unwrap_or_else(|| values.iter().copied().fold(0.0, f64::max))
        .max(f64::EPSILON);
    values
        .iter()
        .map(|v| {
            let f = (v / top).clamp(0.0, 1.0);
            BARS[((f * 7.0).round() as usize).min(7)]
        })
        .collect()
}
