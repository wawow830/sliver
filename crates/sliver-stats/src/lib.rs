//! `sliver-stats`: the numbers behind the `cpu`, `memory`, `load`, `temperature`, `disk` and
//! network-throughput modules, read from `/proc` and `/sys`.
//!
//! Everything is a plain file read (no polling thread, no subprocess): the agent samples when it
//! renders a frame that shows such a module, and schedules its next render from the module's
//! `interval`, so a bar without these modules, or with the display off, never reads any of it.
//! The file roots are injectable ([`Sampler::with_roots`]) so tests use fixture directories.

mod cpu;
mod disk;
mod mem;
mod net;
mod temp;

pub use cpu::{CpuTimes, parse_cpu_line};
pub use disk::{Disk, disk};
pub use mem::{Mem, parse_meminfo};
pub use net::{NetRates, human_rate, parse_net_dev, sparkline};
pub use temp::{Sensor, sensors};

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A CPU usage reading is a delta between two samples; deltas over less than this are too noisy
/// to show, so the previous value is kept until enough time has passed.
const CPU_WINDOW: Duration = Duration::from_millis(900);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Load {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

pub fn parse_loadavg(s: &str) -> Option<Load> {
    let mut it = s.split_whitespace().map(|v| v.parse::<f64>().ok());
    Some(Load {
        one: it.next()??,
        five: it.next()??,
        fifteen: it.next()??,
    })
}

/// Reads statistics, remembering the last CPU sample for deltas.
#[derive(Debug)]
pub struct Sampler {
    proc_root: PathBuf,
    sys_root: PathBuf,
    prev: Option<(Instant, CpuTimes)>,
    usage: Option<f64>,
    net_prev: Option<(Instant, u64, u64)>,
    net: Option<NetRates>,
    cpu_hist: VecDeque<f64>,
    net_hist: VecDeque<f64>,
}

/// How many samples the sparklines remember.
pub const HISTORY: usize = 16;

fn push(h: &mut VecDeque<f64>, v: f64) {
    if h.len() == HISTORY {
        h.pop_front();
    }
    h.push_back(v);
}

impl Default for Sampler {
    fn default() -> Self {
        Self::with_roots("/proc", "/sys")
    }
}

impl Sampler {
    pub fn with_roots(proc_root: impl Into<PathBuf>, sys_root: impl Into<PathBuf>) -> Sampler {
        Sampler {
            proc_root: proc_root.into(),
            sys_root: sys_root.into(),
            prev: None,
            usage: None,
            net_prev: None,
            net: None,
            cpu_hist: VecDeque::new(),
            net_hist: VecDeque::new(),
        }
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.proc_root.join(rel)).ok()
    }

    /// Whole-machine CPU usage in percent. The first call has no delta yet and reports the
    /// average since boot; after that it is the average over the last second or so.
    pub fn cpu_usage(&mut self, now: Instant) -> Option<f64> {
        let times = parse_cpu_line(&self.read("stat")?)?;
        match self.prev {
            None => {
                self.usage = Some(times.usage_since(CpuTimes::default()));
                push(&mut self.cpu_hist, self.usage.unwrap_or(0.0));
                self.prev = Some((now, times));
            }
            Some((t, old)) if now.saturating_duration_since(t) >= CPU_WINDOW => {
                self.usage = Some(times.usage_since(old));
                push(&mut self.cpu_hist, self.usage.unwrap_or(0.0));
                self.prev = Some((now, times));
            }
            Some(_) => {}
        }
        self.usage
    }

    /// Recent CPU usage samples (percent), oldest first.
    pub fn cpu_history(&self) -> Vec<f64> {
        self.cpu_hist.iter().copied().collect()
    }

    /// Recent download rates (bytes/s), oldest first.
    pub fn net_history(&self) -> Vec<f64> {
        self.net_hist.iter().copied().collect()
    }

    /// Network throughput over the time since the last window. The first call has nothing to
    /// compare with and reports zero; calls closer together than a second keep the last rates.
    pub fn net_rates(&mut self, now: Instant) -> Option<NetRates> {
        let (rx, tx) = parse_net_dev(&self.read("net/dev")?)?;
        match self.net_prev {
            None => {
                self.net = Some(NetRates::default());
                self.net_prev = Some((now, rx, tx));
            }
            Some((t, orx, otx)) if now.saturating_duration_since(t) >= CPU_WINDOW => {
                let secs = now.saturating_duration_since(t).as_secs_f64();
                // Counters reset when an interface goes away: treat as no traffic.
                let r = NetRates {
                    down: rx.saturating_sub(orx) as f64 / secs,
                    up: tx.saturating_sub(otx) as f64 / secs,
                };
                push(&mut self.net_hist, r.down);
                self.net = Some(r);
                self.net_prev = Some((now, rx, tx));
            }
            Some(_) => {}
        }
        self.net
    }

    /// Average current frequency over all cores, in GHz (needs cpufreq; `None` without it).
    pub fn cpu_ghz(&self) -> Option<f64> {
        let dir = self.sys_root.join("devices/system/cpu");
        let mut khz = Vec::new();
        for e in std::fs::read_dir(dir).ok()?.filter_map(Result::ok) {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("cpu")
                && name[3..].chars().all(|c| c.is_ascii_digit())
                && name.len() > 3
            {
                if let Some(v) = std::fs::read_to_string(e.path().join("cpufreq/scaling_cur_freq"))
                    .ok()
                    .and_then(|s| s.trim().parse::<f64>().ok())
                {
                    khz.push(v);
                }
            }
        }
        (!khz.is_empty()).then(|| khz.iter().sum::<f64>() / khz.len() as f64 / 1e6)
    }

    pub fn mem(&self) -> Option<Mem> {
        parse_meminfo(&self.read("meminfo")?)
    }

    pub fn load(&self) -> Option<Load> {
        parse_loadavg(&self.read("loadavg")?)
    }

    /// Hardware sensors (hwmon, then thermal zones), in Celsius.
    pub fn sensors(&self) -> Vec<Sensor> {
        sensors(&self.sys_root)
    }

    pub fn sys_root(&self) -> &Path {
        &self.sys_root
    }
}

#[cfg(test)]
mod tests;
