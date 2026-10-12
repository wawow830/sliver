//! Spawning module commands: `sh -c`, own process group (so a timeout kills the whole tree),
//! no stdin, `no_new_privs`, bounded output, SIGTERM when the agent dies (`sliver_proc`, T47).
//! Never as root.

use crate::Context;
use crate::spec::Spec;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Output beyond this is read and thrown away: a runaway script can't eat memory.
const MAX_STDOUT: usize = 64 * 1024;
const MAX_STDERR: usize = 4 * 1024;

pub(crate) fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

pub(crate) fn command(spec: &Spec, ctx: &Context, script: &str, pipe_stderr: bool) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(script)
        .env("SLIVER_MODULE", &spec.key)
        .env("SLIVER_LAYER", &ctx.layer)
        .env("SLIVER_LAYOUT", &ctx.layout)
        .env("SLIVER_APP_ID", &ctx.app_id)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if pipe_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .process_group(0);
    // SAFETY: the closure only calls prctl, which is async-signal-safe, between fork and exec.
    unsafe {
        c.pre_exec(|| {
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            Ok(())
        });
    }
    c
}

/// Spawn a long-running module command (stdout piped, stderr discarded).
pub(crate) fn spawn_stream(spec: &Spec, ctx: &Context, script: &str) -> io::Result<Child> {
    sliver_proc::spawn_tied(command(spec, ctx, script, false))
}

/// SIGKILL the command and everything it started (its process group).
pub(crate) fn kill_group(pid: u32) {
    if pid > 0 {
        // SAFETY: kill(2) with a negative pid targets the process group `pid`; we created that
        // group with process_group(0) so it is ours. Failure (already gone) is fine.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[derive(Debug)]
pub(crate) struct RunResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

struct Capture {
    buf: Arc<Mutex<Vec<u8>>>,
    done: Receiver<()>,
}

fn capture(mut r: impl Read + Send + 'static, cap: usize) -> Capture {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let (tx, done) = channel();
    let b = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = r.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let mut v = b.lock().unwrap_or_else(|e| e.into_inner());
            let room = cap.saturating_sub(v.len());
            v.extend_from_slice(&chunk[..n.min(room)]);
        }
        let _ = tx.send(());
    });
    Capture { buf, done }
}

impl Capture {
    /// The bytes read so far, after giving the reader a moment to finish. A daemonised
    /// grandchild can hold the pipe open forever; we don't wait for it.
    fn finish(self) -> String {
        let _ = self.done.recv_timeout(Duration::from_millis(300));
        let v = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&v).into_owned()
    }
}

/// Run `script` to completion or `spec.timeout`, whichever is first.
pub(crate) fn run_once(spec: &Spec, ctx: &Context, script: &str) -> io::Result<RunResult> {
    let mut child = sliver_proc::spawn_tied(command(spec, ctx, script, true))?;
    let pid = child.id();
    let out = capture(child.stdout.take().expect("piped"), MAX_STDOUT);
    let err = capture(child.stderr.take().expect("piped"), MAX_STDERR);
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let (status, timed_out) = match rx.recv_timeout(spec.timeout) {
        Ok(s) => (s?, false),
        Err(_) => {
            kill_group(pid);
            (
                rx.recv()
                    .map_err(|_| io::Error::other("wait thread died"))??,
                true,
            )
        }
    };
    // Whatever the script left running in its group goes with it.
    kill_group(pid);
    Ok(RunResult {
        code: status.code(),
        stdout: out.finish(),
        stderr: err.finish(),
        timed_out,
    })
}
