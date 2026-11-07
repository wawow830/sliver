//! Child processes that die with sliver.
//!
//! Long-lived helpers (`pactl subscribe`, exec modules, test `dbus-daemon`s) must not outlive
//! the process that started them, whether it exits cleanly, panics, or is SIGKILLed.
//!
//! - [`spawn_tied`] sets `PR_SET_PDEATHSIG(SIGTERM)` in the child. Linux sends that
//!   signal when the **thread** that forked the child exits, not the process. So the fork happens
//!   on one spawner thread that lives as long as the process, and children spawned from short-lived
//!   threads (a `#[test]`, a worker) still only die with the process. A race check catches a
//!   parent that died between fork and prctl.
//! - [`ChildGuard`] kills (SIGKILL) and reaps on drop, for children that should end with their
//!   owner object.
//! - [`kill_group`] SIGKILLs a child's whole process group, for children started with
//!   `Command::process_group(0)` that may have started their own (`sh -c 'a | b'`).
//! - [`output`] runs a short-lived tool to completion within a deadline.
//!
//! PDEATHSIG covers the direct child only, and it survives `exec` of a non-setuid program. The
//! kernel clears it whenever the child changes credentials: setuid/setcap exec, or a daemon that
//! drops privileges, like `dbus-daemon` (`sliver-testbus` uses a pipe to
//! `dbus-run-session` instead).
//! Grandchildren need their process group killed ([`kill_group`]): SIGKILL, because a group
//! member that ignores SIGTERM would otherwise outlive it.
//! In production, systemd's cgroup does the rest when a unit stops.

use std::io::{self, Read};
use std::ops::{Deref, DerefMut};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::process::{Pid, PidfdFlags, Signal};

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

/// SIGKILLs the process group `leader` leads: a child the caller started with
/// `Command::process_group(0)`, and everything it started. `0` (no process) and a group that's
/// already gone are no-ops.
pub fn kill_group(leader: u32) {
    if let Some(pid) = Pid::from_raw(leader as i32) {
        let _ = rustix::process::kill_process_group(pid, Signal::KILL);
    }
}

/// Runs `cmd` like [`spawn_tied`], in its own process group, with stdin and stderr null, and
/// collects its stdout. Once the child exits, the rest of its group is SIGKILLed; at `deadline`
/// the whole group is, and the result is [`io::ErrorKind::TimedOut`].
pub fn output(mut cmd: Command, deadline: Duration) -> io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = ChildGuard::new(spawn_tied(cmd)?);
    let pid = child.id();
    // A pidfd turns readable when the child exits, before it is reaped: until `wait` below, its
    // pid and process group cannot be reused, so `kill_group` cannot hit a stranger.
    let pidfd = rustix::process::pidfd_open(Pid::from_child(&child), PidfdFlags::empty())?;
    let mut stdout = child.stdout.take().expect("piped");
    let end = Instant::now() + deadline;
    let (mut out, mut buf) = (Vec::new(), [0u8; 16 * 1024]);
    let (mut open, mut exited, mut timed_out) = (true, false, false);
    while open || !exited {
        let left = end.saturating_duration_since(Instant::now());
        let timeout = (!timed_out).then(|| Timespec::try_from(left).unwrap_or_default());
        let mut fds = Vec::with_capacity(2);
        if open {
            fds.push(PollFd::new(&stdout, PollFlags::IN));
        }
        if !exited {
            fds.push(PollFd::new(&pidfd, PollFlags::IN));
        }
        match rustix::event::poll(&mut fds, timeout.as_ref()) {
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
            Ok(0) => {
                kill_group(pid);
                timed_out = true;
                // A process that left the group may still hold the pipe: stop reading it.
                open = false;
                continue;
            }
            Ok(_) => {}
        }
        let pipe = open.then(|| fds[0].revents());
        let proc = (!exited).then(|| fds[usize::from(open)].revents());
        if pipe.is_some_and(|r| !r.is_empty()) {
            match stdout.read(&mut buf) {
                Ok(0) => open = false,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        if proc.is_some_and(|r| !r.is_empty()) {
            exited = true;
            kill_group(pid);
        }
    }
    let status = child.wait()?;
    if timed_out {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(Output {
        status,
        stdout: out,
        stderr: Vec::new(),
    })
}

/// A child that is killed and reaped when the guard is dropped.
#[derive(Debug)]
pub struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    pub fn new(child: Child) -> ChildGuard {
        ChildGuard { child: Some(child) }
    }

    /// Gives up ownership: the child is no longer killed on drop.
    pub fn into_inner(mut self) -> Child {
        self.child.take().expect("present until drop")
    }

    /// Kills and reaps now. Idempotent.
    pub fn kill(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
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

    /// Even a group member that ignores SIGTERM goes.
    #[test]
    fn kill_group_kills_grandchildren() {
        let mut c = Command::new("sh");
        c.args(["-c", "trap '' TERM; sleep 60 & echo $!; wait"])
            .stdout(Stdio::piped())
            .process_group(0);
        let mut g = ChildGuard::new(spawn_tied(c).unwrap());
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(g.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let grandchild: u32 = line.trim().parse().unwrap();
        assert!(alive(grandchild));
        kill_group(g.id());
        drop(g);
        assert!(wait_dead(grandchild), "grandchild {grandchild} survived");
    }

    #[test]
    fn output_collects_stdout_and_status() {
        let mut c = Command::new("sh");
        c.args(["-c", "head -c 100000 /dev/zero; exit 3"]);
        let o = output(c, Duration::from_secs(5)).unwrap();
        assert_eq!((o.stdout.len(), o.status.code()), (100_000, Some(3)));
    }

    /// The child's leftovers are killed when it exits, so their copy of stdout can't hold the
    /// call open; at the deadline the child itself is.
    #[test]
    fn output_kills_leftovers_and_stops_at_the_deadline() {
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 60 & echo $!"]);
        let t = Instant::now();
        let o = output(c, Duration::from_secs(30)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        let left: u32 = String::from_utf8(o.stdout).unwrap().trim().parse().unwrap();
        assert!(wait_dead(left), "leftover {left} survived");

        let mut c = Command::new("sh");
        c.args(["-c", "trap '' TERM; sleep 60"]);
        let t = Instant::now();
        let e = output(c, Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }

    /// A child spawned from a short-lived thread survives that thread, and dies only with the
    /// process.
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

    /// The child gets SIGTERM when its parent process dies, even by SIGKILL.
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
