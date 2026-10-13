//! Real Touch Bar hardware access: discovery (sysfs), DRM, evdev and backlight helpers.
//!
//! **Never open the real devices outside the safe-swap harness** (docs/safe-swap.md) or the
//! installed `sliverd.service`. Code that opens devices must call [`require_harness`] first. Discovery only reads sysfs and is always safe.
//! Hardware facts and per-model quirks: docs/hardware.md.

pub mod backlight;
pub mod discover;
pub mod evdev;
pub mod geom;
pub mod input;
pub mod mt;
pub mod panel;
pub mod uinput;

use std::fmt;

/// Error for refusing to touch hardware outside the harness.
#[derive(Debug)]
pub struct NotUnderHarness;

impl fmt::Display for NotUnderHarness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "refusing to open Touch Bar devices outside the safe-swap harness or the installed \
             sliverd.service (SLIVER_SAFESWAP=1 not set, and not running as the system unit); \
             see docs/safe-swap.md",
        )
    }
}

impl std::error::Error for NotUnderHarness {}

/// Ok only inside a safe-swap harness run (`SLIVER_SAFESWAP=1`) or the packaged
/// `sliverd.service`, which sets `SLIVER_SYSTEM_UNIT=1` (there systemd's OnFailure=tiny-dfr is
/// the safety net). Never set either by hand.
pub fn require_harness() -> Result<(), NotUnderHarness> {
    gate().map(|_| ())
}

/// Which rule lets this process open the bar, for diagnostics (`sliverctl doctor`): the same
/// check as [`require_harness`].
pub fn gate() -> Result<&'static str, NotUnderHarness> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    harness_env_ok(
        std::env::var("SLIVER_SAFESWAP").ok().as_deref(),
        std::env::var("SLIVER_SYSTEM_UNIT").ok().as_deref(),
        &cgroup,
        uid,
    )
}

/// `SLIVER_SYSTEM_UNIT=1` only counts inside the system unit itself, so setting the variable by
/// hand in a dev shell still doesn't grab the bar. Either proves it, and neither is possible
/// without root:
/// - `/proc/self/cgroup` is `0::/system.slice/sliverd.service` (no cgroup namespace), or
/// - a private cgroup namespace (`0::/`, what `ProtectControlGroups=strict` gives on systemd
///   ≥ 257, so this is the real unit's case) *and* a `DynamicUser=` uid (61184–65519).
///   Measured on the installed Mac14,7 (systemd 259): `0::/`, uid 65132. The first version of this
///   check looked only for the unit path and crash-looped the RPM into the tiny-dfr fallback.
fn harness_env_ok(
    safeswap: Option<&str>,
    system_unit: Option<&str>,
    cgroup: &str,
    uid: u32,
) -> Result<&'static str, NotUnderHarness> {
    match (safeswap, system_unit) {
        (Some("1"), _) => Ok("safe-swap harness (SLIVER_SAFESWAP=1)"),
        (_, Some("1")) => in_sliverd_unit(cgroup, uid).ok_or(NotUnderHarness),
        _ => Err(NotUnderHarness),
    }
}

/// systemd's `DynamicUser=` uid range (systemd's `DYNAMIC_UID_MIN..=DYNAMIC_UID_MAX`).
const DYNAMIC_UIDS: std::ops::RangeInclusive<u32> = 61184..=65519;

/// See [`harness_env_ok`]. Not a user unit or anything merely named like the system unit.
fn in_sliverd_unit(cgroup: &str, uid: u32) -> Option<&'static str> {
    cgroup
        .lines()
        .filter_map(|l| l.strip_prefix("0::"))
        .find_map(|path| {
            if path == "/system.slice/sliverd.service" {
                Some("sliverd.service (its cgroup)")
            } else if path == "/" && DYNAMIC_UIDS.contains(&uid) {
                Some("sliverd.service (private cgroup namespace, DynamicUser uid)")
            } else {
                None
            }
        })
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
        const UNIT: &str = "0::/system.slice/sliverd.service\n";
        const SHELL: &str = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope\n";
        // The real installed unit (ProtectControlGroups=strict, DynamicUser), measured: 0::/ + 65132.
        const PRIVATE_NS: &str = "0::/\n";
        const ME: u32 = 1000;
        const DYN: u32 = 65132;
        assert!(harness_env_ok(Some("1"), None, SHELL, ME).is_ok_and(|r| r.contains("safe-swap")));
        assert!(
            harness_env_ok(None, Some("1"), PRIVATE_NS, DYN)
                .is_ok_and(|r| r.contains("DynamicUser"))
        );
        assert!(harness_env_ok(None, Some("1"), UNIT, DYN).is_ok());
        assert!(harness_env_ok(None, Some("1"), PRIVATE_NS, DYN).is_ok());
        // Set by hand outside the unit: refused.
        assert!(harness_env_ok(None, Some("1"), SHELL, ME).is_err());
        assert!(harness_env_ok(None, Some("1"), "", ME).is_err());
        // A private cgroup namespace alone (e.g. unshare -C in a user namespace) isn't enough.
        assert!(harness_env_ok(None, Some("1"), PRIVATE_NS, ME).is_err());
        assert!(harness_env_ok(None, Some("1"), PRIVATE_NS, 0).is_err());
        assert!(harness_env_ok(None, None, UNIT, DYN).is_err());
        assert!(harness_env_ok(None, None, SHELL, ME).is_err());
        assert!(harness_env_ok(Some("0"), None, SHELL, ME).is_err());
        assert!(harness_env_ok(Some("yes"), Some("true"), UNIT, DYN).is_err());
        assert!(in_sliverd_unit("0::/system.slice/sliverd.service.d\n", DYN).is_none());
        assert!(in_sliverd_unit("0::/system.slice/sliver-swap.service\n", DYN).is_none());
        assert!(
            in_sliverd_unit(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/sliverd.service\n",
                DYN
            )
            .is_none()
        );
        assert!(in_sliverd_unit("0::/\n", 61183).is_none());
        assert!(in_sliverd_unit("0::/\n", 65520).is_none());
    }

    #[test]
    fn listen_fds() {
        assert_eq!(listen_fd_from(1, "backlight", "backlight"), Some(3));
        assert_eq!(listen_fd_from(2, "ipc:backlight", "backlight"), Some(4));
        assert_eq!(listen_fd_from(1, "ipc:backlight", "backlight"), None);
        assert_eq!(listen_fd_from(0, "", "backlight"), None);
    }
}
