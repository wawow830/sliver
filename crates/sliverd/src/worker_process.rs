use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
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
use crate::diagnostic_capture_transport::{CaptureLevel, FixtureBootstrap, FixtureControl};
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
const FIXTURE_CONTROL: u8 = 7;

const STATUS_OK: u8 = 0;
const STATUS_ERROR: u8 = 1;

static NEXT_UNIT_ID: AtomicU64 = AtomicU64::new(1);

struct SpawnedWorker {
    child: Child,
    stream: UnixStream,
    process_group: libc::pid_t,
    unit: Option<String>,
    cgroup: Option<PathBuf>,
    systemd_runtime: Option<PathBuf>,
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
    cgroup: Option<PathBuf>,
    systemd_runtime: Option<PathBuf>,
    broker: FrameBroker,
    // Exact immutable capability sent at bootstrap. This prevents accidental
    // same-process plan mismatch; it is NOT installed-build/source attestation.
    fixture_plan: Option<FixtureBootstrap>,
}

impl ProcessWorker {
    #[allow(dead_code)] // Private coordinator only; ordinary workers retain None.
    pub(crate) fn matches_fixture_plan(&self, plan: &FixtureBootstrap) -> bool {
        self.fixture_plan.as_ref() == Some(plan)
    }

    #[cfg(test)]
    pub(crate) fn stage_with_fixture(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        storage: OwnedFd,
        plan: FixtureBootstrap,
    ) -> Result<Self> {
        let path = frame_path_for_identity(WorkerIdentity::User)?;
        let slots = FrameSlots::new_shared(
            &path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let level = plan.capture_level();
        Self::stage_with_spawned_capture(
            source,
            initial_backlight,
            initial_input,
            &path,
            slots.broker(),
            WorkerIdentity::User,
            spawn_direct,
            Some((storage, level)),
            Some(plan),
        )
    }

    #[allow(dead_code)] // Only the private coordinator uses this; no service arming.
    pub(crate) fn fixture_control(&self, control: FixtureControl) -> Result<()> {
        let mut payload = Vec::new();
        match control {
            FixtureControl::ConfirmWarmupClosed => payload.push(2),
            FixtureControl::Finish => payload.push(3),
            FixtureControl::Receive(receipt) => {
                payload.push(0);
                payload.extend_from_slice(&receipt.sequence.to_be_bytes());
                payload.extend_from_slice(&receipt.received_ns.to_be_bytes());
                encode_touch(&mut payload, receipt.event);
            }
            FixtureControl::Resolve {
                frame_id,
                resolution,
                resolved_ns,
            } => {
                payload.push(1);
                payload.extend_from_slice(&frame_id.to_be_bytes());
                payload.push(match resolution {
                    crate::diagnostic_fixture::Resolution::Presented => 0,
                    crate::diagnostic_fixture::Resolution::Discarded => 1,
                    crate::diagnostic_fixture::Resolution::Failed => 2,
                });
                payload.extend_from_slice(&resolved_ns.to_be_bytes());
            }
        }
        let response = self.request(FIXTURE_CONTROL, payload, CALLBACK_DEADLINE)?;
        self.terminal_response(parse_status_response(
            "controlling private fixture",
            &response,
        ))
    }

    #[cfg(test)]
    pub(crate) fn stage(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
    ) -> Result<Self> {
        let path = frame_path_for_identity(WorkerIdentity::User)?;
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

    #[cfg(test)]
    pub(crate) fn stage_observed(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        storage: OwnedFd,
    ) -> Result<Self> {
        Self::stage_with_capture(
            source,
            initial_backlight,
            initial_input,
            storage,
            CaptureLevel::Detailed,
        )
    }

    #[cfg(test)]
    pub(crate) fn stage_with_capture(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        storage: OwnedFd,
        level: CaptureLevel,
    ) -> Result<Self> {
        let path = frame_path_for_identity(WorkerIdentity::User)?;
        let slots = FrameSlots::new_shared(
            &path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        Self::stage_with_spawned_capture(
            source,
            initial_backlight,
            initial_input,
            &path,
            slots.broker(),
            WorkerIdentity::User,
            spawn_direct,
            Some((storage, level)),
            None,
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
        Self::stage_with_spawned_capture(
            source,
            initial_backlight,
            initial_input,
            frame_path,
            broker,
            identity,
            spawn,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_with_spawned_capture(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        frame_path: &Path,
        broker: FrameBroker,
        identity: WorkerIdentity,
        spawn: fn(WorkerIdentity) -> Result<SpawnedWorker>,
        storage: Option<(OwnedFd, CaptureLevel)>,
        fixture: Option<FixtureBootstrap>,
    ) -> Result<Self> {
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("enabling Lua worker child reaping");
        }
        let mut spawned = spawn(identity)?;
        if let Err(error) = spawned.stream.set_nonblocking(true) {
            terminate_spawned(&mut spawned);
            return Err(error).context("configuring Lua worker control socket");
        }
        let worker_pid = match receive_hello(&mut spawned.stream, &mut Vec::new()) {
            Ok(pid) => pid,
            Err(error) => {
                terminate_spawned(&mut spawned);
                return Err(error);
            }
        };
        let pidfd = match open_pidfd(worker_pid) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                terminate_spawned(&mut spawned);
                return Err(error).context("opening the Lua worker pidfd");
            }
        };
        let worker = Self {
            child: Mutex::new(spawned.child),
            pidfd,
            stream: Mutex::new(spawned.stream),
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
            cgroup: spawned.cgroup,
            systemd_runtime: spawned.systemd_runtime,
            broker,
            fixture_plan: fixture.clone(),
        };
        worker.request_bootstrap(
            source,
            initial_backlight,
            initial_input,
            frame_path,
            storage,
            fixture.as_ref(),
        )?;
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
        cgroup: None,
        systemd_runtime: None,
    })
}

fn spawn_systemd(identity: WorkerIdentity) -> Result<SpawnedWorker> {
    use std::os::unix::fs::PermissionsExt;

    // A system service's %U specifier resolves to UID 0 even when User= is
    // set. The fallback must address the broker account's lingering manager,
    // not the system manager's runtime directory.
    let runtime = runtime_directory(identity);
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
    let systemd_runtime = (identity == WorkerIdentity::RestrictedFallback).then(|| runtime.clone());
    configure_systemd_user_environment(&mut launcher, systemd_runtime.as_deref());
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
    let cgroup = match systemd_unit_cgroup(&unit, systemd_runtime.as_deref()) {
        Ok(cgroup) => Some(cgroup),
        Err(error) => {
            terminate_spawned_parts(&mut child, 0, None, Some(&unit), systemd_runtime.as_deref());
            let _ = std::fs::remove_file(&socket_path);
            return Err(error);
        }
    };
    let deadline = Instant::now() + CALLBACK_DEADLINE;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    terminate_spawned_parts(
                        &mut child,
                        0,
                        cgroup.as_deref(),
                        Some(&unit),
                        systemd_runtime.as_deref(),
                    );
                    let _ = std::fs::remove_file(&socket_path);
                    bail!("Lua worker service did not connect within two seconds")
                }
                thread::sleep(WRITE_RETRY);
            }
            Err(error) => {
                terminate_spawned_parts(
                    &mut child,
                    0,
                    cgroup.as_deref(),
                    Some(&unit),
                    systemd_runtime.as_deref(),
                );
                let _ = std::fs::remove_file(&socket_path);
                return Err(error).context("accepting the Lua worker connection");
            }
        }
    };
    let _ = std::fs::remove_file(&socket_path);
    if let Err(error) = stream.set_nonblocking(true) {
        terminate_spawned_parts(
            &mut child,
            0,
            cgroup.as_deref(),
            Some(&unit),
            systemd_runtime.as_deref(),
        );
        return Err(error).context("configuring Lua worker control socket");
    }
    Ok(SpawnedWorker {
        child,
        stream,
        process_group: 0,
        cgroup,
        unit: Some(unit),
        systemd_runtime,
    })
}

fn terminate_spawned(spawned: &mut SpawnedWorker) {
    terminate_spawned_parts(
        &mut spawned.child,
        spawned.process_group,
        spawned.cgroup.as_deref(),
        spawned.unit.as_deref(),
        spawned.systemd_runtime.as_deref(),
    );
}

fn terminate_spawned_parts(
    child: &mut Child,
    process_group: libc::pid_t,
    cgroup: Option<&Path>,
    unit: Option<&str>,
    systemd_runtime: Option<&Path>,
) {
    let mut descendants = cgroup.map(cgroup_processes).unwrap_or_default();
    if process_group > 1 {
        descendants.extend(descendants_of(child.id() as libc::pid_t));
    }
    let cgroup_killed = cgroup.is_some_and(kill_cgroup);
    if !cgroup_killed {
        if let Some(unit) = unit {
            kill_systemd_unit(unit, systemd_runtime);
        }
    }
    kill_process_group_id(process_group);
    child.kill().ok();
    let deadline = Instant::now() + KILL_REAP_DEADLINE;
    while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        thread::sleep(WRITE_RETRY);
    }
    reap_pids_until(descendants, deadline);
}

fn configure_systemd_user_environment(command: &mut Command, runtime: Option<&Path>) {
    if let Some(runtime) = runtime {
        command.env("XDG_RUNTIME_DIR", runtime).env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/bus", runtime.display()),
        );
    }
}

fn systemd_unit_cgroup(unit: &str, runtime: Option<&Path>) -> Result<PathBuf> {
    let deadline = Instant::now() + CALLBACK_DEADLINE;
    loop {
        let mut command = Command::new("systemctl");
        configure_systemd_user_environment(&mut command, runtime);
        let output = command
            .args(["--user", "show", unit, "-p", "ControlGroup", "--value"])
            .output()
            .context("reading the Lua worker cgroup")?;
        if output.status.success() {
            let cgroup = String::from_utf8(output.stdout)?.trim().to_owned();
            if !cgroup.is_empty() {
                return Ok(PathBuf::from("/sys/fs/cgroup").join(cgroup.trim_start_matches('/')));
            }
        }
        if Instant::now() >= deadline {
            bail!("Lua worker unit has no cgroup")
        }
        thread::sleep(WRITE_RETRY);
    }
}

fn kill_cgroup(cgroup: &Path) -> bool {
    std::fs::write(cgroup.join("cgroup.kill"), b"1").is_ok()
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
        storage: Option<(OwnedFd, CaptureLevel)>,
        fixture: Option<&FixtureBootstrap>,
    ) -> Result<()> {
        let deadline = Instant::now() + CALLBACK_DEADLINE;
        {
            let stream = lock(&self.stream, "Lua worker control socket")?;
            crate::diagnostic_capture_transport::send_bootstrap_storage(
                &stream,
                storage.as_ref().map(|(fd, level)| (fd, *level)),
                deadline,
            )?;
        }
        let mut payload = Vec::new();
        encode_source(&mut payload, source)?;
        put_bytes(&mut payload, frame_path.as_os_str().as_bytes())?;
        put_f64(&mut payload, initial_backlight);
        encode_input_state(&mut payload, initial_input);
        encode_fixture_bootstrap(&mut payload, fixture)?;
        let response = self.request_until(BOOTSTRAP, payload, deadline)?;
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
        let cgroup_killed = self.cgroup.as_deref().is_some_and(kill_cgroup);
        if !cgroup_killed {
            if let Some(unit) = self.unit.as_deref() {
                kill_systemd_unit(unit, self.systemd_runtime.as_deref());
            }
        }
    }

    fn kill_process_group(&self) {
        for descendant in descendants_of(self.pid) {
            unsafe {
                libc::kill(descendant, libc::SIGKILL);
            }
        }
        kill_process_group_id(self.process_group);
    }

    pub(crate) fn terminate(&self) {
        self.terminate_until(Instant::now() + KILL_REAP_DEADLINE);
    }

    fn terminate_until(&self, _deadline: Instant) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        let cleanup_deadline = Instant::now() + KILL_REAP_DEADLINE;
        self.kill_unit();
        let mut descendants = Vec::new();
        loop {
            let current_descendants = descendants_of(self.pid);
            for descendant in &current_descendants {
                unsafe {
                    libc::kill(*descendant, libc::SIGKILL);
                }
            }
            descendants.extend(current_descendants);
            kill_process_group_id(self.process_group);
            send_pidfd_signal(&self.pidfd, libc::SIGKILL);
            if let Ok(mut child) = self.child.lock() {
                let _ = child.kill();
                if child.try_wait().ok().flatten().is_some() {
                    reap_pids_until(descendants, cleanup_deadline);
                    break;
                }
            }
            if Instant::now() >= cleanup_deadline {
                reap_pids_until(descendants, cleanup_deadline);
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
    // The parent starts its one bootstrap budget after launcher/cgroup
    // discovery, not when this child sends HELLO. Await its request or EOF.
    let storage = crate::diagnostic_capture_transport::receive_parent_bootstrap_storage(&stream)?;
    let capacity = storage.as_ref().map(|(storage, _)| storage.capacity());
    let level = storage.as_ref().map(|(_, level)| *level);
    let observer = storage
        .map(|(storage, _)| crate::diagnostic_observer::Capture::from_mapped(storage))
        .transpose()?;
    if let Some(observer) = &observer {
        observer.record(
            crate::diagnostic_observer::EventKind::WorkerCaptureConfigured {
                detailed: level == Some(CaptureLevel::Detailed),
            },
        );
    }
    let timing = observer
        .as_ref()
        .zip(capacity)
        .map(|(capture, capacity)| {
            crate::diagnostic_timing::TimingCapture::exporting(capacity, capture.clone())
        })
        .transpose()?;
    let result = run_bootstrapped_worker(&mut stream, observer.as_ref(), timing.as_ref(), level);
    if result.is_err() {
        if let Some(observer) = &observer {
            observer.record(crate::diagnostic_observer::EventKind::WorkerRunFailed);
        }
    }
    // Runtime and producers are now dropped. Timing emits its fixed summary
    // before raw closure; completed samples were appended at their actual seams.
    // No heap sample-vector flush or report serialization happens at shutdown.
    if let Some(timing) = timing {
        timing.close();
    }
    if let Some(observer) = observer {
        observer.finish();
    }
    result
}

fn run_bootstrapped_worker(
    stream: &mut UnixStream,
    observer: Option<&crate::diagnostic_observer::Capture>,
    timing: Option<&crate::diagnostic_timing::TimingCapture>,
    level: Option<CaptureLevel>,
) -> Result<()> {
    let mut input = Vec::new();
    let (source, frame_path, initial_backlight, initial_input, fixture_plan) = loop {
        let packets = read_packets(stream, &mut input)?;
        if let Some((kind, payload)) = packets.into_iter().next() {
            ensure!(kind == BOOTSTRAP, "Lua worker expected bootstrap packet");
            break decode_bootstrap(&payload)?;
        }
        thread::sleep(WRITE_RETRY);
    };

    let fixture = fixture_plan
        .map(|plan| -> Result<_> {
            ensure!(
                level == Some(plan.capture_level()),
                "fixture mode/storage level mismatch"
            );
            // All modes pay the identical complete minimal sampler cost before
            // fixture allocation, callbacks or arming. Retain actual probes;
            // source loss here is a setup failure, never implicit calibration.
            timing
                .context("fixture requires minimal timing")?
                .calibrate()?;
            let fixture = crate::diagnostic_fixture::Fixture::new(
                plan.plan()?,
                crate::diagnostic_fixture::Clock::HostMonotonic,
            )?;
            if plan.mode == crate::diagnostic_fixture::Mode::C {
                fixture
                    .attach_observer(observer.context("fixture C requires raw storage")?.clone())?;
            }
            Ok(fixture)
        })
        .transpose()?;
    if let LuaSource::File(path) = &source {
        let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::env::set_current_dir(directory)
            .with_context(|| format!("changing Lua worker directory to {}", directory.display()))?;
    }
    let slots = FrameSlots::open_shared(&frame_path)?;
    let slots = if let Some(observer) = observer.filter(|_| level == Some(CaptureLevel::Detailed)) {
        slots.with_observer(observer.clone())
    } else {
        slots
    };
    let slots = if let Some(timing) = timing {
        slots.with_timing(timing.clone())
    } else {
        slots
    };
    let producer = slots.producer();
    let runtime = match Runtime::load_inner(
        &source,
        initial_backlight,
        initial_input,
        producer,
        fixture.clone(),
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            if let Some(observer) = observer {
                observer.record(crate::diagnostic_observer::EventKind::WorkerLoadFailed);
            }
            eprintln!("Lua worker failed during startup: {error}");
            send_ready(stream, STATUS_ERROR, error)?;
            return Ok(());
        }
    };
    send_ready(stream, STATUS_OK, String::new())?;
    let result = run_worker_loop(runtime, stream, input, observer, fixture.as_ref());
    if result.is_err() {
        if let Some(observer) = observer {
            observer.record(crate::diagnostic_observer::EventKind::WorkerLoopFailed);
        }
    }
    result
}

fn send_hello(stream: &mut UnixStream) -> Result<()> {
    write_packet_blocking(stream, HELLO, &std::process::id().to_be_bytes())
}

fn run_worker_loop(
    mut runtime: Runtime,
    stream: &mut UnixStream,
    mut input: Vec<u8>,
    observer: Option<&crate::diagnostic_observer::Capture>,
    fixture: Option<&crate::diagnostic_fixture::Fixture>,
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
            let result = if *command == FIXTURE_CONTROL {
                handle_fixture_control(payload, fixture)
            } else {
                handle_command(*command, payload, &mut runtime)
            };
            let (status, body) = match result {
                Ok(body) => (STATUS_OK, body),
                Err(error) => {
                    if let Some(observer) = observer {
                        observer.record(crate::diagnostic_observer::EventKind::WorkerCommandFailed);
                    }
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

fn handle_fixture_control(
    payload: &[u8],
    fixture: Option<&crate::diagnostic_fixture::Fixture>,
) -> Result<Vec<u8>> {
    let fixture = fixture.context("worker has no private fixture capability")?;
    let mut reader = Reader::new(payload);
    let control = match reader.u8()? {
        0 => FixtureControl::Receive(crate::diagnostic_fixture::Receipt {
            sequence: reader.u64()?,
            received_ns: reader.u64()?,
            event: decode_touch(&mut reader)?,
        }),
        1 => FixtureControl::Resolve {
            frame_id: reader.u64()?,
            resolution: match reader.u8()? {
                0 => crate::diagnostic_fixture::Resolution::Presented,
                1 => crate::diagnostic_fixture::Resolution::Discarded,
                2 => crate::diagnostic_fixture::Resolution::Failed,
                _ => bail!("invalid fixture resolution"),
            },
            resolved_ns: reader.u64()?,
        },
        2 => FixtureControl::ConfirmWarmupClosed,
        3 => FixtureControl::Finish,
        _ => bail!("unknown private fixture control"),
    };
    reader.finish()?;
    match control {
        FixtureControl::ConfirmWarmupClosed => fixture.confirm_warmup_closed()?,
        FixtureControl::Finish => fixture.finish()?,
        FixtureControl::Receive(receipt) => fixture.receive(receipt)?,
        FixtureControl::Resolve {
            frame_id,
            resolution,
            resolved_ns,
        } => fixture.resolve_frame(frame_id, resolution, resolved_ns)?,
    }
    Ok(Vec::new())
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

fn runtime_directory(identity: WorkerIdentity) -> PathBuf {
    match identity {
        WorkerIdentity::User => std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir),
        WorkerIdentity::RestrictedFallback => fallback_runtime_directory(unsafe { libc::getuid() }),
    }
}

fn fallback_runtime_directory(uid: libc::uid_t) -> PathBuf {
    PathBuf::from("/run/user").join(uid.to_string())
}

pub(super) fn frame_path_for_identity(identity: WorkerIdentity) -> Result<PathBuf> {
    let runtime = runtime_directory(identity);
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

fn kill_process_group_id(process_group: libc::pid_t) {
    if process_group > 1 {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

fn send_pidfd_signal(pidfd: &std::fs::File, signal: libc::c_int) {
    unsafe {
        libc::syscall(libc::SYS_pidfd_send_signal, pidfd.as_raw_fd(), signal, 0, 0);
    }
}

fn cgroup_processes(cgroup: &Path) -> Vec<libc::pid_t> {
    std::fs::read_to_string(cgroup.join("cgroup.procs"))
        .map(|processes| {
            processes
                .split_whitespace()
                .filter_map(|process| process.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn reap_pids_until(mut pids: Vec<libc::pid_t>, deadline: Instant) {
    pids.sort_unstable();
    pids.dedup();
    while !pids.is_empty() {
        pids.retain(|pid| loop {
            let result = unsafe { libc::waitpid(*pid, std::ptr::null_mut(), libc::WNOHANG) };
            if result == *pid {
                return false;
            }
            if result < 0 && unsafe { *libc::__errno_location() } == libc::EINTR {
                continue;
            }
            if result < 0 {
                return false;
            }
            return true;
        });
        if pids.is_empty() || Instant::now() >= deadline {
            break;
        }
        thread::sleep(WRITE_RETRY);
    }
}

fn kill_systemd_unit(unit: &str, runtime: Option<&Path>) {
    let mut command = Command::new("systemctl");
    configure_systemd_user_environment(&mut command, runtime);
    let Ok(mut command) = command
        .args(["--user", "kill", "--kill-who=all", "--signal=SIGKILL", unit])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = Instant::now() + KILL_REAP_DEADLINE;
    loop {
        match command.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = command.kill();
                let _ = command.wait();
                return;
            }
            Ok(None) => thread::sleep(WRITE_RETRY),
        }
    }
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
    if closed {
        if !input.is_empty() {
            bail!("Lua worker control socket closed in the middle of a packet");
        }
        if packets.is_empty() {
            bail!("Lua worker control socket closed");
        }
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

fn encode_fixture_bootstrap(
    output: &mut Vec<u8>,
    fixture: Option<&FixtureBootstrap>,
) -> Result<()> {
    let Some(fixture) = fixture else {
        output.push(0);
        return Ok(());
    };
    fixture.plan()?;
    output.push(1); // private optional plan v1
    put_bytes(output, fixture.run.as_bytes())?;
    put_bytes(output, fixture.generation.as_bytes())?;
    output.extend_from_slice(&fixture.start_ns.to_be_bytes());
    output.extend_from_slice(&fixture.rate.to_be_bytes());
    output.push(match fixture.mode {
        crate::diagnostic_fixture::Mode::A => 0,
        crate::diagnostic_fixture::Mode::B => 1,
        crate::diagnostic_fixture::Mode::C => 2,
    });
    output.push(u8::from(fixture.causal));
    Ok(())
}

fn decode_fixture_bootstrap(reader: &mut Reader<'_>) -> Result<Option<FixtureBootstrap>> {
    match reader.u8()? {
        0 => Ok(None),
        1 => {
            let mut identity = || -> Result<String> {
                let len = reader.u32()? as usize;
                ensure!(
                    (1..=128).contains(&len),
                    "fixture identity length outside bounds"
                );
                Ok(std::str::from_utf8(reader.take(len)?)?.to_owned())
            };
            let run = identity()?;
            let generation = identity()?;
            let start_ns = reader.u64()?;
            let rate = reader.u32()?;
            let mode = match reader.u8()? {
                0 => crate::diagnostic_fixture::Mode::A,
                1 => crate::diagnostic_fixture::Mode::B,
                2 => crate::diagnostic_fixture::Mode::C,
                _ => bail!("invalid fixture mode"),
            };
            let causal = reader.bool()?;
            let mut plan = FixtureBootstrap::new(&run, &generation, start_ns, rate, mode)?;
            plan.causal = causal;
            plan.plan()?;
            Ok(Some(plan))
        }
        _ => bail!("unknown private fixture bootstrap version"),
    }
}

fn decode_bootstrap(
    payload: &[u8],
) -> Result<(
    LuaSource,
    PathBuf,
    f64,
    InputState,
    Option<FixtureBootstrap>,
)> {
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
    let fixture = decode_fixture_bootstrap(&mut reader)?;
    reader.finish()?;
    Ok((source, frame_path, backlight, input, fixture))
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

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into()?))
    }

    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
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
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use super::*;

    fn embedded(source: &str) -> LuaSource {
        LuaSource::embedded(source.as_bytes().to_vec())
    }

    #[test]
    fn process_fixture_binds_reviewed_source_and_preserves_resolution_endpoint() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap, FixtureControl};
        use crate::diagnostic_fixture::{Mode, ObservationKind, Resolution, SOURCE};
        use crate::diagnostic_observer::{clock_ns, decode, EventKind};
        let mut collector = Collector::new(64)?;
        let plan = FixtureBootstrap::new(
            "process-fixture",
            "generation-1",
            clock_ns(false)? + 30_000_000_000,
            30,
            Mode::C,
        )?;
        let worker = ProcessWorker::stage_with_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            1.0,
            InputState::default(),
            collector.take_worker_storage()?,
            plan,
        )?;
        let frame = worker.render(0.0, 0.0, InputState::default())?;
        let marker =
            decode(&frame.frame).expect("reviewed child fixture must draw complete marker pixels");
        assert_eq!(marker.run_id(), b"process-fixture");
        // Decoded C identity is supplied SOFTWARE test context, not native
        // allocation/publication authority or an identity solution for mode A.
        let resolved_ns = clock_ns(false)?;
        worker.fixture_control(FixtureControl::Resolve {
            frame_id: marker.frame_id(),
            resolution: Resolution::Discarded,
            resolved_ns,
        })?;
        worker.shutdown(StopReason::Replaced)?;
        let raw = collector.snapshot()?;
        assert!(raw.metadata_consistent && raw.report.closed);
        assert_eq!(raw.report.failed_operations, 0);
        assert_eq!(raw.report.lost, 0);
        let mut allocated = false;
        let mut resolved = false;
        for record in &raw.report.records {
            if let EventKind::FixtureObserved {
                synthetic,
                decision_ns,
                kind,
            } = record.kind
            {
                assert!(!synthetic);
                assert_eq!(
                    record.at_ns, decision_ns,
                    "child uses its actual host clock for decisions"
                );
                match kind {
                    ObservationKind::Allocated { frame_id, token: 0 } => {
                        assert_eq!(frame_id, marker.frame_id());
                        assert!(record.at_ns <= resolved_ns);
                        allocated = true;
                    }
                    ObservationKind::Resolved {
                        frame_id,
                        resolution: Resolution::Discarded,
                        resolved_ns: endpoint,
                    } => {
                        assert_eq!(frame_id, marker.frame_id());
                        assert_eq!(endpoint, resolved_ns);
                        assert!(endpoint <= record.at_ns);
                        resolved = true;
                    }
                    ObservationKind::Closed => {
                        panic!("ordinary worker shutdown must not invent fixture closure")
                    }
                    _ => {}
                }
            }
        }
        assert!(allocated && resolved);
        Ok(())
    }

    #[test]
    fn process_fixture_receipt_crosses_actual_callback_and_token_pixels() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap, FixtureControl};
        use crate::diagnostic_fixture::{Mode, ObservationKind, Receipt, Resolution, SOURCE};
        use crate::diagnostic_observer::{clock_ns, decode, EventKind};
        let mut collector = Collector::new(128)?;
        let plan = FixtureBootstrap::new(
            "process-receipt",
            "generation-1",
            clock_ns(false)? + 30_000_000_000,
            30,
            Mode::C,
        )?;
        let worker = ProcessWorker::stage_with_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            1.0,
            InputState::default(),
            collector.take_worker_storage()?,
            plan,
        )?;
        let first = worker.render(0.0, 0.0, InputState::default())?;
        worker.fixture_control(FixtureControl::Resolve {
            frame_id: decode(&first.frame).unwrap().frame_id(),
            resolution: Resolution::Discarded,
            resolved_ns: clock_ns(false)?,
        })?;
        worker.commit(0.0, InputState::default())?;
        let event = TouchEvent {
            phase: TouchPhase::Down,
            id: 42,
            time: 987654.25,
            x: 14.5,
            y: 6.0,
            modifiers: crate::hardware::ModifierState::default(),
            pressure: Some(0.75),
            width: None,
            height: Some(4.0),
        };
        let received_ns = clock_ns(false)?;
        worker.fixture_control(FixtureControl::Receive(Receipt {
            sequence: 1,
            received_ns,
            event,
        }))?;
        let effects = worker.drive_until(
            DriveRequest::without_input(1.0, InputState::default()).with_events(vec![event]),
            Instant::now() + CALLBACK_DEADLINE,
        )?;
        let response = effects
            .frame
            .context("eligible callback did not produce frame")?;
        let marker = decode(&response.frame).unwrap();
        assert_eq!(marker.input_id(), Some(b"1".as_slice()));
        let responded_ns = clock_ns(false)?;
        worker.fixture_control(FixtureControl::Resolve {
            frame_id: marker.frame_id(),
            resolution: Resolution::Presented,
            resolved_ns: responded_ns,
        })?;
        worker.shutdown(StopReason::Replaced)?;
        let raw = collector.snapshot()?;
        assert_eq!(raw.report.failed_operations, 0);
        assert_eq!(raw.report.lost, 0);
        assert!(raw
            .report
            .records
            .iter()
            .any(|r| matches!(r.kind, EventKind::TouchCallbackEntered(t) if t == event)));
        assert!(raw.report.records.iter().any(|r| matches!(r.kind, EventKind::FixtureObserved {
            synthetic: false, kind: ObservationKind::ReceiptSupplied { receipt_sequence: 1, received_ns: endpoint, contact: 42, phase: TouchPhase::Down }, ..
        } if endpoint == received_ns && endpoint <= r.at_ns)));
        assert!(raw.report.records.iter().any(|r| matches!(
            r.kind,
            EventKind::FixtureObserved {
                kind: ObservationKind::TokenMutated {
                    receipt_sequence: 1,
                    previous: 0,
                    token: 1
                },
                ..
            }
        )));
        assert!(raw.report.records.iter().any(|r| matches!(r.kind, EventKind::FixtureObserved {
            kind: ObservationKind::ResponseConfirmed { receipt_sequence: 1, frame_id, token: 1, responded_ns: endpoint }, ..
        } if frame_id == marker.frame_id() && endpoint == responded_ns && endpoint <= r.at_ns)));
        Ok(())
    }

    #[test]
    fn process_fixture_calibration_loss_rejects_startup_before_any_allocation() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap};
        use crate::diagnostic_fixture::{Mode, SOURCE};
        use crate::diagnostic_observer::{clock_ns, EventKind};
        for mode in [Mode::A, Mode::B, Mode::C] {
            // One bootstrap record plus 32 probes already exceeds this budget;
            // the aggregate must not be certified or source loading attempted.
            let mut collector = Collector::new(32)?;
            let result = ProcessWorker::stage_with_fixture(
                &LuaSource::embedded(SOURCE.to_vec()),
                1.0,
                InputState::default(),
                collector.take_worker_storage()?,
                FixtureBootstrap::new(
                    "calibration-loss",
                    "child",
                    clock_ns(false)? + 30_000_000_000,
                    30,
                    mode,
                )?,
            );
            assert!(result.is_err());
            let raw = collector.snapshot()?;
            assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
            assert!(raw.report.lost > 0);
            assert!(raw.report.failed_operations > 0);
            assert!(!raw.report.records.iter().any(|r| matches!(
                r.kind,
                EventKind::FixtureObserved { .. }
                    | EventKind::RenderAllocated { .. }
                    | EventKind::MinimalCalibration { .. }
            )));
        }
        Ok(())
    }

    #[test]
    fn process_fixture_closure_controls_reject_early_host_time() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap};
        use crate::diagnostic_fixture::{Mode, ObservationKind, SOURCE};
        use crate::diagnostic_observer::{clock_ns, EventKind};
        for (subtag, expected_error) in [
            (2, "warmup closure requires five seconds"),
            (3, "fixture closure requires E"),
        ] {
            let mut collector = Collector::new(96)?;
            let worker = ProcessWorker::stage_with_fixture(
                &LuaSource::embedded(SOURCE.to_vec()),
                1.0,
                InputState::default(),
                collector.take_worker_storage()?,
                FixtureBootstrap::new(
                    "early-close",
                    "generation-1",
                    clock_ns(false)? + 30_000_000_000,
                    30,
                    Mode::C,
                )?,
            )?;
            // Literal private v1 command, no caller closure timestamp/clock selector.
            let response = worker.request(FIXTURE_CONTROL, vec![subtag], CALLBACK_DEADLINE)?;
            let error = worker
                .terminal_response(parse_status_response("fixture closure", &response))
                .unwrap_err();
            assert!(format!("{error:#}").contains(expected_error), "{error:#}");
            let raw = collector.snapshot()?;
            assert!(raw.report.records.iter().any(|r| matches!(
                r.kind,
                EventKind::FixtureObserved {
                    kind: ObservationKind::Failed,
                    ..
                }
            )));
            assert!(!raw.report.records.iter().any(|r| matches!(
                r.kind,
                EventKind::FixtureObserved {
                    kind: ObservationKind::Closed | ObservationKind::WarmupClosed,
                    ..
                }
            )));
            assert!(
                raw.report.failed_operations >= 2,
                "fixture failure and command failure are distinct observations"
            );
        }
        Ok(())
    }

    #[test]
    fn process_fixture_abc_preserves_scene_and_minimal_span_placement() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap};
        use crate::diagnostic_fixture::{Mode, SOURCE};
        use crate::diagnostic_observer::{clock_ns, decode, EventKind};
        let mut scenes = Vec::new();
        for mode in [Mode::A, Mode::B, Mode::C] {
            let mut collector = Collector::new(64)?;
            let worker = ProcessWorker::stage_with_fixture(
                &LuaSource::embedded(SOURCE.to_vec()),
                1.0,
                InputState::default(),
                collector.take_worker_storage()?,
                FixtureBootstrap::new(
                    "abc-child",
                    "generation-1",
                    clock_ns(false)? + 30_000_000_000,
                    30,
                    mode,
                )?,
            )?;
            let frame = worker.render(0.0, 0.0, InputState::default())?;
            assert_eq!(decode(&frame.frame).is_ok(), mode != Mode::A);
            scenes.push(frame.frame.pixels().to_vec());
            worker.shutdown(StopReason::Replaced)?;
            let raw = collector.snapshot()?;
            assert_eq!(raw.report.failed_operations, 0);
            assert_eq!(raw.report.lost, 0);
            assert_eq!(
                raw.report
                    .records
                    .iter()
                    .any(|r| matches!(r.kind, EventKind::FixtureObserved { .. })),
                mode == Mode::C
            );
            let spans: Vec<_> = raw
                .report
                .records
                .iter()
                .filter_map(|r| match &r.kind {
                    EventKind::MinimalSpan(s) => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(spans.len(), 33);
            assert!(spans[..32].iter().all(|s| s.kind
                == crate::diagnostic_timing::SpanKind::Calibration
                && !s.frame_bearing
                && s.status == crate::diagnostic_timing::SpanStatus::Succeeded));
            assert_eq!(
                spans[32].kind,
                crate::diagnostic_timing::SpanKind::RenderCallback
            );
            assert_eq!(
                spans[32].status,
                crate::diagnostic_timing::SpanStatus::Succeeded
            );
            assert!(spans[32].frame_bearing);
        }
        for y in 0..60 {
            for x in 0..2008 {
                if (188..1820).contains(&x) && (4..12).contains(&y) {
                    continue;
                }
                let offset = (y * 2008 + x) * 4;
                assert_eq!(
                    &scenes[0][offset..offset + 4],
                    &scenes[1][offset..offset + 4]
                );
                assert_eq!(
                    &scenes[1][offset..offset + 4],
                    &scenes[2][offset..offset + 4]
                );
            }
        }
        // No A allocation/publication identity or successful fixture closure
        // is inferred from identical pixels or ordinary storage closure.
        Ok(())
    }

    #[test]
    fn process_fixture_rejects_unprovisioned_and_altered_source_before_execution() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap};
        use crate::diagnostic_fixture::{Mode, ObservationKind, SOURCE};
        use crate::diagnostic_observer::{clock_ns, EventKind};
        let ordinary = embedded("assert(select('#', ...) == 0); require('sliver.v1'); return { api_version=1, render=function() end }");
        let worker = ProcessWorker::stage(&ordinary, 1.0, InputState::default())?;
        worker.shutdown(StopReason::Replaced)?;
        let error = ProcessWorker::stage(
            &LuaSource::embedded(SOURCE.to_vec()),
            1.0,
            InputState::default(),
        )
        .err()
        .context("reviewed fixture unexpectedly armed itself")?;
        assert!(format!("{error:#}").contains("private P1 capability required"));
        let directory = tempfile::tempdir()?;
        let sentinel = directory.path().join("executed");
        let altered = format!(
            "local f=assert(io.open({:?}, 'w')); f:write('wrong source executed'); f:close();\n{}",
            sentinel.to_str().unwrap(),
            std::str::from_utf8(SOURCE)?
        );
        let mut collector = Collector::new(96)?;
        let error = ProcessWorker::stage_with_fixture(
            &embedded(&altered),
            1.0,
            InputState::default(),
            collector.take_worker_storage()?,
            FixtureBootstrap::new(
                "wrong-source",
                "generation-1",
                clock_ns(false)? + 30_000_000_000,
                30,
                Mode::C,
            )?,
        )
        .err()
        .context("altered fixture source was accepted")?;
        assert!(format!("{error:#}").contains("does not match reviewed bytes"));
        assert!(
            !sentinel.exists(),
            "source executed before exact byte binding"
        );
        let raw = collector.snapshot()?;
        assert!(
            raw.report.records.iter().any(|r| matches!(
                r.kind,
                EventKind::FixtureObserved {
                    kind: ObservationKind::Failed,
                    ..
                }
            )),
            "C sink must be attached before bind_source rejects the chunk"
        );
        assert!(raw.report.failed_operations >= 2);
        Ok(())
    }

    #[test]
    fn process_fixture_rejects_invalid_coordinator_operations_terminally() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap, FixtureControl};
        use crate::diagnostic_fixture::{Mode, ObservationKind, Receipt, Resolution, SOURCE};
        use crate::diagnostic_observer::{clock_ns, EventKind};
        let event = TouchEvent {
            phase: TouchPhase::Down,
            id: 1,
            time: 0.0,
            x: 0.0,
            y: 0.0,
            modifiers: crate::hardware::ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        for bad_receipt in [false, true] {
            let mut collector = Collector::new(96)?;
            let worker = ProcessWorker::stage_with_fixture(
                &LuaSource::embedded(SOURCE.to_vec()),
                1.0,
                InputState::default(),
                collector.take_worker_storage()?,
                FixtureBootstrap::new(
                    "bad-control",
                    "generation-1",
                    clock_ns(false)? + 30_000_000_000,
                    30,
                    Mode::C,
                )?,
            )?;
            let command = if bad_receipt {
                FixtureControl::Receive(Receipt {
                    sequence: 2,
                    received_ns: clock_ns(false)?,
                    event,
                })
            } else {
                FixtureControl::Resolve {
                    frame_id: 99,
                    resolution: Resolution::Presented,
                    resolved_ns: clock_ns(false)?,
                }
            };
            assert!(worker.fixture_control(command).is_err());
            assert!(
                worker.render(0.0, 0.0, InputState::default()).is_err(),
                "failed worker must not accept another callback"
            );
            let raw = collector.snapshot()?;
            assert!(raw.report.records.iter().any(|r| matches!(
                r.kind,
                EventKind::FixtureObserved {
                    kind: ObservationKind::Failed,
                    ..
                }
            )));
            assert!(raw.report.failed_operations >= 2);
        }
        Ok(())
    }

    #[test]
    fn process_fixture_rejects_malformed_controls_before_effects() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap};
        use crate::diagnostic_fixture::{Mode, SOURCE};
        use crate::diagnostic_observer::{clock_ns, EventKind};
        let mut bad_resolution = vec![0; 18];
        bad_resolution[0] = 1;
        bad_resolution[9] = 3;
        let mut bad_touch_phase = vec![0; 18];
        bad_touch_phase[17] = 4;
        let mut bad_touch_bool = vec![0; 47];
        bad_touch_bool[46] = 2;
        for payload in [
            vec![],
            vec![255],
            vec![0],
            vec![1],
            vec![2, 0],
            vec![3, 0],
            bad_resolution,
            bad_touch_phase,
            bad_touch_bool,
        ] {
            let mut collector = Collector::new(96)?;
            let worker = ProcessWorker::stage_with_fixture(
                &LuaSource::embedded(SOURCE.to_vec()),
                1.0,
                InputState::default(),
                collector.take_worker_storage()?,
                FixtureBootstrap::new(
                    "malformed",
                    "generation-1",
                    clock_ns(false)? + 30_000_000_000,
                    30,
                    Mode::C,
                )?,
            )?;
            let response = worker.request(FIXTURE_CONTROL, payload.clone(), CALLBACK_DEADLINE)?;
            assert!(
                worker
                    .terminal_response(parse_status_response("malformed control", &response))
                    .is_err(),
                "accepted {payload:?}"
            );
            let raw = collector.snapshot()?;
            assert_eq!(
                raw.report.failed_operations, 1,
                "malformed command fails before fixture operation"
            );
            assert!(!raw
                .report
                .records
                .iter()
                .any(|r| matches!(r.kind, EventKind::FixtureObserved { .. })));
        }
        let ordinary = ProcessWorker::stage(
            &embedded("require('sliver.v1'); return {api_version=1,render=function() end}"),
            1.0,
            InputState::default(),
        )?;
        let error = ordinary
            .fixture_control(FixtureControl::Finish)
            .unwrap_err();
        assert!(format!("{error:#}").contains("no private fixture capability"));
        Ok(())
    }

    #[test]
    fn process_fixture_rejects_bounded_bootstrap_packets_in_actual_child() -> Result<()> {
        use crate::diagnostic_capture_transport::{Collector, FixtureBootstrap};
        use crate::diagnostic_fixture::{Mode, SOURCE};
        use crate::diagnostic_observer::clock_ns;
        let plan = FixtureBootstrap::new("r", "g", clock_ns(false)? + 30_000_000_000, 30, Mode::C)?;
        let mut valid = Vec::new();
        encode_fixture_bootstrap(&mut valid, Some(&plan))?;
        assert_eq!(valid.len(), 25);
        let mut cases = Vec::new();
        for (offset, value) in [(0, 2), (5, b'/'), (23, 3), (24, 2)] {
            let mut packet = valid.clone();
            packet[offset] = value;
            cases.push((packet, CaptureLevel::Detailed));
        }
        for length in [0_u32, 129, u32::MAX] {
            let mut packet = valid.clone();
            packet[1..5].copy_from_slice(&length.to_be_bytes());
            cases.push((packet, CaptureLevel::Detailed));
        }
        let mut overflow = valid.clone();
        overflow[11..19].copy_from_slice(&u64::MAX.to_be_bytes());
        cases.push((overflow, CaptureLevel::Detailed));
        let mut bad_rate = valid.clone();
        bad_rate[19..23].copy_from_slice(&31_u32.to_be_bytes());
        cases.push((bad_rate, CaptureLevel::Detailed));
        let mut bad_causal = valid.clone();
        bad_causal[19..23].copy_from_slice(&60_u32.to_be_bytes());
        bad_causal[24] = 1;
        cases.push((bad_causal, CaptureLevel::Detailed));
        cases.push((valid[..24].to_vec(), CaptureLevel::Detailed));
        let mut trailing = valid.clone();
        trailing.push(0);
        cases.push((trailing, CaptureLevel::Detailed));
        cases.push((valid.clone(), CaptureLevel::Minimal)); // C must have detailed sink.
        let mut wrong_ab = valid;
        wrong_ab[23] = 0;
        cases.push((wrong_ab, CaptureLevel::Detailed));
        for (tail, level) in cases {
            let path = frame_path_for_identity(WorkerIdentity::User)?;
            let _slots = FrameSlots::new_shared(
                &path,
                crate::DISPLAY_WIDTH,
                crate::DISPLAY_HEIGHT,
                crate::DISPLAY_WIDTH * 4,
            )?;
            let mut collector = Collector::new(16)?;
            let storage = collector.take_worker_storage()?;
            let mut spawned = spawn_direct(WorkerIdentity::User)?;
            // Always reap the actual direct child, including failed assertions.
            let result = (|| -> Result<()> {
                spawned.stream.set_nonblocking(true)?;
                receive_hello(&mut spawned.stream, &mut Vec::new())?;
                crate::diagnostic_capture_transport::send_bootstrap_storage(
                    &spawned.stream,
                    Some((&storage, level)),
                    Instant::now() + CALLBACK_DEADLINE,
                )?;
                let mut payload = Vec::new();
                encode_source(&mut payload, &LuaSource::embedded(SOURCE.to_vec()))?;
                put_bytes(&mut payload, path.as_os_str().as_bytes())?;
                put_f64(&mut payload, 1.0);
                encode_input_state(&mut payload, InputState::default());
                payload.extend_from_slice(&tail);
                write_packet_blocking(&mut spawned.stream, BOOTSTRAP, &payload)?;
                let deadline = Instant::now() + CALLBACK_DEADLINE;
                loop {
                    if let Some(status) = spawned.child.try_wait()? {
                        ensure!(
                            !status.success(),
                            "malformed fixture bootstrap exited successfully"
                        );
                        break;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "malformed fixture bootstrap was accepted: {tail:?}"
                    );
                    thread::sleep(WRITE_RETRY);
                }
                let raw = collector.snapshot()?;
                ensure!(
                    raw.report.failed_operations >= 1,
                    "bootstrap rejection missing raw failure"
                );
                Ok(())
            })();
            terminate_spawned(&mut spawned);
            result?;
        }
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_crosses_real_canvas_and_fake_present() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        use crate::diagnostic_observer::{Capture, EventKind};
        use crate::hardware::{FakeTouchBar, HardwareEvent, ModifierState, TouchBarHardware};

        let directory = tempfile::tempdir()?;
        let source = directory.path().join("observed.lua");
        std::fs::write(
            &source,
            include_str!("../../../scripts/native-performance-observer-smoke.lua"),
        )?;
        std::fs::write(
            directory.path().join("native_marker.lua"),
            include_str!("../../../scripts/native-performance-marker.lua"),
        )?;
        let mut collector = Collector::new(32)?;
        let worker = ProcessWorker::stage_observed(
            &LuaSource::file(source),
            1.0,
            InputState::default(),
            collector.take_worker_storage()?,
        )?;
        let frame = worker.render(0.0, 0.0, InputState::default())?;
        let broker_capture = Capture::new(8)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        broker_capture.present(&mut hardware, &frame.frame)?;
        worker.commit(0.0, InputState::default())?;
        let touch = TouchEvent {
            phase: TouchPhase::Down,
            id: 42,
            time: 987654.0,
            x: 14.5,
            y: 6.0,
            modifiers: ModifierState::default(),
            pressure: Some(0.75),
            width: None,
            height: Some(4.0),
        };
        hardware.inject(HardwareEvent::Touch(touch));
        let events = broker_capture.poll(&mut hardware, Duration::ZERO)?;
        assert_eq!(events, [HardwareEvent::Touch(touch)]);
        let effects = worker.drive_until(
            DriveRequest::without_input(1.0, InputState::default()).with_events(vec![touch]),
            Instant::now() + CALLBACK_DEADLINE,
        )?;
        let response = effects
            .frame
            .context("touch did not produce a real frame")?;
        broker_capture.present(&mut hardware, &response.frame)?;
        worker.shutdown(StopReason::Replaced)?;

        let raw = collector.snapshot()?;
        assert!(raw.metadata_consistent);
        assert!(raw.report.closed);
        assert_eq!(raw.report.lost, 0);
        let callbacks: Vec<_> = raw
            .report
            .records
            .iter()
            .filter_map(|r| match &r.kind {
                EventKind::MinimalSpan(sample) => Some(sample),
                _ => None,
            })
            .collect();
        assert_eq!(
            callbacks.len(),
            2,
            "both actual callback samples must cross process exit"
        );
        for (index, sample) in callbacks.iter().enumerate() {
            assert_eq!(
                sample.kind,
                crate::diagnostic_timing::SpanKind::RenderCallback
            );
            assert_eq!(
                sample.status,
                crate::diagnostic_timing::SpanStatus::Succeeded
            );
            assert_eq!(sample.sequence, index as u64 + 1);
            assert!(sample.frame_bearing && sample.start_ns <= sample.end_ns);
        }
        let summary = raw
            .report
            .records
            .iter()
            .find_map(|r| match &r.kind {
                EventKind::MinimalSummary(summary) => Some(summary),
                _ => None,
            })
            .context("minimal timing summary was not exported")?;
        assert_eq!(summary.completed_spans, 2);
        assert_eq!(summary.open_spans, 0);
        assert_eq!(summary.lost, 0);
        assert!(summary.closed_at_ns.is_some());
        assert!(summary.closed_at_ns <= raw.report.closed_at_ns);
        assert!(raw
            .report
            .records
            .iter()
            .any(|r| matches!(r.kind, EventKind::RenderAllocated { attempt: 1 })));
        let marker = raw
            .report
            .records
            .iter()
            .find_map(|r| match &r.kind {
                EventKind::Published {
                    marker: Ok(marker), ..
                } => Some(marker),
                _ => None,
            })
            .context("worker publication was not exported")?;
        assert_eq!(marker.run_id(), b"software-smoke");
        assert_eq!(marker.frame_id(), 1);
        assert!(raw.report.records.iter().any(
            |r| matches!(&r.kind, EventKind::TouchCallbackEntered(observed) if *observed == touch)
        ));
        assert!(raw.report.records.iter().any(|r| matches!(&r.kind, EventKind::Published { marker: Ok(decoded), .. } if decoded.input_id() == Some(b"1".as_slice()))));
        assert_eq!(
            crate::diagnostic_observer::decode(&response.frame)
                .unwrap()
                .input_id(),
            Some(b"1".as_slice())
        );
        assert!(broker_capture
            .close()
            .records
            .iter()
            .any(|r| matches!(&r.kind,
            EventKind::PresentEntered { marker: Ok(decoded), .. } if decoded == marker)));
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_retains_unclosed_prefix_after_watchdog() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        use crate::diagnostic_observer::EventKind;
        let mut collector = Collector::new(8)?;
        let worker = ProcessWorker::stage_observed(
            &embedded("require('sliver.v1'); return { api_version = 1, render = function() while true do end end }"),
            1.0, InputState::default(), collector.take_worker_storage()?,
        )?;
        let error = worker
            .render(0.0, 0.0, InputState::default())
            .err()
            .context("hung worker unexpectedly rendered")?;
        assert!(format!("{error:#}").contains("two seconds"));
        assert!(!worker.is_alive());
        let raw = collector.snapshot()?;
        assert!(raw.initialized);
        assert!(!raw.report.closed);
        assert!(raw.report.closed_at_ns.is_none());
        assert!(raw
            .report
            .records
            .iter()
            .any(|r| matches!(r.kind, EventKind::RenderAllocated { attempt: 1 })));
        assert!(!raw
            .report
            .records
            .iter()
            .any(|r| matches!(r.kind, EventKind::RenderFinished { .. })));
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_starts_before_load_and_retains_overflow() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        use crate::diagnostic_observer::EventKind;
        let mut collector = Collector::new(2)?;
        let worker = ProcessWorker::stage_observed(
            &embedded("local s=require('sliver.v1'); s.timer.after(2.5, function() end); return { api_version=1, render=function(c) c:rectangle(0,0,8,8,'#ff0000') end }"),
            1.0, InputState::default(), collector.take_worker_storage()?,
        )?;
        worker.render(0.0, 0.0, InputState::default())?;
        worker.shutdown(StopReason::Replaced)?;
        let raw = collector.snapshot()?;
        assert!(raw.metadata_consistent && raw.report.closed);
        assert_eq!(raw.report.records.len(), 2);
        assert!(matches!(
            raw.report.records[0].kind,
            EventKind::WorkerCaptureConfigured { detailed: true }
        ));
        assert!(matches!(
            raw.report.records[1].kind,
            EventKind::TimerRegistered {
                timer_id: 1,
                delay_seconds: 2.5,
                ..
            }
        ));
        assert!(raw.report.attempted_records > 2);
        assert_eq!(raw.report.lost, raw.report.attempted_records - 2);
        assert!(
            raw.report.decode_failures > 0,
            "errors after capacity exhaustion must remain counted"
        );
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_retains_source_load_failure() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        let mut collector = Collector::new(4)?;
        let error = ProcessWorker::stage_observed(
            &embedded("error('source load failed deliberately')"),
            1.0,
            InputState::default(),
            collector.take_worker_storage()?,
        )
        .err()
        .context("broken source unexpectedly loaded")?;
        assert!(format!("{error:#}").contains("source load failed deliberately"));
        let raw = collector.snapshot()?;
        assert!(raw.initialized);
        assert_eq!(
            raw.report.failed_operations, 1,
            "READY error must not disappear from raw capture"
        );
        assert_eq!(
            raw.report
                .records
                .iter()
                .filter(|r| matches!(
                    r.kind,
                    crate::diagnostic_observer::EventKind::WorkerLoadFailed
                ))
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_retains_control_loop_failure() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        let mut collector = Collector::new(4)?;
        let worker = ProcessWorker::stage_observed(
            &embedded("require('sliver.v1'); return { api_version=1, render=function() end }"),
            1.0,
            InputState::default(),
            collector.take_worker_storage()?,
        )?;
        // Fail the actual Unix control interface; no fabricated observer event.
        lock(&worker.stream, "test worker connection")?.shutdown(std::net::Shutdown::Both)?;
        worker.wait_for_exit_until(Instant::now() + Duration::from_secs(1))?;
        let raw = collector.snapshot()?;
        assert!(raw.report.closed);
        assert_eq!(
            raw.report.failed_operations, 2,
            "loop and enclosing run errors are both retained as observations"
        );
        assert_eq!(
            raw.report
                .records
                .iter()
                .filter(|r| matches!(
                    r.kind,
                    crate::diagnostic_observer::EventKind::WorkerLoopFailed
                ))
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_retains_failed_stop_command() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        let mut collector = Collector::new(4)?;
        let worker = ProcessWorker::stage_observed(
            &embedded("require('sliver.v1'); return { api_version=1, render=function() end, stop=function() error('stop failed deliberately') end }"),
            1.0, InputState::default(), collector.take_worker_storage()?,
        )?;
        let error = worker
            .shutdown(StopReason::Replaced)
            .err()
            .context("broken stop unexpectedly succeeded")?;
        assert!(format!("{error:#}").contains("stop failed deliberately"));
        let raw = collector.snapshot()?;
        assert_eq!(
            raw.report.failed_operations, 1,
            "command failure must survive a normally returning loop"
        );
        assert_eq!(
            raw.report
                .records
                .iter()
                .filter(|r| matches!(
                    r.kind,
                    crate::diagnostic_observer::EventKind::WorkerCommandFailed
                ))
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_retains_failed_render_sample() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        use crate::diagnostic_observer::EventKind;
        let mut collector = Collector::new(16)?;
        let worker = ProcessWorker::stage_observed(
            &embedded("require('sliver.v1'); return { api_version=1, render=function() error('render failed deliberately') end }"),
            1.0, InputState::default(), collector.take_worker_storage()?,
        )?;
        assert!(worker.render(0.0, 0.0, InputState::default()).is_err());
        let raw = collector.snapshot()?;
        let sample = raw
            .report
            .records
            .iter()
            .find_map(|r| match &r.kind {
                EventKind::MinimalSpan(sample) => Some(sample),
                _ => None,
            })
            .context("failed callback sample was lost before child exit")?;
        assert_eq!(
            sample.kind,
            crate::diagnostic_timing::SpanKind::RenderCallback
        );
        assert_eq!(sample.status, crate::diagnostic_timing::SpanStatus::Failed);
        assert!(sample.frame_bearing && sample.start_ns <= sample.end_ns);
        assert!(raw
            .report
            .records
            .iter()
            .any(|r| matches!(r.kind, EventKind::RenderFinished { marker: None, .. })));
        assert!(raw
            .report
            .records
            .iter()
            .any(|r| matches!(r.kind, EventKind::WorkerCommandFailed)));
        assert!(
            raw.report.failed_operations >= 3,
            "minimal, render, and command failure observations are all retained"
        );
        Ok(())
    }

    #[test]
    fn mapped_worker_capture_retains_setup_failure_before_runtime_load() -> Result<()> {
        use crate::diagnostic_capture_transport::Collector;
        let directory = tempfile::tempdir()?;
        let source = LuaSource::file(directory.path().join("missing-directory/source.lua"));
        let mut collector = Collector::new(8)?;
        assert!(ProcessWorker::stage_observed(
            &source,
            1.0,
            InputState::default(),
            collector.take_worker_storage()?,
        )
        .is_err());
        let raw = collector.snapshot()?;
        assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
        assert_eq!(
            raw.report.failed_operations, 1,
            "setup failure must not become a zero-failure closed capture"
        );
        assert!(!raw.report.records.iter().any(|r| matches!(
            r.kind,
            crate::diagnostic_observer::EventKind::RenderAllocated { .. }
        )));
        Ok(())
    }

    #[test]
    fn worker_minimal_capture_preserves_scene_without_detailed_records() -> Result<()> {
        use crate::diagnostic_capture_transport::{CaptureLevel, Collector};
        use crate::diagnostic_observer::EventKind;
        use crate::hardware::{FakeTouchBar, HardwareEvent, ModifierState, TouchBarHardware};

        let directory = tempfile::tempdir()?;
        let source = directory.path().join("same-scene.lua");
        std::fs::write(
            &source,
            format!(
                "local s=require('sliver.v1'); s.timer.after(100, function() end)\n{}",
                include_str!("../../../scripts/native-performance-observer-smoke.lua"),
            ),
        )?;
        std::fs::write(
            directory.path().join("native_marker.lua"),
            include_str!("../../../scripts/native-performance-marker.lua"),
        )?;
        let mut frames = Vec::new();
        for level in [CaptureLevel::Detailed, CaptureLevel::Minimal] {
            let mut collector = Collector::new(128)?;
            let worker = ProcessWorker::stage_with_capture(
                &LuaSource::file(source.clone()),
                1.0,
                InputState::default(),
                collector.take_worker_storage()?,
                level,
            )?;
            let mut hardware = FakeTouchBar::new();
            hardware.claim()?;
            hardware.present(&worker.render(0.0, 0.0, InputState::default())?.frame)?;
            worker.commit(0.0, InputState::default())?;
            let down = TouchEvent {
                phase: TouchPhase::Down,
                id: 7,
                time: 999.0,
                x: 14.0,
                y: 6.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            };
            hardware.inject(HardwareEvent::Touch(down));
            assert_eq!(hardware.poll(Duration::ZERO)?, [HardwareEvent::Touch(down)]);
            let effects = worker.drive_until(
                DriveRequest::without_input(1.0, InputState::default()).with_events(vec![down]),
                Instant::now() + CALLBACK_DEADLINE,
            )?;
            assert!(effects.key_requests.is_empty() && effects.backlight.is_none());
            let response = effects.frame.context("touch callback produced no frame")?;
            assert_eq!(
                crate::diagnostic_observer::decode(&response.frame)
                    .unwrap()
                    .input_id(),
                Some(b"1".as_slice())
            );
            hardware.present(&response.frame)?;
            worker.shutdown(StopReason::Replaced)?;
            hardware.release()?;
            assert_eq!(hardware.presented_frames().len(), 2);
            frames.push((
                hardware.presented_frames().to_vec(),
                hardware.actions().to_vec(),
            ));

            let raw = collector.snapshot()?;
            assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
            assert_eq!(raw.report.lost, 0);
            assert_eq!(raw.report.failed_operations, 0);
            assert!(matches!(raw.report.records.first().map(|r| &r.kind),
                Some(EventKind::WorkerCaptureConfigured { detailed }) if *detailed == (level == CaptureLevel::Detailed)));
            assert_eq!(
                raw.report
                    .records
                    .iter()
                    .filter(|r| matches!(r.kind, EventKind::WorkerCaptureConfigured { .. }))
                    .count(),
                1
            );
            let samples: Vec<_> = raw
                .report
                .records
                .iter()
                .filter_map(|r| match &r.kind {
                    EventKind::MinimalSpan(sample) => Some(sample),
                    _ => None,
                })
                .collect();
            assert_eq!(samples.len(), 2);
            assert!(samples.iter().all(|s| s.frame_bearing
                && s.kind == crate::diagnostic_timing::SpanKind::RenderCallback
                && s.status == crate::diagnostic_timing::SpanStatus::Succeeded));
            assert!(raw.report.records.iter().any(|r| matches!(&r.kind,
                EventKind::MinimalSummary(s) if s.completed_spans == 2 && s.open_spans == 0 && s.lost == 0)));
            if level == CaptureLevel::Minimal {
                assert!(
                    raw.report.records.iter().all(|r| matches!(
                        r.kind,
                        EventKind::WorkerCaptureConfigured { .. }
                            | EventKind::MinimalSpan(_)
                            | EventKind::MinimalSummary(_)
                    )),
                    "minimal-only capture leaked detailed records"
                );
            } else {
                assert!(raw
                    .report
                    .records
                    .iter()
                    .any(|r| matches!(r.kind, EventKind::TimerRegistered { .. })));
                assert!(raw
                    .report
                    .records
                    .iter()
                    .any(|r| matches!(r.kind, EventKind::RenderAllocated { .. })));
                assert!(raw
                    .report
                    .records
                    .iter()
                    .any(|r| matches!(r.kind, EventKind::Published { .. })));
                assert!(raw
                    .report
                    .records
                    .iter()
                    .any(|r| matches!(r.kind, EventKind::TouchCallbackEntered(_))));
            }
        }
        assert_eq!(
            frames[0], frames[1],
            "capture level changed pixels or hardware actions"
        );
        Ok(())
    }

    #[test]
    fn worker_minimal_capture_retains_load_and_callback_errors() -> Result<()> {
        use crate::diagnostic_capture_transport::{CaptureLevel, Collector};
        use crate::diagnostic_observer::EventKind;
        for (source, load_failure) in [
            ("error('minimal load failure')", true),
            ("require('sliver.v1'); return { api_version=1, render=function() error('minimal render failure') end }", false),
        ] {
            let mut collector = Collector::new(16)?;
            let staged = ProcessWorker::stage_with_capture(
                &embedded(source), 1.0, InputState::default(),
                collector.take_worker_storage()?, CaptureLevel::Minimal,
            );
            if load_failure {
                assert!(staged.is_err());
            } else {
                assert!(staged?.render(0.0, 0.0, InputState::default()).is_err());
            }
            let raw = collector.snapshot()?;
            assert!(raw.initialized);
            assert!(raw.report.failed_operations > 0);
            assert_eq!(raw.report.lost, 0);
            assert!(matches!(raw.report.records.first().map(|r| &r.kind),
                Some(EventKind::WorkerCaptureConfigured { detailed: false })));
            assert!(raw.report.records.iter().all(|r| matches!(r.kind,
                EventKind::WorkerCaptureConfigured { .. } | EventKind::MinimalSpan(_) |
                EventKind::MinimalSummary(_) | EventKind::WorkerLoadFailed |
                EventKind::WorkerCommandFailed | EventKind::WorkerLoopFailed | EventKind::WorkerRunFailed)),
                "error path leaked detailed events in Minimal");
            if load_failure {
                assert!(raw.report.records.iter().any(|r| matches!(r.kind, EventKind::WorkerLoadFailed)));
            } else {
                assert!(raw.report.records.iter().any(|r| matches!(r.kind, EventKind::WorkerCommandFailed)));
                assert!(raw.report.records.iter().any(|r| matches!(&r.kind,
                    EventKind::MinimalSpan(s) if s.status == crate::diagnostic_timing::SpanStatus::Failed)));
            }
        }
        Ok(())
    }

    fn spawn_direct_after_discovery_delay(identity: WorkerIdentity) -> Result<SpawnedWorker> {
        let spawned = spawn_direct(identity)?;
        // Model spawn_systemd discovering the cgroup after the child has sent
        // HELLO, before the parent begins receive_hello/request_bootstrap.
        thread::sleep(CALLBACK_DEADLINE + Duration::from_millis(500));
        Ok(spawned)
    }

    #[test]
    fn off_worker_bootstrap_waits_for_parent_after_slow_launcher_discovery() -> Result<()> {
        let path = frame_path_for_identity(WorkerIdentity::User)?;
        let slots = FrameSlots::new_shared(
            &path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let worker = ProcessWorker::stage_with_spawned(
            &embedded("require('sliver.v1'); return { api_version=1, render=function(c) c:rectangle(0,0,2,1,'#ff0000') end }"),
            1.0, InputState::default(), &path, slots.broker(), WorkerIdentity::User,
            spawn_direct_after_discovery_delay,
        )?;
        let frame = worker.render(0.0, 0.0, InputState::default())?;
        // Logical canvas snapshots retain Cairo's native BGRA byte order.
        assert_eq!(&frame.frame.pixels()[..4], &[0, 0, 255, 255]);
        worker.shutdown(StopReason::Replaced)?;
        Ok(())
    }

    fn spawn_waiting_launcher(_identity: WorkerIdentity) -> Result<SpawnedWorker> {
        let (parent_fd, child_fd) = socket_pair()?;
        let worker_path = worker_path()?;
        let mut launcher = Command::new("sh");
        launcher
            .arg("-c")
            .arg(r#""$1"; exec sleep 30"#)
            .arg("sliver-test-launcher")
            .arg(worker_path)
            .stdin(unsafe { Stdio::from(std::fs::File::from_raw_fd(child_fd)) })
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("SLIVER_LUA_WORKER_FD", WORKER_FD.to_string());
        unsafe {
            use std::os::unix::process::CommandExt;
            launcher.pre_exec(|| {
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = match launcher.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe {
                    libc::close(parent_fd);
                }
                return Err(error).context("starting the test worker launcher");
            }
        };
        let process_group = child.id() as libc::pid_t;
        let stream = unsafe { UnixStream::from_raw_fd(parent_fd) };
        Ok(SpawnedWorker {
            child,
            stream,
            process_group,
            unit: None,
            cgroup: None,
            systemd_runtime: None,
        })
    }

    fn spawn_systemd_launcher_that_never_connects(
        _identity: WorkerIdentity,
    ) -> Result<SpawnedWorker> {
        let (parent_fd, child_fd) = socket_pair()?;
        let marker = std::env::var_os("SLIVER_TEST_SYSTEMD_DESCENDANT_MARKER")
            .context("test descendant marker is not configured")?;
        let launcher_marker = std::env::var_os("SLIVER_TEST_SYSTEMD_LAUNCHER_MARKER")
            .context("test launcher marker is not configured")?;
        let unit = format!("sliver-test-no-connect-{}.service", std::process::id());
        let helper = PathBuf::from(&marker).with_extension("sh");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nsleep 30 &\nprintf '%s %s\\n' \"$$\" \"$!\" > {}\nwait\n",
                marker.display(),
            ),
        )?;
        let mut permissions = std::fs::metadata(&helper)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)?;
        let mut launcher = Command::new("systemd-run");
        launcher.args([
            "--user",
            "--unit",
            &unit,
            "--collect",
            "--wait",
            "--quiet",
            "--service-type=exec",
            "--property",
            "KillMode=control-group",
            "--property",
            "Restart=no",
        ]);
        launcher.arg(&helper);
        launcher.stdin(unsafe { Stdio::from(std::fs::File::from_raw_fd(child_fd)) });
        launcher.stdout(Stdio::null()).stderr(Stdio::null());
        let child = match launcher.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe {
                    libc::close(parent_fd);
                }
                return Err(error).context("starting the systemd no-connect test launcher");
            }
        };
        std::fs::write(&launcher_marker, child.id().to_string())?;
        let cgroup = systemd_unit_cgroup(&unit, None)?;
        let stream = unsafe { UnixStream::from_raw_fd(parent_fd) };
        Ok(SpawnedWorker {
            child,
            stream,
            process_group: 0,
            unit: Some(unit),
            cgroup: Some(cgroup),
            systemd_runtime: None,
        })
    }

    fn spawn_launcher_that_never_connects(_identity: WorkerIdentity) -> Result<SpawnedWorker> {
        let (parent_fd, child_fd) = socket_pair()?;
        let marker = std::env::var_os("SLIVER_TEST_DIRECT_DESCENDANT_MARKER")
            .context("test descendant marker is not configured")?;
        let mut launcher = Command::new("sh");
        launcher
            .arg("-c")
            .arg(r#"sh -c 'echo "$PPID $$" > "$1"; exec sleep 30' child "$1" & exec sleep 30"#)
            .arg("sliver-test-launcher")
            .arg(marker)
            .stdin(unsafe { Stdio::from(std::fs::File::from_raw_fd(child_fd)) })
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            use std::os::unix::process::CommandExt;
            launcher.pre_exec(|| {
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = match launcher.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe {
                    libc::close(parent_fd);
                }
                return Err(error).context("starting the non-connecting test launcher");
            }
        };
        let process_group = child.id() as libc::pid_t;
        let stream = unsafe { UnixStream::from_raw_fd(parent_fd) };
        Ok(SpawnedWorker {
            child,
            stream,
            process_group,
            unit: None,
            cgroup: None,
            systemd_runtime: None,
        })
    }

    fn cgroup_is_empty(cgroup: &Path) -> bool {
        std::fs::read_to_string(cgroup.join("cgroup.procs"))
            .map(|processes| processes.trim().is_empty())
            .unwrap_or(true)
    }

    fn wait_for_path_to_disappear(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        !path.exists()
    }

    fn launcher_has_been_reaped(pid: u32) -> bool {
        let path = format!("/proc/{pid}");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !std::path::Path::new(&path).exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn cleanup_test_launcher(pid: u32) {
        let mut status = 0;
        unsafe {
            if libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) == 0 {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
                while libc::waitpid(pid as libc::pid_t, &mut status, 0) < 0
                    && *libc::__errno_location() == libc::EINTR
                {}
            }
        }
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
    fn complete_worker_packet_precedes_eof_error() -> Result<()> {
        let (mut sender, mut receiver) = UnixStream::pair()?;
        write_packet_blocking(&mut sender, HELLO, &[1, 2, 3, 4])?;
        sender.shutdown(std::net::Shutdown::Write)?;

        let packets = read_packets(&mut receiver, &mut Vec::new())?;
        assert_eq!(packets, vec![(HELLO, vec![1, 2, 3, 4])]);
        let error = read_packets(&mut receiver, &mut Vec::new())
            .expect_err("EOF was reported before the buffered packet was consumed");
        assert!(error.to_string().contains("socket closed"));
        Ok(())
    }

    #[test]
    fn worker_exits_when_supervisor_disconnects_before_bootstrap() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket_path = directory.path().join("worker.sock");
        let listener = UnixListener::bind(&socket_path)?;
        let mut child = Command::new(worker_path()?)
            .arg("--connect")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let (mut peer, _) = listener.accept()?;
        let mut header = [0u8; 4];
        peer.read_exact(&mut header)?;
        let length = u32::from_be_bytes(header) as usize;
        let mut hello = vec![0u8; length];
        peer.read_exact(&mut hello)?;
        assert_eq!(hello.first(), Some(&HELLO));
        drop(peer);
        drop(listener);

        let deadline = Instant::now() + Duration::from_secs(1);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Lua worker stayed alive after its supervisor disconnected");
            }
            thread::sleep(WRITE_RETRY);
        };
        assert!(!status.success());
        Ok(())
    }

    #[test]
    fn fallback_systemd_helpers_use_the_broker_user_manager_environment() {
        let runtime = Path::new("/run/user/976");
        let mut command = Command::new("systemctl");
        configure_systemd_user_environment(&mut command, Some(runtime));

        let environment = |name: &str| {
            command
                .get_envs()
                .find(|(key, _)| *key == std::ffi::OsStr::new(name))
                .and_then(|(_, value)| value)
        };
        assert_eq!(
            environment("XDG_RUNTIME_DIR"),
            Some(std::ffi::OsStr::new("/run/user/976"))
        );
        assert_eq!(
            environment("DBUS_SESSION_BUS_ADDRESS"),
            Some(std::ffi::OsStr::new("unix:path=/run/user/976/bus"))
        );
    }

    #[test]
    fn fallback_systemd_stage_works_without_inherited_user_bus_environment() -> Result<()> {
        const CHILD: &str = "SLIVER_TEST_FALLBACK_ENV_CHILD";
        if std::env::var_os(CHILD).is_some() {
            ensure!(
                std::env::var_os("XDG_RUNTIME_DIR").is_none(),
                "fallback child unexpectedly inherited XDG_RUNTIME_DIR"
            );
            ensure!(
                std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none(),
                "fallback child unexpectedly inherited DBUS_SESSION_BUS_ADDRESS"
            );
            let directory = tempfile::tempdir()?;
            let frame_path = directory.path().join("frame.bin");
            let slots = FrameSlots::new_shared(
                &frame_path,
                crate::DISPLAY_WIDTH,
                crate::DISPLAY_HEIGHT,
                crate::DISPLAY_WIDTH * 4,
            )?;
            let worker = ProcessWorker::stage_with_frames_systemd(
                &embedded(
                    "require('sliver.v1'); return { api_version = 1, render = function() end }",
                ),
                0.0,
                InputState::default(),
                &frame_path,
                slots.broker(),
                WorkerIdentity::RestrictedFallback,
            )?;
            let frame = worker.render(1.0, 0.0, InputState::default())?;
            ensure!(
                (frame.frame.width(), frame.frame.height())
                    == (crate::DISPLAY_WIDTH, crate::DISPLAY_HEIGHT),
                "fallback worker returned an invalid frame"
            );
            worker.shutdown(StopReason::Shutdown)
        } else {
            let _systemd_tests = crate::lock_systemd_tests();
            let available = std::process::Command::new("systemd-run")
                .args(["--user", "--wait", "--quiet", "true"])
                .status();
            anyhow::ensure!(
                available.is_ok_and(|status| status.success()),
                "systemd user manager is required for this worker test"
            );
            let status = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "lua_worker::worker_process::tests::fallback_systemd_stage_works_without_inherited_user_bus_environment",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove("XDG_RUNTIME_DIR")
                .env_remove("DBUS_SESSION_BUS_ADDRESS")
                .status()?;
            anyhow::ensure!(
                status.success(),
                "fallback worker failed without inherited user bus environment: {status}"
            );
            Ok(())
        }
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
    fn restricted_fallback_runtime_is_derived_from_the_broker_uid() {
        assert_eq!(
            fallback_runtime_directory(976),
            PathBuf::from("/run/user/976")
        );
        assert_eq!(
            runtime_directory(WorkerIdentity::RestrictedFallback),
            fallback_runtime_directory(unsafe { libc::getuid() })
        );
    }

    #[test]
    fn systemd_worker_uses_the_declared_resource_and_device_policy() -> Result<()> {
        let _systemd_tests = crate::lock_systemd_tests();
        let available = std::process::Command::new("systemd-run")
            .args(["--user", "--wait", "--quiet", "true"])
            .status();
        anyhow::ensure!(
            available.is_ok_and(|status| status.success()),
            "systemd user manager is required for this worker policy test"
        );

        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("fallback-runtime");
        let source = embedded(&format!(
            r#"
            local marker = assert(io.open({marker:?}, "w"))
            marker:write(assert(os.getenv("XDG_RUNTIME_DIR")))
            marker:close()
            require("sliver.v1")
            return {{ api_version = 1, render = function() end }}
            "#,
            marker = marker.to_string_lossy(),
        ));
        let frame_path = frame_path_for_identity(WorkerIdentity::RestrictedFallback)?;
        let slots = FrameSlots::new_shared(
            &frame_path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let worker = ProcessWorker::stage_with_frames_systemd(
            &source,
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
        assert_eq!(
            std::fs::read_to_string(&marker)?,
            fallback_runtime_directory(unsafe { libc::getuid() })
                .display()
                .to_string()
        );
        let status = std::fs::read_to_string(format!("/proc/{}/status", worker.pid))?;
        let effective_uid = status
            .lines()
            .find(|line| line.starts_with("Uid:"))
            .and_then(|line| line.split_whitespace().nth(2))
            .context("worker status did not contain an effective UID")?
            .parse::<libc::uid_t>()?;
        assert_eq!(effective_uid, unsafe { libc::getuid() });
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
    fn a_systemd_staging_error_kills_descendants_and_reaps_the_launcher() -> Result<()> {
        let _systemd_tests = crate::lock_systemd_tests();
        let directory = tempfile::tempdir()?;
        let descendant_marker = directory.path().join("descendant-pids");
        let launcher_marker = directory.path().join("launcher-pid");
        unsafe {
            std::env::set_var("SLIVER_TEST_SYSTEMD_DESCENDANT_MARKER", &descendant_marker);
            std::env::set_var("SLIVER_TEST_SYSTEMD_LAUNCHER_MARKER", &launcher_marker);
        }
        let frame_path = directory.path().join("frame");
        let slots = FrameSlots::new_shared(
            &frame_path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let result = ProcessWorker::stage_with_spawned(
            &embedded("return { api_version = 1, render = function() end }"),
            0.0,
            InputState::default(),
            &frame_path,
            slots.broker(),
            WorkerIdentity::User,
            spawn_systemd_launcher_that_never_connects,
        );
        unsafe {
            std::env::remove_var("SLIVER_TEST_SYSTEMD_DESCENDANT_MARKER");
            std::env::remove_var("SLIVER_TEST_SYSTEMD_LAUNCHER_MARKER");
        }
        let error = match result {
            Ok(_) => bail!("a systemd launcher that never connects was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("identify itself"));
        let descendant_pids = std::fs::read_to_string(&descendant_marker)?;
        for pid in descendant_pids
            .split_whitespace()
            .map(|value| {
                value
                    .parse::<u32>()
                    .with_context(|| format!("invalid descendant pid {value:?}"))
            })
            .collect::<Result<Vec<_>>>()?
        {
            assert!(wait_for_path_to_disappear(
                Path::new(&format!("/proc/{pid}")),
                KILL_REAP_DEADLINE,
            ));
        }
        let launcher_pid_text = std::fs::read_to_string(&launcher_marker)?;
        let launcher_pid = launcher_pid_text
            .trim()
            .parse::<u32>()
            .with_context(|| format!("invalid launcher pid {launcher_pid_text:?}"))?;
        assert!(launcher_has_been_reaped(launcher_pid));
        Ok(())
    }

    #[test]
    fn a_staging_error_kills_descendants_and_reaps_the_launcher() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("descendant-pid");
        unsafe {
            std::env::set_var("SLIVER_TEST_DIRECT_DESCENDANT_MARKER", &marker);
        }
        let result = ProcessWorker::stage_with_spawned(
            &embedded("return { api_version = 1, render = function() end }"),
            0.0,
            InputState::default(),
            &directory.path().join("frame"),
            FrameSlots::new_shared(
                &directory.path().join("frame"),
                crate::DISPLAY_WIDTH,
                crate::DISPLAY_HEIGHT,
                crate::DISPLAY_WIDTH * 4,
            )?
            .broker(),
            WorkerIdentity::User,
            spawn_launcher_that_never_connects,
        );
        unsafe {
            std::env::remove_var("SLIVER_TEST_DIRECT_DESCENDANT_MARKER");
        }
        let error = match result {
            Ok(_) => bail!("a launcher that never connects was accepted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("identify itself"));
        let pids: Vec<_> = std::fs::read_to_string(&marker)?
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<std::result::Result<_, _>>()?;
        assert_eq!(pids.len(), 2);
        for pid in pids {
            assert!(wait_for_path_to_disappear(
                Path::new(&format!("/proc/{pid}")),
                Duration::from_millis(100)
            ));
        }
        Ok(())
    }

    #[test]
    fn a_hung_render_reaps_the_launcher_that_waits_for_the_worker() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let frame_path = directory.path().join("frame");
        let slots = FrameSlots::new_shared(
            &frame_path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let worker = ProcessWorker::stage_with_spawned(
            &embedded(
                r#"
                require("sliver.v1")
                return {
                    api_version = 1,
                    render = function()
                        while true do end
                    end,
                }
                "#,
            ),
            0.0,
            InputState::default(),
            &frame_path,
            slots.broker(),
            WorkerIdentity::User,
            spawn_waiting_launcher,
        )?;
        let launcher_pid = lock(&worker.child, "test worker launcher")?.id();
        let error = match worker.render(1.0, 0.0, InputState::default()) {
            Ok(_) => bail!("hung Lua callback returned"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("two seconds"));
        let reaped = launcher_has_been_reaped(launcher_pid);
        drop(worker);
        cleanup_test_launcher(launcher_pid);
        assert!(reaped, "worker launcher {launcher_pid} was not reaped");
        Ok(())
    }

    #[test]
    fn a_systemd_hung_render_kills_descendants_and_releases_the_worker_cgroup() -> Result<()> {
        let _systemd_tests = crate::lock_systemd_tests();
        let available = Command::new("systemd-run")
            .args(["--user", "--wait", "--quiet", "true"])
            .status();
        ensure!(
            available.is_ok_and(|status| status.success()),
            "systemd user manager is required for this worker lifecycle test"
        );

        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("descendant-pid");
        let helper = directory.path().join("descendant.sh");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\necho \"$$\" > {}\ncat /proc/$$/cgroup >> {}\nexec sleep 30\n",
                marker.display(),
                marker.display(),
            ),
        )?;
        let mut permissions = std::fs::metadata(&helper)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)?;
        let source = format!(
            r#"
            local sliver = require("sliver.v1")
            assert(os.execute({helper:?} .. " >/dev/null 2>&1 &"))
            return {{
                api_version = 1,
                render = function()
                    while true do end
                end,
            }}
        "#,
            helper = helper.to_string_lossy(),
        );
        let frame_path = directory.path().join("frame");
        let slots = FrameSlots::new_shared(
            &frame_path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let worker = ProcessWorker::stage_with_frames_systemd(
            &embedded(&source),
            0.0,
            InputState::default(),
            &frame_path,
            slots.broker(),
            WorkerIdentity::User,
        )?;
        let unit = worker.unit.as_ref().context("systemd worker had no unit")?;
        let cgroup = systemd_unit_cgroup(unit, None)?;
        let launcher_pid = lock(&worker.child, "systemd worker launcher")?.id();
        let error = match worker.render(1.0, 0.0, InputState::default()) {
            Ok(_) => bail!("hung Lua callback returned"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("two seconds"));
        let marker_contents = std::fs::read_to_string(&marker)?;
        let mut marker_lines = marker_contents.lines();
        let descendant = marker_lines
            .next()
            .context("worker did not record its descendant")?
            .parse::<u32>()?;
        let descendant_cgroup = marker_lines
            .next()
            .context("descendant cgroup was not recorded")?;
        let expected_cgroup = format!(
            "0::/{}",
            cgroup
                .strip_prefix("/sys/fs/cgroup")
                .context("worker cgroup is outside cgroup v2")?
                .display()
        );
        assert_eq!(descendant_cgroup, expected_cgroup);
        assert_ne!(descendant, 0, "worker did not record its descendant");
        assert!(wait_for_path_to_disappear(
            Path::new(&format!("/proc/{descendant}")),
            KILL_REAP_DEADLINE,
        ));
        assert!(launcher_has_been_reaped(launcher_pid));
        assert!(cgroup_is_empty(&cgroup));
        drop(worker);
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
