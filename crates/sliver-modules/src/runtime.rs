//! The scheduler: one thread per module that has a command, each asleep on a channel until its
//! deadline, a trigger or a signal. Nothing polls; an inactive runtime (display off, screen
//! locked) has no timers armed at all.

use crate::output::{Output, parse_json, parse_text};
use crate::proc::{self, RunResult};
use crate::spec::{Mode, Spec};
use crate::{Context, Update};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

enum Cmd {
    Trigger,
    Active(bool),
    Stop,
}

struct Module {
    spec: Spec,
    tx: Sender<Cmd>,
    /// pid of a running continuous process (0 = none).
    pid: Arc<AtomicU32>,
    stopped: Arc<AtomicBool>,
}

struct Inner {
    modules: HashMap<String, Module>,
    ctx: Arc<Mutex<Context>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for m in self.modules.values() {
            m.stopped.store(true, Ordering::SeqCst);
            let _ = m.tx.send(Cmd::Stop);
            proc::kill_group(m.pid.load(Ordering::SeqCst));
        }
    }
}

/// Handle to the running modules. Cheap to clone; the modules stop when the last clone drops.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl Runtime {
    /// Start every module that has a command. Static modules (no `exec`) are not scheduled.
    /// Updates arrive on the returned receiver, only when a module's output actually changed.
    pub fn start(specs: Vec<Spec>) -> (Runtime, Receiver<Update>) {
        let (out_tx, out_rx) = channel();
        let ctx = Arc::new(Mutex::new(Context::default()));
        let mut modules = HashMap::new();
        for spec in specs {
            let (tx, rx) = channel();
            let m = Module {
                spec: spec.clone(),
                tx,
                pid: Arc::default(),
                stopped: Arc::default(),
            };
            if spec.exec.is_some() {
                let (pid, stopped) = (m.pid.clone(), m.stopped.clone());
                let (out, ctx) = (out_tx.clone(), ctx.clone());
                std::thread::Builder::new()
                    .name(format!("module {}", spec.key))
                    .spawn(move || worker(spec, rx, out, ctx, pid, stopped))
                    .expect("spawn thread");
            }
            modules.insert(m.spec.key.clone(), m);
        }
        (
            Runtime {
                inner: Arc::new(Inner { modules, ctx }),
            },
            out_rx,
        )
    }

    /// What `SLIVER_LAYER` / `SLIVER_LAYOUT` / `SLIVER_APP_ID` hold for the next runs.
    pub fn set_context(&self, c: Context) {
        *self.inner.ctx.lock().unwrap_or_else(|e| e.into_inner()) = c;
    }

    /// `update MODULE` / `sliverctl refresh`: run it now. `false` = no such module.
    pub fn trigger(&self, key: &str) -> bool {
        let Some(m) = self.inner.modules.get(key) else {
            return false;
        };
        let _ = m.tx.send(Cmd::Trigger);
        if m.spec.mode == Mode::Continuous {
            // The only way to "run it again" is to restart the process.
            proc::kill_group(m.pid.load(Ordering::SeqCst));
        }
        true
    }

    /// An `on-*` action fired on `key`: re-run it unless `exec-on-event = false`.
    pub fn action_fired(&self, key: &str) {
        if self
            .inner
            .modules
            .get(key)
            .is_some_and(|m| m.spec.exec_on_event && m.spec.mode != Mode::Continuous)
        {
            self.trigger(key);
        }
    }

    /// `pkill -RTMIN+N` / `sliverctl signal N`: re-run every module with `signal = N`.
    pub fn signal(&self, n: u8) -> usize {
        let keys: Vec<&String> = self
            .inner
            .modules
            .values()
            .filter(|m| m.spec.signal == Some(n))
            .map(|m| &m.spec.key)
            .collect();
        keys.iter().filter(|k| self.trigger(k)).count()
    }

    /// Display off / screen locked: stop all timers. On resume every module runs once at once,
    /// so the bar is current when it lights up. Continuous scripts keep running (they hold
    /// their own state); their updates still arrive.
    pub fn set_active(&self, active: bool) {
        for m in self.inner.modules.values() {
            let _ = m.tx.send(Cmd::Active(active));
        }
    }

    pub fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self.inner.modules.keys().cloned().collect();
        k.sort();
        k
    }
}

/// Deduplicating sender: unchanged output is not an update.
struct Emitter {
    key: String,
    tx: Sender<Update>,
    last: Option<Output>,
}

impl Emitter {
    /// `false` = nobody is listening any more.
    fn emit(&mut self, o: Output) -> bool {
        if self.last.as_ref() == Some(&o) {
            return true;
        }
        self.last = Some(o.clone());
        self.tx
            .send(Update {
                module: self.key.clone(),
                output: o,
            })
            .is_ok()
    }
}

fn to_output(spec: &Spec, text: &str) -> Output {
    let parsed = if spec.json {
        match text.lines().find(|l| !l.trim().is_empty()) {
            Some(line) => parse_json(line),
            None => Ok(Output::default()),
        }
    } else {
        Ok(parse_text(text))
    };
    match parsed {
        Ok(o) if spec.escape => o.escaped(),
        Ok(o) => o,
        Err(e) => Output::error(e),
    }
}

fn failure(spec: &Spec, r: &RunResult) -> Output {
    if r.timed_out {
        return Output::error(format!("timed out after {:?}", spec.timeout));
    }
    let why = r
        .stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(str::trim);
    match (r.code, why) {
        (_, Some(w)) => Output::error(w),
        (Some(c), None) => Output::error(format!("exited with status {c}")),
        (None, None) => Output::error("killed by a signal"),
    }
}

/// One execution of an interval/once module.
fn run_and_emit(spec: &Spec, ctx: &Context, em: &mut Emitter) -> bool {
    if let Some(cond) = &spec.exec_if {
        match proc::run_once(spec, ctx, cond) {
            Ok(r) if r.code == Some(0) && !r.timed_out => {}
            Ok(_) => return em.emit(Output::hidden()),
            Err(e) => return em.emit(Output::error(format!("exec-if: {e}"))),
        }
    }
    let Some(script) = spec.exec.as_deref() else {
        return true;
    };
    let out = match proc::run_once(spec, ctx, script) {
        Ok(r) if r.code == Some(0) && !r.timed_out => to_output(spec, &r.stdout),
        Ok(r) => failure(spec, &r),
        Err(e) => Output::error(format!("cannot run: {e}")),
    };
    em.emit(out)
}

fn worker(
    spec: Spec,
    rx: Receiver<Cmd>,
    tx: Sender<Update>,
    ctx: Arc<Mutex<Context>>,
    pid: Arc<AtomicU32>,
    stopped: Arc<AtomicBool>,
) {
    let mut em = Emitter {
        key: spec.key.clone(),
        tx,
        last: None,
    };
    if proc::is_root() {
        em.emit(Output::error("user modules do not run as root"));
        return;
    }
    let snapshot = || ctx.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if spec.mode == Mode::Continuous {
        continuous(&spec, &rx, &mut em, &snapshot, &pid, &stopped);
        return;
    }
    let mut active = true;
    let mut pending = true;
    let mut next: Option<Instant> = None;
    loop {
        if active && pending {
            pending = false;
            if stopped.load(Ordering::SeqCst) || !run_and_emit(&spec, &snapshot(), &mut em) {
                return;
            }
            next = match spec.mode {
                Mode::Every(d) => Some(Instant::now() + d),
                _ => None,
            };
        }
        let cmd = match next.filter(|_| active) {
            Some(t) => rx.recv_timeout(t.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match cmd {
            Ok(Cmd::Trigger) | Err(RecvTimeoutError::Timeout) => pending = true,
            Ok(Cmd::Active(a)) => {
                pending |= a && !active;
                active = a;
            }
            Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn continuous(
    spec: &Spec,
    rx: &Receiver<Cmd>,
    em: &mut Emitter,
    snapshot: &dyn Fn() -> Context,
    pid: &AtomicU32,
    stopped: &AtomicBool,
) {
    let script = spec.exec.as_deref().unwrap_or("");
    let mut delay = spec.restart_interval;
    loop {
        if stopped.load(Ordering::SeqCst) {
            return;
        }
        let started = Instant::now();
        match proc::spawn_stream(spec, &snapshot(), script) {
            Err(e) => {
                if !em.emit(Output::error(format!("cannot run: {e}"))) {
                    return;
                }
            }
            Ok(mut child) => {
                pid.store(child.id(), Ordering::SeqCst);
                let stdout = child.stdout.take().expect("piped");
                for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
                    let line = String::from_utf8_lossy(&line);
                    if line.trim().is_empty() {
                        continue;
                    }
                    let o = if spec.json {
                        to_output(spec, &line)
                    } else {
                        to_output(spec, line.trim_end())
                    };
                    if !em.emit(o) {
                        proc::kill_group(child.id());
                        return;
                    }
                }
                let status = child.wait();
                proc::kill_group(pid.swap(0, Ordering::SeqCst));
                if stopped.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(st) = status {
                    if !st.success() && !em.emit(Output::error(format!("exited with {st}"))) {
                        return;
                    }
                }
            }
        }
        // Scripts that die quickly back off (x2, up to a minute); ones that ran a while restart
        // after the plain restart-interval.
        delay = if started.elapsed() >= Duration::from_secs(10) {
            spec.restart_interval
        } else {
            (delay * 2)
                .min(Duration::from_secs(60))
                .max(spec.restart_interval)
        };
        // A trigger that arrived while the process was running restarts it without waiting.
        let mut skip = false;
        while let Ok(c) = rx.try_recv() {
            match c {
                Cmd::Trigger => skip = true,
                Cmd::Stop => return,
                Cmd::Active(_) => {}
            }
        }
        if !skip {
            match rx.recv_timeout(delay) {
                Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                _ => {}
            }
        }
    }
}
