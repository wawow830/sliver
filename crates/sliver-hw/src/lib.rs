//! Real Touch Bar hardware access: discovery (sysfs), DRM, evdev and backlight helpers.
//!
//! **Never open the real devices outside the safe-swap harness** (docs/safe-swap.md). Code that
//! opens devices must call [`require_harness`] first. Discovery only reads sysfs and is always safe.
//! Hardware facts and per-model quirks: docs/hardware.md.

pub mod discover;
pub mod evdev;
pub mod geom;

use std::fmt;

/// Error for refusing to touch hardware outside the harness.
#[derive(Debug)]
pub struct NotUnderHarness;

impl fmt::Display for NotUnderHarness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "refusing to open Touch Bar devices outside the safe-swap harness \
             (SLIVER_SAFESWAP=1 not set); see docs/safe-swap.md",
        )
    }
}

impl std::error::Error for NotUnderHarness {}

/// Ok only when running inside a safe-swap harness run (or a future sliverd unit that sets it).
pub fn require_harness() -> Result<(), NotUnderHarness> {
    harness_env_ok(std::env::var("SLIVER_SAFESWAP").ok().as_deref())
}

fn harness_env_ok(v: Option<&str>) -> Result<(), NotUnderHarness> {
    match v {
        Some("1") => Ok(()),
        _ => Err(NotUnderHarness),
    }
}

/// Find an fd passed by systemd `OpenFile=`/socket activation by its name (`LISTEN_FDNAMES`).
pub fn listen_fd(name: &str) -> Option<i32> {
    let pid_ok = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
        .is_none_or(|p| p == std::process::id());
    let n: i32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    let names = std::env::var("LISTEN_FDNAMES").unwrap_or_default();
    if !pid_ok {
        return None;
    }
    listen_fd_from(n, &names, name)
}

fn listen_fd_from(n: i32, names: &str, name: &str) -> Option<i32> {
    const SD_LISTEN_FDS_START: i32 = 3;
    names
        .split(':')
        .take(n.max(0) as usize)
        .position(|x| x == name)
        .map(|i| SD_LISTEN_FDS_START + i as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_gate() {
        assert!(harness_env_ok(Some("1")).is_ok());
        assert!(harness_env_ok(None).is_err());
        assert!(harness_env_ok(Some("0")).is_err());
        assert!(harness_env_ok(Some("yes")).is_err());
    }

    #[test]
    fn listen_fds() {
        assert_eq!(listen_fd_from(1, "backlight", "backlight"), Some(3));
        assert_eq!(listen_fd_from(2, "ipc:backlight", "backlight"), Some(4));
        assert_eq!(listen_fd_from(1, "ipc:backlight", "backlight"), None);
        assert_eq!(listen_fd_from(0, "", "backlight"), None);
    }
}
