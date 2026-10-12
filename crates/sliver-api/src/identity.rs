//! Who is a D-Bus client, really? Verified from `/proc`, never from what the client claims.
//!
//! Used for two things (`docs/api.md` §4): binding `app` contexts to the focused window (same pid,
//! same Flatpak instance or same systemd app unit) and the sandbox tier.

use std::fs;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcIdentity {
    /// Flatpak `(app id, instance id)` from `/proc/<pid>/root/.flatpak-info`.
    pub flatpak: Option<(String, String)>,
    /// Snap name from the cgroup (`snap.<name>.<app>-….scope`).
    pub snap: Option<String>,
    /// Per-app systemd unit (`app-…​.scope`/`app-…​.service`), if the launcher made one.
    pub app_unit: Option<String>,
}

impl ProcIdentity {
    pub fn sandboxed(&self) -> bool {
        self.flatpak.is_some() || self.snap.is_some()
    }

    /// The app id we can vouch for (sandboxed apps only).
    pub fn verified_app_id(&self) -> Option<&str> {
        self.flatpak
            .as_ref()
            .map(|f| f.0.as_str())
            .or(self.snap.as_deref())
    }

    /// Same running app instance (not just the same app).
    pub fn same_instance(&self, other: &ProcIdentity) -> bool {
        match (&self.flatpak, &other.flatpak) {
            (Some(a), Some(b)) => return a.1 == b.1,
            (Some(_), None) | (None, Some(_)) => return false,
            _ => {}
        }
        matches!((&self.app_unit, &other.app_unit), (Some(a), Some(b)) if a == b)
    }
}

/// Process lookups, faked in tests.
pub trait ProcInfo: Send + Sync {
    fn identity(&self, pid: u32) -> ProcIdentity;
    fn parent(&self, pid: u32) -> Option<u32>;

    /// Whether `ancestor` is a (transitive) parent of `pid`. Bounded walk.
    fn is_ancestor(&self, ancestor: u32, pid: u32) -> bool {
        let mut p = pid;
        for _ in 0..64 {
            match self.parent(p) {
                Some(q) if q == ancestor => return true,
                Some(q) if q > 1 && q != p => p = q,
                _ => return false,
            }
        }
        false
    }
}

/// Reads `/proc`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealProc;

impl ProcInfo for RealProc {
    fn identity(&self, pid: u32) -> ProcIdentity {
        let flatpak = fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info"))
            .ok()
            .and_then(|s| parse_flatpak_info(&s));
        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        let (snap, app_unit) = parse_cgroup(&cgroup);
        ProcIdentity {
            flatpak,
            snap,
            app_unit,
        }
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        parse_stat_ppid(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }
}

pub fn parse_flatpak_info(s: &str) -> Option<(String, String)> {
    let (mut section, mut name, mut instance) = ("", None, None);
    for line in s.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if let Some((k, v)) = line.split_once('=') {
            match (section, k.trim()) {
                ("[Application]", "name") => name = Some(v.trim().to_owned()),
                ("[Instance]", "instance-id") => instance = Some(v.trim().to_owned()),
                _ => {}
            }
        }
    }
    Some((name?, instance.unwrap_or_default()))
}

/// `(snap name, app unit)` from `/proc/<pid>/cgroup` (cgroup v2 line `0::/path`).
pub fn parse_cgroup(s: &str) -> (Option<String>, Option<String>) {
    let Some(path) = s.lines().find_map(|l| l.strip_prefix("0::")) else {
        return (None, None);
    };
    let leaf = path.rsplit('/').next().unwrap_or("");
    let snap = leaf
        .strip_prefix("snap.")
        .and_then(|r| r.split('.').next())
        .map(str::to_owned);
    let unit = (leaf.starts_with("app-")
        && (leaf.ends_with(".scope") || leaf.ends_with(".service")))
    .then(|| leaf.to_owned());
    (snap, unit)
}

pub fn parse_stat_ppid(stat: &str) -> Option<u32> {
    // `pid (comm) state ppid …`. comm may contain spaces and parens, so split at the last ')'.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Fake process table: pid -> (parent, identity).
    #[derive(Default)]
    pub struct FakeProc(pub HashMap<u32, (u32, ProcIdentity)>);

    impl FakeProc {
        pub fn add(&mut self, pid: u32, parent: u32, id: ProcIdentity) -> &mut Self {
            self.0.insert(pid, (parent, id));
            self
        }
    }

    impl ProcInfo for FakeProc {
        fn identity(&self, pid: u32) -> ProcIdentity {
            self.0.get(&pid).map(|x| x.1.clone()).unwrap_or_default()
        }
        fn parent(&self, pid: u32) -> Option<u32> {
            self.0.get(&pid).map(|x| x.0)
        }
    }

    #[test]
    fn parsers() {
        let info = "[Application]\nname=org.gimp.GIMP\nruntime=x\n\n[Instance]\ninstance-id=314\n";
        assert_eq!(
            parse_flatpak_info(info),
            Some(("org.gimp.GIMP".into(), "314".into()))
        );
        let cg = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-firefox-4242.scope\n";
        assert_eq!(
            parse_cgroup(cg),
            (None, Some("app-gnome-firefox-4242.scope".into()))
        );
        let cg = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/snap.firefox.firefox-1b2c.scope";
        assert_eq!(parse_cgroup(cg).0.as_deref(), Some("firefox"));
        assert_eq!(
            parse_cgroup("0::/user.slice/user-1000.slice/session-2.scope"),
            (None, None)
        );
        assert_eq!(
            parse_stat_ppid("1234 (weird ) name) S 77 1234 1234 0"),
            Some(77)
        );
    }

    #[test]
    fn real_proc_smoke() {
        let me = std::process::id();
        let id = RealProc.identity(me);
        assert!(!id.sandboxed() || std::path::Path::new("/.flatpak-info").exists());
        let parent = RealProc.parent(me).unwrap();
        assert!(RealProc.is_ancestor(parent, me));
        assert!(!RealProc.is_ancestor(me, parent));
    }

    #[test]
    fn instances() {
        let fp = |i: &str| ProcIdentity {
            flatpak: Some(("a".into(), i.into())),
            ..Default::default()
        };
        let unit = |u: &str| ProcIdentity {
            app_unit: Some(u.into()),
            ..Default::default()
        };
        assert!(fp("1").same_instance(&fp("1")));
        assert!(!fp("1").same_instance(&fp("2")));
        assert!(unit("app-x.scope").same_instance(&unit("app-x.scope")));
        assert!(!unit("app-x.scope").same_instance(&fp("1")));
        assert!(!ProcIdentity::default().same_instance(&ProcIdentity::default()));
    }
}
