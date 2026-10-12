/// Memory in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mem {
    pub total: u64,
    /// `MemAvailable`: what programs can get without swapping.
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl Mem {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn percent_used(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f64 * 100.0 / self.total as f64
        }
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }

    pub fn swap_percent(&self) -> f64 {
        if self.swap_total == 0 {
            0.0
        } else {
            self.swap_used() as f64 * 100.0 / self.swap_total as f64
        }
    }
}

pub fn parse_meminfo(s: &str) -> Option<Mem> {
    let get = |key: &str| -> Option<u64> {
        let line = s.lines().find(|l| l.starts_with(key))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    };
    Some(Mem {
        total: get("MemTotal:")?,
        available: get("MemAvailable:").or_else(|| get("MemFree:"))?,
        swap_total: get("SwapTotal:").unwrap_or(0),
        swap_free: get("SwapFree:").unwrap_or(0),
    })
}
