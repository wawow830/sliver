/// Cumulative CPU jiffies from the first line of `/proc/stat`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub idle: u64,
}

impl CpuTimes {
    /// Percent busy between `old` and `self`; 0 if no time passed.
    pub fn usage_since(self, old: CpuTimes) -> f64 {
        let busy = self.busy.saturating_sub(old.busy);
        let idle = self.idle.saturating_sub(old.idle);
        let total = busy + idle;
        if total == 0 {
            0.0
        } else {
            busy as f64 * 100.0 / total as f64
        }
    }
}

/// Parse the aggregate `cpu ` line: `cpu user nice system idle iowait irq softirq steal guest guest_nice`.
/// `guest` time is already counted in `user`, so only the first eight numbers are summed.
pub fn parse_cpu_line(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let n: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .filter_map(|v| v.parse().ok())
        .collect();
    if n.len() < 4 {
        return None;
    }
    let idle = n[3] + n.get(4).copied().unwrap_or(0);
    let total: u64 = n.iter().sum();
    Some(CpuTimes {
        busy: total - idle,
        idle,
    })
}
