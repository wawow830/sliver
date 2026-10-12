//! An isolated private D-Bus for tests (T47).
//!
//! `dbus-daemon --session` loads the user's session config, standard service dirs included, so a
//! test that merely *calls* e.g. `org.freedesktop.Notifications` D-Bus-activates real desktop
//! services (portals, notification daemons) that outlive the test. [`TestBus`] runs the daemon
//! with a minimal config instead: **no service dirs, so nothing can be activated**, and its own
//! directory for the socket.
//!
//! **It can't outlive the test, even on SIGKILL.** `PR_SET_PDEATHSIG` doesn't work for
//! `dbus-daemon` (it changes credentials at startup, which clears the death signal). So the bus
//! runs under `dbus-run-session`, whose program is `cat` reading a pipe from us. When the test
//! process goes away for any reason, the pipe closes, `cat` exits, and `dbus-run-session` takes
//! the daemon down.
//!
//! ```no_run
//! let Some(bus) = sliver_testbus::TestBus::start() else { return }; // skipped without dbus-daemon
//! // zbus::blocking::connection::Builder::address(bus.address())...
//! ```

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// A private `dbus-daemon`. Killed and cleaned up on drop.
pub struct TestBus {
    /// `dbus-run-session`, which owns the daemon.
    child: Child,
    /// Write end of `cat`'s stdin: closing it ends the session.
    lifeline: Option<std::process::ChildStdin>,
    addr: String,
    dir: PathBuf,
}

const CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=@SOCKET@</listen>
  <auth>EXTERNAL</auth>
  <!-- Deliberately no <servicedir>/<standard_session_servicedirs/>: nothing can be activated. -->
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

fn base_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// Make `cmd`'s child get SIGTERM when the spawning thread exits, so it can't outlive the test.
/// Also refuses to start if the parent is already gone.
pub fn die_with_parent(cmd: &mut Command) -> &mut Command {
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: only async-signal-safe libc calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        })
    }
}

impl TestBus {
    /// Start a bus. `None` (with a note on stderr) if `dbus-daemon` isn't installed, so tests
    /// can skip.
    pub fn start() -> Option<TestBus> {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = base_dir().join(format!(
            "sliver-testbus-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).ok()?;
        let conf = dir.join("bus.conf");
        let socket = dir.join("bus.sock");
        std::fs::write(&conf, CONFIG.replace("@SOCKET@", &socket.to_string_lossy())).ok()?;
        let mut cmd = Command::new("dbus-run-session");
        cmd.arg(format!("--config-file={}", conf.display()))
            .args([
                "--",
                "sh",
                "-c",
                "echo \"$DBUS_SESSION_BUS_ADDRESS\"; exec cat >/dev/null",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // No die_with_parent here: a SIGTERM'd dbus-run-session would orphan the daemon. The
        // lifeline pipe is what ends it.
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("sliver-testbus: dbus-run-session unavailable ({e}); skipping");
                let _ = std::fs::remove_dir_all(&dir);
                return None;
            }
        };
        let mut addr = String::new();
        let read = BufReader::new(child.stdout.take()?).read_line(&mut addr);
        if read.is_err() || addr.trim().is_empty() {
            eprintln!("sliver-testbus: dbus-daemon didn't start; skipping");
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        let lifeline = child.stdin.take();
        Some(TestBus {
            child,
            lifeline,
            addr: addr.trim().to_owned(),
            dir,
        })
    }

    /// The bus address, for `zbus::connection::Builder::address` or `DBUS_SESSION_BUS_ADDRESS`.
    pub fn address(&self) -> &str {
        &self.addr
    }

    /// Set `DBUS_SESSION_BUS_ADDRESS` (and nothing else from the user's session) on a command
    /// that should talk to this bus, and tie its lifetime to the test.
    pub fn command(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        c.env("DBUS_SESSION_BUS_ADDRESS", &self.addr);
        die_with_parent(&mut c);
        c
    }
}

impl Drop for TestBus {
    fn drop(&mut self) {
        // Closing the lifeline ends `cat`, and dbus-run-session then stops the daemon and exits.
        drop(self.lifeline.take());
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_and_cleaned_up() {
        let Some(bus) = TestBus::start() else { return };
        let dir = bus.dir.clone();
        let pid = bus.child.id();
        // No service files: activating anything fails at once instead of starting real services.
        let out = bus
            .command("busctl")
            .args([
                "call",
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
            ])
            .args(["org.freedesktop.Notifications", "GetServerInformation"])
            .arg(format!("--address={}", bus.address()))
            .output();
        if let Ok(out) = out {
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(!out.status.success());
            assert!(
                err.contains("not provided by any .service files")
                    || err.contains("ServiceUnknown"),
                "{err}"
            );
        }
        drop(bus);
        assert!(!dir.exists());
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }

    fn alive(addr: &str) -> bool {
        Command::new("busctl")
            .args(["--address", addr, "status"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The test process dies without running destructors: the bus must go anyway.
    #[test]
    fn bus_dies_with_the_test_process() {
        if std::env::var_os("SLIVER_TESTBUS_ORPHAN").is_some() {
            let bus = TestBus::start().unwrap();
            println!("ADDR {}", bus.address());
            std::mem::forget(bus);
            std::process::exit(0); // no Drop, like a crash or SIGKILL
        }
        if Command::new("busctl").arg("--version").output().is_err() || TestBus::start().is_none() {
            return;
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::bus_dies_with_the_test_process",
                "--nocapture",
            ])
            .env("SLIVER_TESTBUS_ORPHAN", "1")
            .stderr(Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let addr = stdout
            .lines()
            .find_map(|l| l.strip_prefix("ADDR "))
            .expect("child printed its bus");
        let end = Instant::now() + Duration::from_secs(5);
        while alive(addr) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(addr), "the bus outlived its test process");
        if let Some(dir) = addr
            .strip_prefix("unix:path=")
            .and_then(|p| std::path::Path::new(p).parent())
        {
            let _ = std::fs::remove_dir_all(dir.to_string_lossy().split(',').next().unwrap());
        }
    }
}
