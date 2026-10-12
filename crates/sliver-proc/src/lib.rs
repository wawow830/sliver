//! Child processes that die with sliver (T47).
//!
//! Long-lived helpers (`pactl subscribe`, exec modules, test `dbus-daemon`s) must not outlive
//! the process that started them, whether it exits cleanly, panics, or is SIGKILLed.
//!
//! - [`spawn_tied`] sets `PR_SET_PDEATHSIG(SIGTERM)` in the child. Careful: Linux sends that
//!   signal when the **thread** that forked the child exits, not the process. So the fork happens
//!   on one spawner thread that lives as long as the process, and children spawned from short-lived
//!   threads (a `#[test]`, a worker) still only die with the process. A race check catches a
//!   parent that died between fork and prctl.
//! - [`ChildGuard`] kills (SIGTERM to its process group if it has one, then SIGKILL) and reaps
//!   on drop, for children that should end with their owner object.
//!
//! PDEATHSIG covers the direct child only, and it survives `exec` of a non-setuid program. The
//! kernel clears it whenever the child changes credentials: setuid/setcap exec, or a daemon that
//! drops privileges, like `dbus-daemon` (cedar's `sliver-testbus` uses a pipe to
//! `dbus-run-session` instead).
//! Grandchildren (`sh -c 'a | b'`) need the child's process group killed ([`ChildGuard::group`]).
//! In production, systemd's cgroup does the rest when a unit stops.

use std::io;
use std::ops::{Deref, DerefMut};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

use rustix::process::{Pid, Signal};

type Job = (Command, Sender<io::Result<Child>>);

fn spawner() -> &'static Mutex<Sender<Job>> {
    static SPAWNER: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    SPAWNER.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("sliver-proc-spawner".into())
            .spawn(move || {
                // Lives until the process ends: the static Sender is never dropped.
                for (mut cmd, reply) in rx {
                    let _ = reply.send(cmd.spawn());
                }
            })
            .expect("spawn sliver-proc spawner thread");
        Mutex::new(tx)
    })
}

/// Spawns `cmd` so that the child gets SIGTERM when this process dies (see the module docs).
pub fn spawn_tied(mut cmd: Command) -> io::Result<Child> {
    let parent = rustix::process::getpid();
    // SAFETY: the closure only makes async-signal-safe raw syscalls (prctl, getppid) between
    // fork and exec, and allocates nothing.
    unsafe {
        cmd.pre_exec(move || {
            rustix::process::set_parent_process_death_signal(Some(Signal::TERM))?;
            // The parent may have died before prctl took effect: then we'd never get the signal.
            if rustix::process::getppid() != Some(parent) {
                return Err(io::Error::from_raw_os_error(
                    rustix::io::Errno::SRCH.raw_os_error(),
                ));
            }
            Ok(())
        });
    }
    let (tx, rx) = channel();
    spawner()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .send((cmd, tx))
        .map_err(|_| io::Error::other("sliver-proc spawner thread is gone"))?;
    rx.recv()
        .map_err(|_| io::Error::other("sliver-proc spawner thread is gone"))?
}

/// A child that is killed and reaped when the guard is dropped.
#[derive(Debug)]
pub struct ChildGuard {
    child: Option<Child>,
    group: bool,
}

impl ChildGuard {
    pub fn new(child: Child) -> ChildGuard {
        ChildGuard {
            child: Some(child),
            group: false,
        }
    }

    /// The child leads its own process group (`Command::process_group(0)`): on drop the whole
    /// group gets SIGTERM first, so grandchildren go too.
    pub fn group(child: Child) -> ChildGuard {
        ChildGuard {
            child: Some(child),
            group: true,
        }
    }

    /// Gives up ownership: the child is no longer killed on drop.
    pub fn into_inner(mut self) -> Child {
        self.child.take().expect("present until drop")
    }

    /// Kills (group first, if any) and reaps now. Idempotent.
    pub fn kill(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if self.group {
            if let Some(pid) = Pid::from_raw(child.id() as i32) {
                let _ = rustix::process::kill_process_group(pid, Signal::TERM);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        self.child.as_ref().expect("present until drop")
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("present until drop")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        // /proc/<pid> exists and isn't a zombie.
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.split(") ").nth(1).is_some_and(|r| r.starts_with('Z')))
            .unwrap_or(false)
    }

    fn wait_dead(pid: u32) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn guard_kills_and_reaps() {
        let mut c = Command::new("sleep");
        c.arg("60");
        let g = ChildGuard::new(spawn_tied(c).unwrap());
        let pid = g.id();
        assert!(alive(pid));
        drop(g);
        assert!(!alive(pid), "killed and reaped on drop");
    }

    #[test]
    fn group_guard_kills_grandchildren() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 60 & echo $!; wait"])
            .stdout(Stdio::piped())
            .process_group(0);
        let mut g = ChildGuard::group(spawn_tied(c).unwrap());
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(g.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let grandchild: u32 = line.trim().parse().unwrap();
        assert!(alive(grandchild));
        drop(g);
        assert!(wait_dead(grandchild), "grandchild {grandchild} survived");
    }

    /// The point of the spawner thread: a child spawned from a short-lived thread survives that
    /// thread, and dies only with the process.
    #[test]
    fn child_outlives_spawning_thread() {
        let child = std::thread::spawn(|| {
            let mut c = Command::new("sleep");
            c.arg("60");
            spawn_tied(c).unwrap()
        })
        .join()
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            alive(child.id()),
            "PDEATHSIG fired when the spawning thread exited"
        );
        ChildGuard::new(child);
    }

    /// The real guarantee: the child gets SIGTERM when its parent process dies, even by SIGKILL.
    /// The parent is this test binary re-executed as a helper that spawns `sleep` and waits.
    #[test]
    fn child_dies_with_its_process() {
        if std::env::var_os("SLIVER_PROC_HELPER").is_some() {
            return; // running as the helper's own test filter: nothing to do
        }
        let exe = std::env::current_exe().unwrap();
        let mut helper = Command::new(exe)
            .args([
                "--exact",
                "tests::helper_parent",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("SLIVER_PROC_HELPER", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut out = std::io::BufReader::new(helper.stdout.take().unwrap());
        let mut pid = None;
        let mut line = String::new();
        while std::io::BufRead::read_line(&mut out, &mut line).unwrap() > 0 {
            // libtest may print it after its own "test helper_parent ... " prefix.
            if let Some(i) = line.find("TIED_CHILD=") {
                pid = Some(line[i + 11..].trim().parse::<u32>().unwrap());
                break;
            }
            line.clear();
        }
        let pid = pid.expect("helper printed its child's pid");
        assert!(alive(pid));
        helper.kill().unwrap(); // SIGKILL: no destructors, no cleanup code runs
        helper.wait().unwrap();
        assert!(
            wait_dead(pid),
            "tied child {pid} outlived its SIGKILLed parent"
        );
    }

    #[test]
    #[allow(clippy::zombie_processes)] // the helper gets SIGKILLed; its child must die by PDEATHSIG
    fn helper_parent() {
        if std::env::var_os("SLIVER_PROC_HELPER").is_none() {
            return;
        }
        let mut c = Command::new("sleep");
        c.arg("60");
        let child = spawn_tied(c).unwrap();
        println!("TIED_CHILD={}", child.id());
        std::thread::sleep(Duration::from_secs(60));
    }
}
