//! Debug logging for harness runs (`SLIVER_LOG=debug` or `-v`), so a real-hardware run
//! verifies itself from its log: touches, keys, Fn/lid, layer switches, flushes, backlight writes,
//! agent connects and messages. Lines go to stderr (the harness's `sliverd.log`, the journal in
//! production). Off by default: no cost beyond one branch.
//!
//! Implemented as decorators around the backends plus hooks in the event loop, so the core stays
//! free of logging.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sliver_render::Pixmap;
use sliver_render::panel::Span;

use crate::backend::{Backends, Backlight, Display, Keys};

static ENABLED: AtomicBool = AtomicBool::new(false);
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn enable() {
    START.get_or_init(Instant::now);
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Whether `SLIVER_LOG` asks for debug output.
pub fn from_env() -> bool {
    std::env::var("SLIVER_LOG").is_ok_and(|v| matches!(v.as_str(), "debug" | "trace" | "1"))
}

pub fn line(args: std::fmt::Arguments<'_>) {
    if enabled() {
        let t = START.get_or_init(Instant::now).elapsed();
        eprintln!("[{:>4}.{:03}] {args}", t.as_secs(), t.subsec_millis());
    }
}

#[macro_export]
macro_rules! debug {
    ($($t:tt)*) => {
        if $crate::log::enabled() {
            $crate::log::line(format_args!($($t)*));
        }
    };
}

struct LogDisplay(Box<dyn Display>);
struct LogBacklight(Box<dyn Backlight>);
struct LogKeys(Box<dyn Keys>);

impl Display for LogDisplay {
    fn flush(&mut self, frame: &Pixmap, damage: &[Span]) {
        let cols: u32 = damage.iter().map(|s| s.w).sum();
        let t = Instant::now();
        self.0.flush(frame, damage);
        crate::debug!(
            "flush {} span(s), {cols} cols, {:.1} ms",
            damage.len(),
            t.elapsed().as_secs_f32() * 1000.0
        );
    }
}

impl Backlight for LogBacklight {
    fn set(&mut self, value: u32) {
        crate::debug!("backlight {value}");
        self.0.set(value);
    }
    fn screen_fraction(&mut self) -> Option<f32> {
        self.0.screen_fraction()
    }
}

impl Keys for LogKeys {
    fn key(&mut self, code: u16, down: bool) {
        crate::debug!("key {code} {}", if down { "down" } else { "up" });
        self.0.key(code, down);
    }
    fn sync(&mut self) {
        self.0.sync();
    }
}

/// Wraps the backends in logging decorators.
pub fn wrap(b: Backends) -> Backends {
    Backends {
        display: Box::new(LogDisplay(b.display)),
        backlight: Box::new(LogBacklight(b.backlight)),
        keys: Box::new(LogKeys(b.keys)),
    }
}
