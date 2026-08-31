use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};

use super::{
    DriveRequest, InputState, InputTransition, KeyOperation, KeyRequest, LuaSource, ModifierMode,
    Runtime, StopReason, TimedFrame, TouchEvent, TouchPhase, VisibilityReason, WorkerEffects,
    WorkerIdentity,
};
use crate::frame_slots::{FrameBroker, FrameSlots, FrameTiming};
use crate::hardware::{LogicalFrame, Modifier, ObservedKey, OutputKey};

#[cfg(test)]
const WORKER_FD: RawFd = 0;
#[cfg(test)]
const FIRST_INHERITED_FD: RawFd = 3;
const MAX_PACKET_BYTES: usize = 2 * 1024 * 1024;
const CALLBACK_DEADLINE: Duration = Duration::from_secs(2);
#[cfg(test)]
const STOP_DEADLINE: Duration = Duration::from_millis(500);
const KILL_REAP_DEADLINE: Duration = Duration::from_millis(500);
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(100);
const WRITE_RETRY: Duration = Duration::from_millis(2);

const BOOTSTRAP: u8 = 10;
const HELLO: u8 = 6;
const COMMAND: u8 = 2;
const REPLY: u8 = 3;
const READY: u8 = 4;
const HEARTBEAT: u8 = 5;

const RENDER: u8 = 1;
const COMMIT: u8 = 2;
const DRIVE: u8 = 3;
const PENDING_BACKLIGHT: u8 = 4;
const RESTORE_BACKLIGHT: u8 = 5;
const SHUTDOWN: u8 = 6;

const STATUS_OK: u8 = 0;
const STATUS_ERROR: u8 = 1;

static NEXT_UNIT_ID: AtomicU64 = AtomicU64::new(1);

struct SpawnedWorker {
    child: Child,
    stream: UnixStream,
    process_group: libc::pid_t,
    unit: Option<String>,
}

struct ProcessEffects {
    frame: Option<FrameTiming>,
    backlight: Option<f64>,
    key_requests: Vec<KeyRequest>,
    next_worker_deadline: Option<f64>,
    redraw_pending: bool,
}

pub(crate) struct ProcessWorker {
    child: Mutex<Child>,
    pidfd: std::fs::File,
    stream: Mutex<UnixStream>,
    input: Mutex<Vec<u8>>,
    last_heartbeat: Mutex<Instant>,
    failure: Mutex<Option<String>>,
    terminated: AtomicBool,
    pid: libc::pid_t,
    process_group: libc::pid_t,
    unit: Option<String>,
    broker: FrameBroker,
}

impl ProcessWorker {
    #[cfg(test)]
    pub(crate) fn stage(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
    ) -> Result<Self> {
        let path = frame_path()?;
        let slots = FrameSlots::new_shared(
            &path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        Self::stage_with_frames(
            source,
            initial_backlight,
            initial_input,
            &path,
            slots.broker(),
            WorkerIdentity::User,
        )
    }

    #[allow(clippy::needless_return)]
    pub(crate) fn stage_with_frames(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        frame_path: &Path,
        broker: FrameBroker,
        identity: WorkerIdentity,
    ) -> Result<Self> {
        #[cfg(test)]
        {
            return Self::stage_with_spawned(
                source,
                initial_backlight,
                initial_input,
                frame_path,
                broker,
                identity,
                spawn_direct,
            );
        }
        #[cfg(not(test))]
        {
            Self::stage_with_spawned(
                source,
                initial_backlight,
                initial_input,
                frame_path,
                broker,
                identity,
                spawn_systemd,
            )
        }
    }

    #[cfg(test)]
    pub(crate) fn stage_with_frames_systemd(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        frame_path: &Path,
        broker: FrameBroker,
        identity: WorkerIdentity,
    ) -> Result<Self> {
        Self::stage_with_spawned(
            source,
            initial_backlight,
            initial_input,
            frame_path,
            broker,
            identity,
            spawn_systemd,
        )
    }

    fn stage_with_spawned(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        frame_path: &Path,
        broker: FrameBroker,
        identity: WorkerIdentity,
        spawn: fn(WorkerIdentity) -> Result<SpawnedWorker>,
    ) -> Result<Self> {
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("enabling Lua worker child reaping");
        }
        let mut spawned = spawn(identity)?;
        let mut stream = spawned.stream;
        if let Err(error) = stream.set_nonblocking(true) {
            if let Some(unit) = spawned.unit.as_deref() {
                kill_systemd_unit(unit);
            }
            let _ = terminate_child(&mut spawned.child);
            return Err(error).context("configuring Lua worker control socket");
        }
        let worker_pid = match receive_hello(&mut stream, &mut Vec::new()) {
            Ok(pid) => pid,
            Err(error) => {
                if let Some(unit) = spawned.unit.as_deref() {
                    kill_systemd_unit(unit);
                }
                let _ = terminate_child(&mut spawned.child);
                return Err(error);
            }
        };
        let pidfd = match open_pidfd(worker_pid) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                if let Some(unit) = spawned.unit.as_deref() {
                    kill_systemd_unit(unit);
                }
                let _ = terminate_child(&mut spawned.child);
                return Err(error).context("opening the Lua worker pidfd");
            }
        };
        let worker = Self {
            child: Mutex::new(spawned.child),
            pidfd,
            stream: Mutex::new(stream),
            input: Mutex::new(Vec::new()),
            last_heartbeat: Mutex::new(Instant::now()),
            failure: Mutex::new(None),
            terminated: AtomicBool::new(false),
            pid: worker_pid,
            process_group: if spawned.process_group == 0 {
                worker_pid
            } else {
                spawned.process_group
            },
            unit: spawned.unit,
            broker,
        };
        worker.request_bootstrap(source, initial_backlight, initial_input, frame_path)?;
        Ok(worker)
    }
}

#[cfg(test)]
fn spawn_direct(_identity: WorkerIdentity) -> Result<SpawnedWorker> {
    let (parent_fd, child_fd) = socket_pair()?;
    let parent_pid = unsafe { libc::getpid() };
    let mut command = worker_command()?;
    command.stdin(unsafe { Stdio::from(std::fs::File::from_raw_fd(child_fd)) });
    command.stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(move || {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            close_inherited_descriptors();
            Ok(())
        });
    }
    command.env("SLIVER_LUA_WORKER_FD", WORKER_FD.to_string());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            unsafe {
                libc::close(parent_fd);
            }
            return Err(error).context("starting the Lua worker process");
        }
    };
    let pid = child.id() as libc::pid_t;
    let stream = unsafe { UnixStream::from_raw_fd(parent_fd) };
    Ok(SpawnedWorker {
        child,
        stream,
        process_group: pid,
        unit: None,
    })
}

fn spawn_systemd(identity: WorkerIdentity) -> Result<SpawnedWorker> {
    use std::os::unix::fs::PermissionsExt;

    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = runtime.join("sliver");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("creating worker socket directory {}", directory.display()))?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let id = NEXT_UNIT_ID.fetch_add(1, Ordering::Relaxed);
    let socket_path = directory.join(format!("worker-{}-{id}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding worker socket {}", socket_path.display()))?;
    listener.set_nonblocking(true)?;
    let unit = format!("sliver-lua-worker-{}-{id}.service", std::process::id());
    let mut launcher = Command::new("systemd-run");
    let syslog_identifier = match identity {
        WorkerIdentity::User => "sliver-lua",
        WorkerIdentity::RestrictedFallback => "sliver-fallback",
    };
    launcher.args([
        "--user",
        "--unit",
        unit.as_str(),
        "--collect",
        "--wait",
        "--quiet",
        "--service-type=exec",
    ]);
    for property in [
        "MemoryAccounting=yes",
        "MemoryMax=512M",
        "TasksAccounting=yes",
        "TasksMax=64",
        "OOMPolicy=kill",
        "KillMode=control-group",
        "TimeoutStopSec=1s",
        "NoNewPrivileges=yes",
        "PrivateDevices=yes",
        "DevicePolicy=closed",
        "ProtectKernelTunables=yes",
        "StandardInput=null",
        "StandardOutput=journal",
        "StandardError=journal",
        "Restart=no",
        "WatchdogSec=0",
    ] {
        launcher.arg("--property").arg(property);
    }
    launcher
        .arg("--property")
        .arg(format!("SyslogIdentifier={syslog_identifier}"));
    let worker_path = match worker_path() {
        Ok(path) => path,
        Err(error) => {
            let _ = std::fs::remove_file(&socket_path);
            return Err(error);
        }
    };
    launcher
        .arg(worker_path)
        .arg("--connect")
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = match launcher.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_file(&socket_path);
            return Err(error).context("starting the systemd Lua worker service");
        }
    };
    let deadline = Instant::now() + CALLBACK_DEADLINE;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    kill_systemd_unit(&unit);
                    let _ = terminate_child(&mut child);
                    let _ = std::fs::remove_file(&socket_path);
                    bail!("Lua worker service did not connect within two seconds")
                }
                thread::sleep(WRITE_RETRY);
            }
            Err(error) => {
                kill_systemd_unit(&unit);
                let _ = terminate_child(&mut child);
                let _ = std::fs::remove_file(&socket_path);
                return Err(error).context("accepting the Lua worker connection");
            }
        }
    };
    let _ = std::fs::remove_file(&socket_path);
    if let Err(error) = stream.set_nonblocking(true) {
        kill_systemd_unit(&unit);
        let _ = terminate_child(&mut child);
        return Err(error).context("configuring Lua worker control socket");
    }
    Ok(SpawnedWorker {
        child,
        stream,
        process_group: 0,
        unit: Some(unit),
    })
}

fn terminate_child(child: &mut Child) -> Result<()> {
    child.kill().ok();
    let deadline = Instant::now() + KILL_REAP_DEADLINE;
    loop {
        if child.try_wait()?.is_some() || Instant::now() >= deadline {
            return Ok(());
        }
        thread::sleep(WRITE_RETRY);
    }
}

fn receive_hello(stream: &mut UnixStream, input: &mut Vec<u8>) -> Result<libc::pid_t> {
    let deadline = Instant::now() + CALLBACK_DEADLINE;
    loop {
        if let Some((kind, payload)) = read_packets(stream, input)?.into_iter().next() {
            ensure!(
                kind == HELLO,
                "Lua worker sent an unexpected startup packet"
            );
            ensure!(payload.len() == 4, "Lua worker hello packet is malformed");
            return Ok(u32::from_be_bytes(payload.as_slice().try_into()?) as libc::pid_t);
        }
        if Instant::now() >= deadline {
            bail!("Lua worker did not identify itself within two seconds")
        }
        thread::sleep(WRITE_RETRY);
    }
}

impl ProcessWorker {
    fn request_bootstrap(
        &self,
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        frame_path: &Path,
    ) -> Result<()> {
        let mut payload = Vec::new();
        encode_source(&mut payload, source)?;
        put_bytes(&mut payload, frame_path.as_os_str().as_bytes())?;
        put_f64(&mut payload, initial_backlight);
        encode_input_state(&mut payload, initial_input);
        let response = self.request(BOOTSTRAP, payload, CALLBACK_DEADLINE)?;
        match response.as_slice() {
            [STATUS_OK] => Ok(()),
            _ => Err(self.protocol_failure(response_error("loading Lua worker", &response))),
        }
    }

    pub(crate) fn render(
        &self,
        presentation_time: f64,
        delta: f64,
        input_state: InputState,
    ) -> Result<TimedFrame> {
        let mut payload = Vec::new();
        put_f64(&mut payload, presentation_time);
        put_f64(&mut payload, delta);
        encode_input_state(&mut payload, input_state);
        let response = self.request(RENDER, payload, CALLBACK_DEADLINE)?;
        let timing = self
            .terminal_response(parse_frame_response(&response))?
            .context("Lua worker published no frame")?;
        let completed = self
            .broker
            .take_newest()?
            .context("Lua worker published no shared frame")?;
        let (frame, _) = LogicalFrame::from_completed(completed);
        Ok(TimedFrame { frame, timing })
    }

    pub(crate) fn commit(&self, now_seconds: f64, input_state: InputState) -> Result<()> {
        let mut payload = Vec::new();
        put_f64(&mut payload, now_seconds);
        encode_input_state(&mut payload, input_state);
        let response = self.request(COMMIT, payload, CALLBACK_DEADLINE)?;
        self.terminal_response(parse_status_response("committing Lua worker", &response))
    }

    pub(crate) fn drive_until(
        &self,
        request: DriveRequest,
        deadline: Instant,
    ) -> Result<WorkerEffects> {
        let mut payload = Vec::new();
        encode_drive_request(&mut payload, request)?;
        let response = self.request_until(DRIVE, payload, deadline)?;
        let effects = self.terminal_response_until(parse_effects_response(&response), deadline)?;
        let frame = match effects.frame {
            Some(timing) => {
                let completed = self
                    .broker
                    .take_newest()?
                    .context("Lua worker published no shared frame")?;
                let (frame, _) = LogicalFrame::from_completed(completed);
                Some(TimedFrame { frame, timing })
            }
            None => None,
        };
        Ok(WorkerEffects {
            frame,
            backlight: effects.backlight,
            key_requests: effects.key_requests,
            next_worker_deadline: effects.next_worker_deadline,
            redraw_pending: effects.redraw_pending,
        })
    }

    pub(crate) fn pending_backlight(&self) -> Result<Option<f64>> {
        let response = self.request(PENDING_BACKLIGHT, Vec::new(), CALLBACK_DEADLINE)?;
        self.terminal_response(parse_optional_f64_response(
            "reading staged Lua backlight",
            &response,
        ))
    }

    pub(crate) fn restore_backlight(&self, level: f64) -> Result<()> {
        let mut payload = Vec::new();
        put_f64(&mut payload, level);
        let response = self.request(RESTORE_BACKLIGHT, payload, CALLBACK_DEADLINE)?;
        self.terminal_response(parse_status_response(
            "restoring Lua backlight state",
            &response,
        ))
    }

    #[cfg(test)]
    pub(crate) fn shutdown(self, reason: StopReason) -> Result<()> {
        self.shutdown_until(reason, Instant::now() + STOP_DEADLINE)
    }

    pub(crate) fn shutdown_until(self, reason: StopReason, deadline: Instant) -> Result<()> {
        let payload = vec![stop_reason_code(reason)];
        let response = self.request_until(SHUTDOWN, payload, deadline);
        match response {
            Ok(response) => {
                let result = self.terminal_response_until(
                    parse_status_response("stopping Lua worker", &response),
                    deadline,
                );
                self.wait_for_exit_until(deadline)?;
                self.kill_unit();
                self.kill_process_group();
                result
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn is_alive(&self) -> bool {
        if self.terminated.load(Ordering::Acquire) {
            return false;
        }
        let packets = match self.drain_packets() {
            Ok(packets) => packets,
            Err(error) => {
                self.mark_failed(error.to_string());
                self.terminate();
                return false;
            }
        };
        for (kind, _) in packets {
            if kind == HEARTBEAT {
                self.note_heartbeat();
            }
        }
        let exited = self
            .child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok())
            .flatten()
            .is_some();
        if exited {
            self.mark_failed("Lua worker process exited".into());
            self.kill_unit();
            self.kill_process_group();
            self.terminated.store(true, Ordering::Release);
            return false;
        }
        let stale = self
            .last_heartbeat
            .lock()
            .map(|last| last.elapsed() >= CALLBACK_DEADLINE)
            .unwrap_or(true);
        if stale {
            self.mark_failed("Lua worker heartbeat timed out after two seconds".into());
            self.terminate();
            return false;
        }
        true
    }

    pub(crate) fn failure_reason(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|failure| failure.clone())
    }

    fn kill_unit(&self) {
        if let Some(unit) = self.unit.as_deref() {
            kill_systemd_unit(unit);
        }
    }

    fn kill_process_group(&self) {
        for descendant in descendants_of(self.pid) {
            unsafe {
                libc::kill(descendant, libc::SIGKILL);
            }
        }
        unsafe {
            libc::kill(-self.process_group, libc::SIGKILL);
        }
    }

    pub(crate) fn terminate(&self) {
        self.terminate_until(Instant::now() + KILL_REAP_DEADLINE);
    }

    fn terminate_until(&self, deadline: Instant) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        self.kill_unit();
        loop {
            self.kill_process_group();
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.pidfd.as_raw_fd(),
                    libc::SIGKILL,
                    0,
                    0,
                );
            }
            let exited = self
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.try_wait().ok())
                .flatten()
                .is_some();
            if exited || Instant::now() >= deadline {
                break;
            }
            thread::sleep(WRITE_RETRY);
        }
    }

    fn request(&self, command: u8, payload: Vec<u8>, timeout: Duration) -> Result<Vec<u8>> {
        self.request_until(command, payload, Instant::now() + timeout)
    }

    fn request_until(&self, command: u8, payload: Vec<u8>, deadline: Instant) -> Result<Vec<u8>> {
        if self.terminated.load(Ordering::Acquire) {
            bail!("Lua worker process is not running")
        }
        {
            let mut stream = lock(&self.stream, "Lua worker control socket")?;
            let (kind, prefix) = if command == BOOTSTRAP {
                (BOOTSTRAP, &[][..])
            } else {
                (COMMAND, &[command][..])
            };
            if let Err(error) = write_packet(&mut stream, kind, prefix, &payload, deadline)
                .with_context(|| format!("sending Lua worker command {command}"))
            {
                self.mark_failed(error.to_string());
                self.terminate_until(deadline);
                return Err(error);
            }
        }
        loop {
            let packets = match self.drain_packets() {
                Ok(packets) => packets,
                Err(error) => {
                    self.mark_failed(error.to_string());
                    self.terminate_until(deadline);
                    return Err(error);
                }
            };
            // A command reply and the heartbeat sent after it may arrive in
            // the same read. Observe heartbeats before returning the reply.
            for (kind, _) in &packets {
                if *kind == HEARTBEAT {
                    self.note_heartbeat();
                }
            }
            for (kind, payload) in packets {
                match kind {
                    HEARTBEAT => {}
                    REPLY => {
                        if Instant::now() >= deadline {
                            return Err(self.deadline_failure(command, deadline));
                        }
                        let (response_command, response) = match split_reply(&payload) {
                            Ok(reply) => reply,
                            Err(error) => {
                                return Err(self.protocol_failure_until(error, deadline));
                            }
                        };
                        if response_command != command {
                            return Err(self.protocol_failure_until(
                                anyhow!(
                                    "Lua worker replied to command {response_command} while waiting for {command}"
                                ),
                                deadline,
                            ));
                        }
                        return Ok(response);
                    }
                    READY if command == BOOTSTRAP => {
                        if Instant::now() >= deadline {
                            return Err(self.deadline_failure(command, deadline));
                        }
                        return Ok(payload);
                    }
                    READY => {
                        return Err(self.protocol_failure_until(
                            anyhow!("Lua worker sent a startup packet outside bootstrap"),
                            deadline,
                        ))
                    }
                    other => {
                        return Err(self.protocol_failure_until(
                            anyhow!("unexpected Lua worker packet {other}"),
                            deadline,
                        ))
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(self.deadline_failure(command, deadline));
            }
            thread::sleep(WRITE_RETRY);
        }
    }

    fn deadline_failure(&self, command: u8, deadline: Instant) -> anyhow::Error {
        let label = if command == SHUTDOWN {
            "Lua worker stop exceeded 500 milliseconds"
        } else {
            "Lua worker callback exceeded two seconds"
        };
        self.mark_failed(label.into());
        self.terminate_until(deadline);
        anyhow!(label)
    }

    fn protocol_failure(&self, error: anyhow::Error) -> anyhow::Error {
        self.protocol_failure_until(error, Instant::now() + KILL_REAP_DEADLINE)
    }

    fn protocol_failure_until(&self, error: anyhow::Error, deadline: Instant) -> anyhow::Error {
        self.mark_failed(format!("Lua worker protocol failure: {error:#}"));
        self.terminate_until(deadline);
        error.context("Lua worker protocol failure")
    }

    fn terminal_response<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|error| self.protocol_failure(error))
    }

    fn terminal_response_until<T>(&self, result: Result<T>, deadline: Instant) -> Result<T> {
        result.map_err(|error| self.protocol_failure_until(error, deadline))
    }

    fn drain_packets(&self) -> Result<Vec<(u8, Vec<u8>)>> {
        let mut stream = lock(&self.stream, "Lua worker control socket")?;
        let mut input = lock(&self.input, "Lua worker packet buffer")?;
        read_packets(&mut stream, &mut input)
    }

    fn note_heartbeat(&self) {
        if let Ok(mut last) = self.last_heartbeat.lock() {
            *last = Instant::now();
        }
    }

    fn mark_failed(&self, reason: String) {
        if let Ok(mut failure) = self.failure.lock() {
            if failure.is_none() {
                *failure = Some(reason);
            }
        }
    }

    fn wait_for_exit_until(&self, deadline: Instant) -> Result<()> {
        loop {
            let mut child = lock(&self.child, "Lua worker process")?;
            if child.try_wait()?.is_some() {
                self.terminated.store(true, Ordering::Release);
                drop(child);
                self.kill_unit();
                self.kill_process_group();
                return Ok(());
            }
            drop(child);
            if Instant::now() >= deadline {
                self.mark_failed("Lua worker did not exit after stop".into());
                self.terminate_until(deadline);
                bail!("Lua worker did not exit after stop")
            }
            thread::sleep(WRITE_RETRY);
        }
    }
}

impl Drop for ProcessWorker {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(crate) fn worker_main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut stream = if let Some(flag) = args.next() {
        ensure!(flag == "--connect", "unknown Lua worker argument");
        let path = args.next().context("--connect requires a socket path")?;
        ensure!(args.next().is_none(), "too many Lua worker arguments");
        UnixStream::connect(path).context("connecting to the Lua worker supervisor")?
    } else {
        let fd = std::env::var("SLIVER_LUA_WORKER_FD")
            .context("SLIVER_LUA_WORKER_FD is not set")?
            .parse::<RawFd>()
            .context("SLIVER_LUA_WORKER_FD is invalid")?;
        let control_fd = unsafe { libc::dup(fd) };
        if control_fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("duplicating Lua worker control fd");
        }
        unsafe {
            libc::close(fd);
        }
        unsafe { UnixStream::from_raw_fd(control_fd) }
    };
    stream.set_nonblocking(true)?;
    send_hello(&mut stream)?;
    let mut input = Vec::new();
    let (source, frame_path, initial_backlight, initial_input) = loop {
        let packets = read_packets(&mut stream, &mut input)?;
        if let Some((kind, payload)) = packets.into_iter().next() {
            ensure!(kind == BOOTSTRAP, "Lua worker expected bootstrap packet");
            break decode_bootstrap(&payload)?;
        }
        thread::sleep(WRITE_RETRY);
    };

    if let LuaSource::File(path) = &source {
        let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::env::set_current_dir(directory)
            .with_context(|| format!("changing Lua worker directory to {}", directory.display()))?;
    }
    let slots = FrameSlots::open_shared(&frame_path)?;
    let producer = slots.producer();
    let runtime = match Runtime::load(&source, initial_backlight, initial_input, producer) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Lua worker failed during startup: {error}");
            send_ready(&mut stream, STATUS_ERROR, error)?;
            return Ok(());
        }
    };
    send_ready(&mut stream, STATUS_OK, String::new())?;
    run_worker_loop(runtime, &mut stream, input)
}

fn send_hello(stream: &mut UnixStream) -> Result<()> {
    write_packet_blocking(stream, HELLO, &std::process::id().to_be_bytes())
}

fn run_worker_loop(
    mut runtime: Runtime,
    stream: &mut UnixStream,
    mut input: Vec<u8>,
) -> Result<()> {
    let mut next_heartbeat = Instant::now();
    loop {
        let mut handled = false;
        for (kind, payload) in read_packets(stream, &mut input)? {
            handled = true;
            ensure!(kind == COMMAND, "unexpected packet from Lua supervisor");
            let (command, payload) = payload
                .split_first()
                .context("Lua worker command is empty")?;
            let result = handle_command(*command, payload, &mut runtime);
            let (status, body) = match result {
                Ok(body) => (STATUS_OK, body),
                Err(error) => {
                    eprintln!("Lua worker command failed: {error:#}");
                    (STATUS_ERROR, encode_error(&error.to_string()))
                }
            };
            let mut response = vec![*command, status];
            response.extend(body);
            write_packet_blocking(stream, REPLY, &response)?;
            if *command == SHUTDOWN {
                return Ok(());
            }
        }
        if Instant::now() >= next_heartbeat {
            write_packet_blocking(stream, HEARTBEAT, &[])?;
            next_heartbeat = Instant::now() + HEARTBEAT_INTERVAL;
        }
        if !handled {
            thread::sleep(WRITE_RETRY);
        }
    }
}

fn handle_command(command: u8, payload: &[u8], runtime: &mut Runtime) -> Result<Vec<u8>> {
    match command {
        RENDER => {
            let mut reader = Reader::new(payload);
            let presentation_time = reader.f64()?;
            let delta = reader.f64()?;
            let input_state = decode_input_state(&mut reader)?;
            reader.finish()?;
            runtime
                .render_and_queue(presentation_time, delta, input_state)
                .map_err(anyhow::Error::msg)?;
            let frame = runtime
                .pending_frame
                .is_none()
                .then_some(FrameTiming::new(presentation_time, delta)?);
            encode_frame_response(frame)
        }
        COMMIT => {
            let mut reader = Reader::new(payload);
            let now = reader.f64()?;
            let input_state = decode_input_state(&mut reader)?;
            reader.finish()?;
            runtime
                .commit(now, input_state)
                .map_err(anyhow::Error::msg)?;
            Ok(Vec::new())
        }
        DRIVE => {
            let request = decode_drive_request(payload)?;
            let effects = runtime.drive(request).map_err(anyhow::Error::msg)?;
            encode_effects_response(effects)
        }
        PENDING_BACKLIGHT => {
            ensure!(
                payload.is_empty(),
                "pending backlight command has a payload"
            );
            let mut body = Vec::new();
            match runtime.pending_backlight() {
                Some(level) => {
                    body.push(1);
                    put_f64(&mut body, level);
                }
                None => body.push(0),
            }
            Ok(body)
        }
        RESTORE_BACKLIGHT => {
            let mut reader = Reader::new(payload);
            let level = reader.f64()?;
            reader.finish()?;
            runtime
                .restore_backlight(level)
                .map_err(anyhow::Error::msg)?;
            Ok(Vec::new())
        }
        SHUTDOWN => {
            let reason = decode_stop_reason(payload)?;
            runtime.stop(reason).map_err(anyhow::Error::msg)?;
            Ok(Vec::new())
        }
        other => bail!("unknown Lua worker command {other}"),
    }
}

#[cfg(test)]
fn socket_pair() -> Result<(RawFd, RawFd)> {
    let mut fds = [0; 2];
    let result = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("creating Lua worker socket pair");
    }
    Ok((fds[0], fds[1]))
}

fn open_pidfd(pid: libc::pid_t) -> Result<std::fs::File> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd as RawFd) })
}

fn descendants_of(pid: libc::pid_t) -> Vec<libc::pid_t> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    let Ok(children) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut descendants = Vec::new();
    for child in children
        .split_whitespace()
        .filter_map(|value| value.parse::<libc::pid_t>().ok())
    {
        descendants.push(child);
        descendants.extend(descendants_of(child));
    }
    descendants
}

pub(super) fn frame_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = runtime.join("sliver");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("creating frame directory {}", directory.display()))?;
    let id = NEXT_UNIT_ID.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("frame-{}-{id}.bin", std::process::id()));
    let _ = std::fs::remove_file(&path);
    Ok(path)
}

fn worker_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SLIVER_LUA_WORKER") {
        return Ok(PathBuf::from(path));
    }
    let current = std::env::current_exe().context("finding the Sliver supervisor executable")?;
    let parent = current
        .parent()
        .context("Sliver supervisor executable has no parent")?;
    let worker = parent.join("sliver-lua-worker");
    if worker.exists() {
        return Ok(worker);
    }
    parent
        .file_name()
        .is_some_and(|name| name == "deps")
        .then(|| {
            parent
                .parent()
                .map(|parent| parent.join("sliver-lua-worker"))
        })
        .flatten()
        .context("Sliver Lua worker executable was not found")
}

#[cfg(test)]
fn worker_command() -> Result<Command> {
    Ok(Command::new(worker_path()?))
}

fn kill_systemd_unit(unit: &str) {
    let _ = Command::new("systemctl")
        .args(["--user", "kill", "--kill-who=all", "--signal=SIGKILL", unit])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

#[cfg(test)]
unsafe fn close_inherited_descriptors() {
    #[allow(clippy::useless_conversion)]
    let result = libc::syscall(
        libc::SYS_close_range,
        FIRST_INHERITED_FD as libc::c_ulong,
        libc::c_uint::MAX as libc::c_ulong,
        0,
    );
    if result < 0 && *libc::__errno_location() == libc::ENOSYS {
        for fd in FIRST_INHERITED_FD..1024 {
            libc::close(fd);
        }
    }
}

fn write_packet_blocking(stream: &mut UnixStream, kind: u8, payload: &[u8]) -> Result<()> {
    let deadline = Instant::now() + CALLBACK_DEADLINE;
    write_packet(stream, kind, &[], payload, deadline)
}

fn write_packet(
    stream: &mut UnixStream,
    kind: u8,
    prefix: &[u8],
    payload: &[u8],
    deadline: Instant,
) -> Result<()> {
    let length = 1usize
        .checked_add(prefix.len())
        .and_then(|length| length.checked_add(payload.len()))
        .context("Lua worker packet length overflow")?;
    ensure!(length <= MAX_PACKET_BYTES, "Lua worker packet is too large");
    let length = u32::try_from(length).context("Lua worker packet is too large")?;
    let mut packet = Vec::with_capacity(4 + length as usize);
    packet.extend_from_slice(&length.to_be_bytes());
    packet.push(kind);
    packet.extend_from_slice(prefix);
    packet.extend_from_slice(payload);
    let mut written = 0;
    while written < packet.len() {
        match stream.write(&packet[written..]) {
            Ok(0) => bail!("Lua worker control socket closed while writing"),
            Ok(count) => written += count,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("Lua worker control socket write timed out")
                }
                thread::sleep(WRITE_RETRY);
            }
            Err(error) => return Err(error).context("writing Lua worker packet"),
        }
    }
    Ok(())
}

fn read_packets(stream: &mut UnixStream, input: &mut Vec<u8>) -> Result<Vec<(u8, Vec<u8>)>> {
    let mut bytes = [0u8; 8192];
    let mut closed = false;
    loop {
        match stream.read(&mut bytes) {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(count) => {
                ensure!(
                    input.len().saturating_add(count) <= MAX_PACKET_BYTES + 4,
                    "Lua worker packet buffer is full"
                );
                input.extend_from_slice(&bytes[..count]);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => return Err(error).context("reading Lua worker packet"),
        }
    }
    let mut packets = Vec::new();
    loop {
        if input.len() < 4 {
            break;
        }
        let length = u32::from_be_bytes(input[..4].try_into()?) as usize;
        ensure!(
            length > 0 && length <= MAX_PACKET_BYTES,
            "invalid Lua worker packet length"
        );
        if input.len() < 4 + length {
            break;
        }
        let packet: Vec<_> = input.drain(..4 + length).collect();
        packets.push((packet[4], packet[5..].to_vec()));
    }
    if closed && !input.is_empty() {
        bail!("Lua worker control socket closed in the middle of a packet");
    }
    Ok(packets)
}

fn send_ready(stream: &mut UnixStream, status: u8, error: String) -> Result<()> {
    let body = if status == STATUS_OK {
        vec![status]
    } else {
        let mut body = vec![status];
        body.extend(encode_error(&error));
        body
    };
    write_packet_blocking(stream, READY, &body)
}

fn split_reply(payload: &[u8]) -> Result<(u8, Vec<u8>)> {
    let (command, payload) = payload.split_first().context("Lua worker reply is empty")?;
    ensure!(!payload.is_empty(), "Lua worker reply has no status");
    Ok((*command, payload.to_vec()))
}

fn response_error(context: &str, response: &[u8]) -> anyhow::Error {
    match response {
        [STATUS_ERROR, rest @ ..] => anyhow!("{context}: {}", decode_error(rest)),
        _ => anyhow!("{context}: malformed Lua worker response"),
    }
}

fn parse_status_response(context: &str, response: &[u8]) -> Result<()> {
    match response {
        [STATUS_OK] => Ok(()),
        [STATUS_ERROR, rest @ ..] => bail!("{context}: {}", decode_error(rest)),
        _ => bail!("{context}: malformed Lua worker response"),
    }
}

fn parse_optional_f64_response(context: &str, response: &[u8]) -> Result<Option<f64>> {
    if let [STATUS_OK, present, rest @ ..] = response {
        ensure!(
            rest.len() == if *present == 0 { 0 } else { 8 },
            "{context}: malformed response"
        );
        return if *present == 0 {
            Ok(None)
        } else {
            Ok(Some(f64::from_bits(u64::from_be_bytes(rest.try_into()?))))
        };
    }
    if let [STATUS_ERROR, rest @ ..] = response {
        bail!("{context}: {}", decode_error(rest));
    }
    bail!("{context}: malformed Lua worker response")
}

fn parse_frame_response(response: &[u8]) -> Result<Option<FrameTiming>> {
    match response {
        [STATUS_OK, rest @ ..] => decode_frame_response(rest),
        [STATUS_ERROR, rest @ ..] => bail!("{}", decode_error(rest)),
        _ => bail!("malformed Lua worker frame response"),
    }
}

fn parse_effects_response(response: &[u8]) -> Result<ProcessEffects> {
    match response {
        [STATUS_OK, rest @ ..] => decode_effects(rest),
        [STATUS_ERROR, rest @ ..] => bail!("{}", decode_error(rest)),
        _ => bail!("malformed Lua worker effects response"),
    }
}

fn encode_source(output: &mut Vec<u8>, source: &LuaSource) -> Result<()> {
    match source {
        LuaSource::File(path) => {
            output.push(0);
            let bytes = path.as_os_str().as_bytes();
            put_bytes(output, bytes)?;
        }
        LuaSource::Embedded(bytes) => {
            output.push(1);
            put_bytes(output, bytes)?;
        }
    }
    Ok(())
}

fn decode_bootstrap(payload: &[u8]) -> Result<(LuaSource, PathBuf, f64, InputState)> {
    let mut reader = Reader::new(payload);
    let kind = reader.u8()?;
    let bytes = reader.bytes()?;
    let source = match kind {
        0 => LuaSource::File(PathBuf::from(std::ffi::OsString::from_vec(bytes))),
        1 => LuaSource::embedded(bytes),
        other => bail!("unknown Lua worker source kind {other}"),
    };
    let frame_path = PathBuf::from(std::ffi::OsString::from_vec(reader.bytes()?));
    let backlight = reader.f64()?;
    let input = decode_input_state(&mut reader)?;
    reader.finish()?;
    Ok((source, frame_path, backlight, input))
}

fn encode_input_state(output: &mut Vec<u8>, state: InputState) {
    output.push(u8::from(state.fn_active));
    for modifier in Modifier::ALL {
        output.push(u8::from(state.modifiers.is_active(modifier)));
    }
}

fn decode_input_state(reader: &mut Reader<'_>) -> Result<InputState> {
    let fn_active = reader.bool()?;
    let mut modifiers = crate::hardware::ModifierState::default();
    for modifier in Modifier::ALL {
        modifiers.set(modifier, reader.bool()?);
    }
    Ok(InputState {
        fn_active,
        modifiers,
    })
}

fn encode_drive_request(output: &mut Vec<u8>, request: DriveRequest) -> Result<()> {
    let super::DriveRequest {
        now_seconds,
        input_state,
        transitions,
        delta,
        events,
        options,
    } = request;
    put_f64(output, now_seconds);
    encode_input_state(output, input_state);
    put_u32(output, transitions.len())?;
    for transition in transitions {
        encode_transition(output, transition);
    }
    put_f64(output, delta);
    put_u32(output, events.len())?;
    for event in events {
        encode_touch(output, event);
    }
    match options.visibility {
        Some((visible, reason)) => {
            output.push(1);
            output.push(u8::from(visible));
            output.push(match reason {
                VisibilityReason::Recovery => 0,
                VisibilityReason::Suspend => 1,
                VisibilityReason::Device => 2,
            });
        }
        None => output.push(0),
    }
    output.push(u8::from(options.force_render));
    output.push(u8::from(options.resume_timers));
    Ok(())
}

fn decode_drive_request(payload: &[u8]) -> Result<DriveRequest> {
    let mut reader = Reader::new(payload);
    let now_seconds = reader.f64()?;
    let input_state = decode_input_state(&mut reader)?;
    let transition_count = reader.count()?;
    let mut transitions = Vec::with_capacity(transition_count);
    for _ in 0..transition_count {
        transitions.push(decode_transition(&mut reader)?);
    }
    let delta = reader.f64()?;
    let event_count = reader.count()?;
    let mut events = Vec::with_capacity(event_count);
    for _ in 0..event_count {
        events.push(decode_touch(&mut reader)?);
    }
    let visibility = if reader.bool()? {
        let visible = reader.bool()?;
        let reason = match reader.u8()? {
            0 => VisibilityReason::Recovery,
            1 => VisibilityReason::Suspend,
            2 => VisibilityReason::Device,
            reason => bail!("unknown Lua worker visibility reason {reason}"),
        };
        Some((visible, reason))
    } else {
        None
    };
    let force_render = reader.bool()?;
    let resume_timers = reader.bool()?;
    reader.finish()?;
    let mut request = DriveRequest::new(now_seconds, input_state, transitions, delta, events);
    if let Some((visible, reason)) = visibility {
        request = request.with_visibility(visible, reason, force_render);
    } else if force_render {
        request.options.force_render = true;
    }
    if resume_timers {
        request = request.with_timer_resume();
    }
    Ok(request)
}

fn encode_transition(output: &mut Vec<u8>, transition: InputTransition) {
    output.push(match transition.key {
        ObservedKey::Fn => 0,
        ObservedKey::Modifier(modifier) => 1 + modifier_index(modifier),
    });
    output.push(u8::from(transition.active));
    encode_input_state(output, transition.state);
}

fn decode_transition(reader: &mut Reader<'_>) -> Result<InputTransition> {
    let key = match reader.u8()? {
        0 => ObservedKey::Fn,
        value @ 1..=8 => ObservedKey::Modifier(Modifier::ALL[usize::from(value - 1)]),
        value => bail!("unknown Lua worker input transition key {value}"),
    };
    let active = reader.bool()?;
    let state = decode_input_state(reader)?;
    Ok(InputTransition { key, active, state })
}

fn encode_touch(output: &mut Vec<u8>, event: TouchEvent) {
    output.push(match event.phase {
        TouchPhase::Down => 0,
        TouchPhase::Move => 1,
        TouchPhase::Up => 2,
        TouchPhase::Cancel => 3,
    });
    put_u32(output, event.id as usize).expect("touch id has a fixed width");
    put_f64(output, event.time);
    put_f64(output, event.x);
    put_f64(output, event.y);
    encode_input_state(
        output,
        InputState {
            fn_active: false,
            modifiers: event.modifiers,
        },
    );
    for value in [event.pressure, event.width, event.height] {
        match value {
            Some(value) => {
                output.push(1);
                put_f64(output, value);
            }
            None => output.push(0),
        }
    }
}

fn decode_touch(reader: &mut Reader<'_>) -> Result<TouchEvent> {
    let phase = match reader.u8()? {
        0 => TouchPhase::Down,
        1 => TouchPhase::Move,
        2 => TouchPhase::Up,
        3 => TouchPhase::Cancel,
        value => bail!("unknown Lua worker touch phase {value}"),
    };
    let id = reader.u32()?;
    let time = reader.f64()?;
    let x = reader.f64()?;
    let y = reader.f64()?;
    let state = decode_input_state(reader)?;
    let mut optional = || -> Result<Option<f64>> {
        if reader.bool()? {
            Ok(Some(reader.f64()?))
        } else {
            Ok(None)
        }
    };
    Ok(TouchEvent {
        phase,
        id,
        time,
        x,
        y,
        modifiers: state.modifiers,
        pressure: optional()?,
        width: optional()?,
        height: optional()?,
    })
}

fn encode_frame_response(frame: Option<FrameTiming>) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    match frame {
        Some(timing) => {
            body.push(1);
            encode_timing(&mut body, timing);
        }
        None => body.push(0),
    }
    Ok(body)
}

fn encode_effects_response(effects: super::RuntimeEffects) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    match effects.frame {
        Some(timing) => {
            body.push(1);
            encode_timing(&mut body, timing);
        }
        None => body.push(0),
    }
    match effects.backlight {
        Some(level) => {
            body.push(1);
            put_f64(&mut body, level);
        }
        None => body.push(0),
    }
    put_u32(&mut body, effects.key_requests.len())?;
    for request in effects.key_requests {
        body.push(match request.operation {
            KeyOperation::Down => 0,
            KeyOperation::Up => 1,
            KeyOperation::Tap => 2,
        });
        encode_key(&mut body, request.key);
        match request.modifiers {
            ModifierMode::Inherit => body.push(0),
            ModifierMode::None => body.push(1),
            ModifierMode::Explicit(keys) => {
                body.push(2);
                put_u32(&mut body, keys.len())?;
                for key in keys {
                    encode_key(&mut body, key);
                }
            }
        }
    }
    match effects.next_worker_deadline {
        Some(deadline) => {
            body.push(1);
            put_f64(&mut body, deadline);
        }
        None => body.push(0),
    }
    body.push(u8::from(effects.redraw_pending));
    Ok(body)
}

fn decode_effects(payload: &[u8]) -> Result<ProcessEffects> {
    let mut reader = Reader::new(payload);
    let frame = if reader.bool()? {
        Some(decode_timing(&mut reader)?)
    } else {
        None
    };
    let backlight = if reader.bool()? {
        Some(reader.f64()?)
    } else {
        None
    };
    let count = reader.count()?;
    let mut key_requests = Vec::with_capacity(count);
    for _ in 0..count {
        let operation = match reader.u8()? {
            0 => KeyOperation::Down,
            1 => KeyOperation::Up,
            2 => KeyOperation::Tap,
            value => bail!("unknown Lua worker key operation {value}"),
        };
        let key = decode_key(&mut reader)?;
        let modifiers = match reader.u8()? {
            0 => ModifierMode::Inherit,
            1 => ModifierMode::None,
            2 => {
                let count = reader.count()?;
                let mut keys = Vec::with_capacity(count);
                for _ in 0..count {
                    keys.push(decode_key(&mut reader)?);
                }
                ModifierMode::Explicit(keys)
            }
            value => bail!("unknown Lua worker modifier mode {value}"),
        };
        key_requests.push(KeyRequest {
            operation,
            key,
            modifiers,
        });
    }
    let next_worker_deadline = if reader.bool()? {
        Some(reader.f64()?)
    } else {
        None
    };
    let redraw_pending = reader.bool()?;
    reader.finish()?;
    Ok(ProcessEffects {
        frame,
        backlight,
        key_requests,
        next_worker_deadline,
        redraw_pending,
    })
}

fn encode_timing(output: &mut Vec<u8>, timing: FrameTiming) {
    put_f64(output, timing.presentation_time);
    put_f64(output, timing.delta);
}

fn decode_frame_response(payload: &[u8]) -> Result<Option<FrameTiming>> {
    let mut reader = Reader::new(payload);
    let frame = if reader.bool()? {
        Some(decode_timing(&mut reader)?)
    } else {
        None
    };
    reader.finish()?;
    Ok(frame)
}

fn decode_timing(reader: &mut Reader<'_>) -> Result<FrameTiming> {
    FrameTiming::new(reader.f64()?, reader.f64()?)
}

fn encode_key(output: &mut Vec<u8>, key: OutputKey) {
    match key {
        OutputKey::Keyboard(key) => {
            output.push(0);
            output.push(key as u8);
        }
        OutputKey::Consumer(key) => {
            output.push(1);
            output.push(key as u8);
        }
    }
}

fn decode_key(reader: &mut Reader<'_>) -> Result<OutputKey> {
    let kind = reader.u8()?;
    let value = reader.u8()?;
    match kind {
        0 => Ok(OutputKey::Keyboard(
            crate::hardware::KeyboardKey::from_wire(value)?,
        )),
        1 => Ok(OutputKey::Consumer(
            crate::hardware::ConsumerKey::from_wire(value)?,
        )),
        other => bail!("unknown Lua worker output key kind {other}"),
    }
}

fn encode_error(error: &str) -> Vec<u8> {
    let mut output = Vec::new();
    put_bytes(&mut output, error.as_bytes()).expect("error length has a bounded wire format");
    output
}

fn decode_error(payload: &[u8]) -> String {
    let mut reader = Reader::new(payload);
    reader
        .bytes()
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| "malformed Lua worker error".into())
}

fn stop_reason_code(reason: StopReason) -> u8 {
    match reason {
        StopReason::Replaced => 0,
        StopReason::Logout => 1,
        StopReason::Shutdown => 2,
    }
}

fn decode_stop_reason(payload: &[u8]) -> Result<StopReason> {
    ensure!(
        payload.len() == 1,
        "Lua worker stop request has an invalid payload"
    );
    match payload[0] {
        0 => Ok(StopReason::Replaced),
        1 => Ok(StopReason::Logout),
        2 => Ok(StopReason::Shutdown),
        value => bail!("unknown Lua worker stop reason {value}"),
    }
}

fn modifier_index(modifier: Modifier) -> u8 {
    Modifier::ALL
        .iter()
        .position(|candidate| *candidate == modifier)
        .expect("all modifiers have wire values") as u8
}

fn put_u32(output: &mut Vec<u8>, value: usize) -> Result<()> {
    output.extend_from_slice(
        &u32::try_from(value)
            .context("Lua worker collection is too large")?
            .to_be_bytes(),
    );
    Ok(())
}

fn put_f64(output: &mut Vec<u8>, value: f64) {
    output.extend_from_slice(&value.to_bits().to_be_bytes());
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    put_u32(output, bytes.len())?;
    output.extend_from_slice(bytes);
    Ok(())
}

fn lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> Result<MutexGuard<'a, T>> {
    mutex.lock().map_err(|_| anyhow!("{name} was poisoned"))
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(count)
            .context("Lua worker packet offset overflow")?;
        ensure!(end <= self.bytes.len(), "Lua worker packet is truncated");
        let bytes = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => bail!("Lua worker boolean has value {value}"),
        }
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(u64::from_be_bytes(
            self.take(8)?.try_into()?,
        )))
    }

    fn bytes(&mut self) -> Result<Vec<u8>> {
        let length = usize::try_from(self.u32()?).context("Lua worker byte length is invalid")?;
        Ok(self.take(length)?.to_vec())
    }

    fn count(&mut self) -> Result<usize> {
        let count = usize::try_from(self.u32()?).context("Lua worker count is invalid")?;
        ensure!(
            count <= MAX_PACKET_BYTES,
            "Lua worker collection is too large"
        );
        Ok(count)
    }

    fn finish(self) -> Result<()> {
        ensure!(
            self.offset == self.bytes.len(),
            "Lua worker packet has trailing bytes"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn embedded(source: &str) -> LuaSource {
        LuaSource::embedded(source.as_bytes().to_vec())
    }

    #[test]
    fn truncated_worker_packet_is_rejected() -> Result<()> {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (mut sender, mut receiver) = UnixStream::pair()?;
        sender.write_all(&100u32.to_be_bytes())?;
        sender.write_all(&[HELLO])?;
        sender.shutdown(std::net::Shutdown::Write)?;
        let error = read_packets(&mut receiver, &mut Vec::new())
            .expect_err("truncated worker packet was accepted");
        assert!(error.to_string().contains("middle of a packet"));
        Ok(())
    }

    #[test]
    fn process_worker_renders_and_stops_with_a_reason() -> Result<()> {
        let worker = ProcessWorker::stage(
            &embedded(
                r#"
                local sliver = require("sliver.v1")
                return {
                    api_version = 1,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                    end,
                }
                "#,
            ),
            0.0,
            InputState::default(),
        )?;
        let frame = worker.render(1.0, 0.0, InputState::default())?;
        assert_eq!((frame.frame.width(), frame.frame.height()), (2008, 60));
        assert_eq!(
            &frame.frame.pixels()[10 * frame.frame.stride() + 10 * 4..][..4],
            &[0, 0, 255, 255]
        );
        worker.shutdown(StopReason::Replaced)
    }

    #[test]
    fn process_worker_loads_a_pure_lua_module_from_the_entry_directory() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(
            directory.path().join("helper.lua"),
            "return { red = 1, green = 0, blue = 0 }",
        )?;
        let source = directory.path().join("entry.lua");
        std::fs::write(
            &source,
            r#"
            local color = require("helper")
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, color.red, color.green, color.blue, 1)
                end,
            }
            "#,
        )?;
        let worker = ProcessWorker::stage(&LuaSource::file(source), 0.0, InputState::default())?;
        let frame = worker.render(1.0, 0.0, InputState::default())?;
        assert_eq!(
            &frame.frame.pixels()[10 * frame.frame.stride() + 10 * 4..][..4],
            &[0, 0, 255, 255]
        );
        worker.shutdown(StopReason::Shutdown)
    }

    #[test]
    fn systemd_worker_uses_the_declared_resource_and_device_policy() -> Result<()> {
        let available = std::process::Command::new("systemd-run")
            .args(["--user", "--wait", "--quiet", "true"])
            .status();
        anyhow::ensure!(
            available.is_ok_and(|status| status.success()),
            "systemd user manager is required for this worker policy test"
        );

        let _directory = tempfile::tempdir()?;
        let frame_path = frame_path()?;
        let slots = FrameSlots::new_shared(
            &frame_path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let worker = ProcessWorker::stage_with_frames_systemd(
            &embedded(
                r#"
                require("sliver.v1")
                return { api_version = 1, render = function() end }
                "#,
            ),
            0.0,
            InputState::default(),
            &frame_path,
            slots.broker(),
            WorkerIdentity::RestrictedFallback,
        )?;
        let unit = worker.unit.clone().context("systemd worker had no unit")?;
        let properties = std::process::Command::new("systemctl")
            .args(["--user", "show", &unit])
            .output()
            .context("reading systemd worker properties")?;
        let properties = String::from_utf8_lossy(&properties.stdout);
        assert!(properties.contains("MemoryMax=536870912"));
        assert!(properties.contains("TasksMax=64"));
        assert!(properties.contains("PrivateDevices=yes"));
        assert!(properties.contains("DevicePolicy=closed"));
        worker.shutdown(StopReason::Shutdown)
    }

    #[test]
    fn graceful_stop_delivers_each_allowed_reason() -> Result<()> {
        for (reason, expected) in [
            (StopReason::Replaced, "replaced"),
            (StopReason::Logout, "logout"),
            (StopReason::Shutdown, "shutdown"),
        ] {
            let directory = tempfile::tempdir()?;
            let marker = directory.path().join("stop-reason");
            let source = format!(
                r#"
                local sliver = require("sliver.v1")
                return {{
                    api_version = 1,
                    stop = function(reason)
                        local file = assert(io.open({marker:?}, "w"))
                        file:write(reason)
                        file:close()
                    end,
                    render = function() end,
                }}
                "#,
                marker = marker.to_string_lossy(),
            );
            let worker = ProcessWorker::stage(&embedded(&source), 0.0, InputState::default())?;
            worker.shutdown(reason)?;
            assert_eq!(std::fs::read_to_string(marker)?, expected);
        }
        Ok(())
    }

    #[test]
    fn a_vendored_lua_c_module_runs_inside_the_worker_process() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let c_source = directory.path().join("native_probe.c");
        let module = directory.path().join("native_probe.so");
        std::fs::write(
            &c_source,
            r#"
            #include <stddef.h>
            typedef struct lua_State lua_State;
            typedef long long lua_Integer;
            typedef double lua_Number;
            extern void luaL_checkversion_(lua_State *, lua_Number, size_t);
            extern lua_Number lua_version(lua_State *);
            extern void lua_pushinteger(lua_State *, lua_Integer);
            int luaopen_native_probe(lua_State *state) {
                luaL_checkversion_(state, 504.0, sizeof(lua_Integer) * 16 + sizeof(lua_Number));
                lua_pushinteger(state, (lua_Integer)lua_version(state));
                return 1;
            }
            "#,
        )?;
        let compile = std::process::Command::new("cc")
            .args(["-shared", "-fPIC", "-o"])
            .arg(&module)
            .arg(&c_source)
            .status()
            .context("compiling Lua C-module probe")?;
        anyhow::ensure!(compile.success(), "C-module probe did not compile");
        let source = directory.path().join("native.lua");
        std::fs::write(
            &source,
            r#"
            local probe = require("native_probe")
            require("sliver.v1")
            assert(probe == 504)
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1)
                end,
            }
            "#,
        )?;
        let worker = ProcessWorker::stage(&LuaSource::file(source), 0.0, InputState::default())?;
        let frame = worker.render(1.0, 0.0, InputState::default())?;
        assert_eq!(
            &frame.frame.pixels()[10 * frame.frame.stride() + 10 * 4..][..4],
            &[255, 0, 0, 255]
        );
        worker.shutdown(StopReason::Shutdown)
    }

    #[test]
    fn a_blocking_c_module_is_killed_without_running_stop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let c_source = directory.path().join("blocking_probe.c");
        let module = directory.path().join("blocking_probe.so");
        std::fs::write(
            &c_source,
            r#"
            #include <stddef.h>
            #include <unistd.h>
            typedef struct lua_State lua_State;
            typedef long long lua_Integer;
            typedef double lua_Number;
            extern void luaL_checkversion_(lua_State *, lua_Number, size_t);
            extern void lua_createtable(lua_State *, int, int);
            extern void lua_pushcclosure(lua_State *, int (*)(lua_State *), int);
            extern void lua_setfield(lua_State *, int, const char *);
            static int block(lua_State *state) {
                (void)state;
                sleep(10);
                return 0;
            }
            int luaopen_blocking_probe(lua_State *state) {
                luaL_checkversion_(state, 504.0, sizeof(lua_Integer) * 16 + sizeof(lua_Number));
                lua_createtable(state, 0, 1);
                lua_pushcclosure(state, block, 0);
                lua_setfield(state, -2, "block");
                return 1;
            }
            "#,
        )?;
        let compile = std::process::Command::new("cc")
            .args(["-shared", "-fPIC", "-o"])
            .arg(&module)
            .arg(&c_source)
            .status()
            .context("compiling blocking Lua C-module probe")?;
        anyhow::ensure!(compile.success(), "blocking C-module probe did not compile");
        let source = directory.path().join("blocking.lua");
        std::fs::write(
            &source,
            r#"
            local probe = require("blocking_probe")
            require("sliver.v1")
            return {
                api_version = 1,
                render = function()
                    probe.block()
                end,
            }
            "#,
        )?;
        let worker = ProcessWorker::stage(&LuaSource::file(source), 0.0, InputState::default())?;
        let started = Instant::now();
        let error = match worker.render(1.0, 0.0, InputState::default()) {
            Ok(_) => bail!("blocking C callback returned"),
            Err(error) => error,
        };
        assert!(started.elapsed() >= CALLBACK_DEADLINE);
        assert!(error.to_string().contains("two seconds"));
        Ok(())
    }

    #[test]
    fn an_idle_worker_that_stops_heartbeating_is_killed() -> Result<()> {
        let worker = ProcessWorker::stage(
            &embedded(
                r#"
                require("sliver.v1")
                return { api_version = 1, render = function() end }
                "#,
            ),
            0.0,
            InputState::default(),
        )?;
        unsafe {
            libc::kill(worker.pid, libc::SIGSTOP);
        }
        thread::sleep(CALLBACK_DEADLINE + Duration::from_millis(50));
        assert!(!worker.is_alive());
        Ok(())
    }

    #[test]
    fn a_blocking_process_call_is_killed_without_running_stop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("stop-called");
        let source = format!(
            r#"
            local sliver = require("sliver.v1")
            return {{
                api_version = 1,
                stop = function()
                    local file = assert(io.open({marker:?}, "w"))
                    file:close()
                end,
                render = function()
                    os.execute("sleep 10")
                end,
            }}
            "#,
            marker = marker.to_string_lossy(),
        );
        let worker = ProcessWorker::stage(&embedded(&source), 0.0, InputState::default())?;
        let started = Instant::now();
        let error = match worker.render(1.0, 0.0, InputState::default()) {
            Ok(_) => bail!("blocking process call returned"),
            Err(error) => error,
        };
        assert!(started.elapsed() >= CALLBACK_DEADLINE);
        assert!(error.to_string().contains("two seconds"));
        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn a_hung_render_is_killed_without_running_stop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("stop-called");
        let source = format!(
            r#"
            local sliver = require("sliver.v1")
            return {{
                api_version = 1,
                stop = function()
                    local file = assert(io.open({marker:?}, "w"))
                    file:close()
                end,
                render = function()
                    while true do end
                end,
            }}
            "#,
            marker = marker.to_string_lossy(),
        );
        let worker = ProcessWorker::stage(&embedded(&source), 0.0, InputState::default())?;
        let started = Instant::now();
        let error = match worker.render(1.0, 0.0, InputState::default()) {
            Ok(_) => bail!("hung Lua callback returned"),
            Err(error) => error,
        };
        assert!(started.elapsed() >= CALLBACK_DEADLINE);
        assert!(error.to_string().contains("two seconds"));
        assert!(!worker.is_alive());
        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn a_hung_stop_is_killed_at_the_graceful_stop_deadline() -> Result<()> {
        let source = embedded(
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                stop = function(reason)
                    assert(reason == "shutdown")
                    while true do end
                end,
                render = function() end,
            }
            "#,
        );
        let worker = ProcessWorker::stage(&source, 0.0, InputState::default())?;
        let started = Instant::now();
        let error = match worker.shutdown(StopReason::Shutdown) {
            Ok(()) => bail!("hung stop callback returned"),
            Err(error) => error,
        };
        let elapsed = started.elapsed();
        assert!(elapsed >= STOP_DEADLINE);
        assert!(elapsed < CALLBACK_DEADLINE);
        assert!(error.to_string().contains("stop exceeded 500 milliseconds"));
        Ok(())
    }
}
