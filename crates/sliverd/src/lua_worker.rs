use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, ensure, Context, Result};
use mlua::{
    Function, HookTriggers, Lua, MultiValue, Table, UserData, UserDataMethods, Value, VmState,
};

use crate::frame_canvas::FrameCanvas;
#[cfg(test)]
use crate::frame_slots::FrameWriter;
use crate::frame_slots::{FrameBroker, FrameProducer, FrameSlots, FrameTiming};
use crate::hardware::{
    output_key_metadata, InputState, InputTransition, LogicalFrame, Modifier, ObservedKey,
    OutputKey, TouchEvent, TouchPhase,
};
use crate::lua_canvas::{create_path, Canvas};
use crate::lua_image::create_image;

#[path = "worker_process.rs"]
mod worker_process;

pub(crate) fn worker_process_main() -> Result<()> {
    worker_process::worker_main()
}

pub(crate) struct TimedFrame {
    pub(crate) frame: LogicalFrame,
    pub(crate) timing: FrameTiming,
}

pub(crate) struct StagedLuaWorker {
    pub(crate) worker: LuaWorker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkerIdentity {
    User,
    RestrictedFallback,
}

struct SourceMetadata {
    path: String,
    directory: String,
}

impl SourceMetadata {
    fn from_path(path: &Path) -> Self {
        Self {
            path: path.to_string_lossy().into_owned(),
            directory: path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_string_lossy()
                .into_owned(),
        }
    }
}

struct LuaSourceDescription<'a> {
    content: SourceContent<'a>,
    path: Option<&'a Path>,
    metadata: Option<SourceMetadata>,
    label: String,
    chunk_name: String,
}

enum SourceContent<'a> {
    File(&'a Path),
    Embedded(&'a [u8]),
}

impl LuaSourceDescription<'_> {
    fn read_bytes(&self) -> std::io::Result<Vec<u8>> {
        match &self.content {
            SourceContent::File(path) => std::fs::read(path),
            SourceContent::Embedded(bytes) => Ok(bytes.to_vec()),
        }
    }
}

#[derive(Clone)]
pub(crate) enum LuaSource {
    File(PathBuf),
    Embedded(Arc<[u8]>),
}

impl LuaSource {
    pub(crate) fn file(path: PathBuf) -> Self {
        Self::File(path)
    }

    pub(crate) fn embedded(bytes: Vec<u8>) -> Self {
        Self::Embedded(Arc::<[u8]>::from(bytes))
    }

    fn describe(&self) -> LuaSourceDescription<'_> {
        match self {
            Self::File(path) => {
                let label = path.display().to_string();
                LuaSourceDescription {
                    content: SourceContent::File(path),
                    path: Some(path),
                    metadata: Some(SourceMetadata::from_path(path)),
                    chunk_name: format!("@{label}"),
                    label,
                }
            }
            Self::Embedded(bytes) => LuaSourceDescription {
                content: SourceContent::Embedded(bytes),
                path: None,
                metadata: None,
                label: "<embedded default>".to_owned(),
                chunk_name: "=sliver".to_owned(),
            },
        }
    }
}

pub(crate) struct WorkerEffects {
    pub(crate) frame: Option<TimedFrame>,
    pub(crate) backlight: Option<f64>,
    pub(crate) key_requests: Vec<KeyRequest>,
    pub(crate) next_worker_deadline: Option<f64>,
    pub(crate) redraw_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyOperation {
    Down,
    Up,
    Tap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModifierMode {
    Inherit,
    None,
    Explicit(Vec<OutputKey>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeyRequest {
    pub(crate) operation: KeyOperation,
    pub(crate) key: OutputKey,
    pub(crate) modifiers: ModifierMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VisibilityReason {
    Recovery,
    Suspend,
    Device,
}

impl VisibilityReason {
    fn as_lua_str(self) -> &'static str {
        match self {
            Self::Recovery => "recovery",
            Self::Suspend => "suspend",
            Self::Device => "device",
        }
    }
}

pub(crate) fn earliest_deadline(first: Option<f64>, second: Option<f64>) -> Option<f64> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

#[derive(Default)]
struct DriveOptions {
    visibility: Option<(bool, VisibilityReason)>,
    force_render: bool,
    resume_timers: bool,
}

pub(crate) struct DriveRequest {
    now_seconds: f64,
    input_state: InputState,
    transitions: Vec<InputTransition>,
    delta: f64,
    events: Vec<TouchEvent>,
    options: DriveOptions,
}

impl DriveRequest {
    pub(crate) fn new(
        now_seconds: f64,
        input_state: InputState,
        transitions: Vec<InputTransition>,
        delta: f64,
        events: Vec<TouchEvent>,
    ) -> Self {
        Self {
            now_seconds,
            input_state,
            transitions,
            delta,
            events,
            options: DriveOptions::default(),
        }
    }

    pub(crate) fn without_input(now_seconds: f64, input_state: InputState) -> Self {
        Self::new(now_seconds, input_state, Vec::new(), 0.0, Vec::new())
    }

    pub(crate) fn with_visibility(
        mut self,
        visible: bool,
        reason: VisibilityReason,
        force_render: bool,
    ) -> Self {
        self.options.visibility = Some((visible, reason));
        self.options.force_render = force_render;
        self
    }

    pub(crate) fn with_events(mut self, events: Vec<TouchEvent>) -> Self {
        self.events = events;
        self
    }

    pub(crate) fn with_timer_resume(mut self) -> Self {
        self.options.resume_timers = true;
        self
    }
}

struct RuntimeEffects {
    frame: Option<FrameTiming>,
    backlight: Option<f64>,
    key_requests: Vec<KeyRequest>,
    next_worker_deadline: Option<f64>,
    redraw_pending: bool,
}

struct PendingFrame {
    frame: LogicalFrame,
    timing: FrameTiming,
}

const PENDING_FRAME_RETRY_SECONDS: f64 = 0.005;
const MAX_KEY_REQUESTS_PER_DRIVE: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    Replaced,
    #[allow(dead_code)]
    Logout,
    Shutdown,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Logout => "logout",
            Self::Shutdown => "shutdown",
        }
    }
}

pub(crate) struct LuaWorker {
    commands: Option<mpsc::Sender<WorkerCommand>>,
    owner: Option<thread::JoinHandle<()>>,
    process: Option<worker_process::ProcessWorker>,
    broker: FrameBroker,
    #[cfg(test)]
    producer: FrameProducer,
}

#[allow(dead_code)]
enum WorkerCommand {
    #[allow(dead_code)]
    Render(
        f64,
        f64,
        InputState,
        mpsc::SyncSender<std::result::Result<(), String>>,
    ),
    Commit(
        f64,
        InputState,
        mpsc::SyncSender<std::result::Result<(), String>>,
    ),
    RetryPending(f64, mpsc::SyncSender<std::result::Result<bool, String>>),
    PendingBacklight(mpsc::SyncSender<std::result::Result<Option<f64>, String>>),
    Drive(
        DriveRequest,
        mpsc::SyncSender<std::result::Result<RuntimeEffects, String>>,
    ),
    RestoreBacklight(f64, mpsc::SyncSender<std::result::Result<(), String>>),
    Shutdown(
        StopReason,
        mpsc::SyncSender<std::result::Result<(), String>>,
    ),
    Abandon,
}

struct Runtime {
    _lua: Lua,
    render: Function,
    stop: Option<Function>,
    visibility: Option<Function>,
    touch: Option<Function>,
    key: Option<Function>,
    source: LuaSource,
    controls: RuntimeControls,
    visible: bool,
    timers_paused: bool,
    paused_since: Option<f64>,
    producer: FrameProducer,
    pending_frame: Option<PendingFrame>,
    pending_retry_deadline: Option<f64>,
}

struct CallbackRefs {
    start: Option<Function>,
    stop: Option<Function>,
    visibility: Option<Function>,
    touch: Option<Function>,
    key: Option<Function>,
    render: Function,
}

#[derive(Clone)]
struct RuntimeControls {
    redraw_pending: Rc<Cell<bool>>,
    timers: Rc<RefCell<TimerRegistry>>,
    committed: Rc<Cell<bool>>,
    now_seconds: Rc<Cell<Option<f64>>>,
    backlight_level: Rc<Cell<f64>>,
    pending_backlight: Rc<Cell<Option<f64>>>,
    input_state: Rc<Cell<InputState>>,
    key_requests: Rc<RefCell<Vec<KeyRequest>>>,
}

struct TimerRegistry {
    next_id: u64,
    entries: Vec<TimerEntry>,
}

struct TimerEntry {
    id: u64,
    delay: f64,
    interval: Option<f64>,
    next_deadline: Option<f64>,
    callback: Function,
}

struct TimerHandle {
    id: u64,
    timers: Rc<RefCell<TimerRegistry>>,
}

#[derive(Clone, Copy)]
struct LuaKey(OutputKey);

struct DueTimer {
    id: u64,
    scheduled_deadline: f64,
    interval: Option<f64>,
    callback: Function,
}

impl LuaWorker {
    #[cfg(test)]
    pub(crate) fn stage(source: &Path) -> Result<StagedLuaWorker> {
        Self::stage_with_backlight(source, 0.0)
    }

    #[cfg(test)]
    pub(crate) fn stage_with_backlight(
        source: &Path,
        initial_backlight: f64,
    ) -> Result<StagedLuaWorker> {
        Self::stage_with_backlight_and_input(source, initial_backlight, InputState::default())
    }

    #[cfg(test)]
    pub(crate) fn stage_with_backlight_and_input(
        source: &Path,
        initial_backlight: f64,
        initial_input: InputState,
    ) -> Result<StagedLuaWorker> {
        Self::stage_source_with_backlight_and_input(
            LuaSource::file(source.to_path_buf()),
            initial_backlight,
            initial_input,
        )
    }

    #[cfg(test)]
    pub(crate) fn stage_source_with_backlight_and_input(
        source: LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
    ) -> Result<StagedLuaWorker> {
        Self::stage_source_with_identity(
            source,
            initial_backlight,
            initial_input,
            WorkerIdentity::User,
        )
    }

    #[allow(clippy::needless_return)]
    #[cfg(test)]
    pub(crate) fn stage_source_with_identity_process(
        source: LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        identity: WorkerIdentity,
    ) -> Result<StagedLuaWorker> {
        Self::stage_source_with_process_backend(
            source,
            initial_backlight,
            initial_input,
            identity,
            worker_process::ProcessWorker::stage_with_frames,
        )
    }

    #[allow(clippy::needless_return)]
    #[cfg(test)]
    pub(crate) fn stage_source_with_identity_systemd(
        source: LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        identity: WorkerIdentity,
    ) -> Result<StagedLuaWorker> {
        Self::stage_source_with_process_backend(
            source,
            initial_backlight,
            initial_input,
            identity,
            worker_process::ProcessWorker::stage_with_frames_systemd,
        )
    }

    #[cfg(test)]
    fn stage_source_with_process_backend(
        source: LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        identity: WorkerIdentity,
        stage: fn(
            &LuaSource,
            f64,
            InputState,
            &std::path::Path,
            FrameBroker,
            WorkerIdentity,
        ) -> Result<worker_process::ProcessWorker>,
    ) -> Result<StagedLuaWorker> {
        validate_backlight_level(initial_backlight)?;
        let frame_path = worker_process::frame_path_for_identity(identity)?;
        let slots = FrameSlots::new_shared(
            &frame_path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        let producer = slots.producer();
        let broker = slots.broker();
        let process = stage(
            &source,
            initial_backlight,
            initial_input,
            &frame_path,
            broker.clone(),
            identity,
        )?;
        Ok(StagedLuaWorker {
            worker: Self {
                commands: None,
                owner: None,
                process: Some(process),
                broker,
                producer,
            },
        })
    }

    #[allow(clippy::needless_return)]
    pub(crate) fn stage_source_with_identity(
        source: LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        identity: WorkerIdentity,
    ) -> Result<StagedLuaWorker> {
        #[cfg(test)]
        let _ = identity;
        validate_backlight_level(initial_backlight)?;
        #[cfg(test)]
        let slots = FrameSlots::new(
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        #[cfg(not(test))]
        let frame_path = worker_process::frame_path_for_identity(identity)?;
        #[cfg(not(test))]
        let slots = FrameSlots::new_shared(
            &frame_path,
            crate::DISPLAY_WIDTH,
            crate::DISPLAY_HEIGHT,
            crate::DISPLAY_WIDTH * 4,
        )?;
        #[cfg(test)]
        let producer = slots.producer();
        let broker = slots.broker();

        #[cfg(test)]
        {
            let (command_tx, command_rx) = mpsc::channel();
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let test_producer = producer.clone();
            let owner = thread::Builder::new()
                .name("sliver-lua".into())
                .spawn(move || {
                    owner_main(
                        source,
                        initial_backlight,
                        initial_input,
                        producer,
                        command_rx,
                        ready_tx,
                    )
                })
                .context("starting Lua owner thread")?;
            let mut worker = Self {
                commands: Some(command_tx),
                owner: Some(owner),
                process: None,
                broker,
                producer: test_producer,
            };
            return match ready_rx.recv() {
                Ok(Ok(())) => Ok(StagedLuaWorker { worker }),
                Ok(Err(error)) => {
                    worker.abandon();
                    Err(anyhow!(error))
                }
                Err(_) => {
                    worker.abandon();
                    Err(anyhow!("Lua owner thread exited before staging completed"))
                }
            };
        }

        #[cfg(not(test))]
        {
            let process = worker_process::ProcessWorker::stage_with_frames(
                &source,
                initial_backlight,
                initial_input,
                &frame_path,
                broker.clone(),
                identity,
            )?;
            Ok(StagedLuaWorker {
                worker: Self {
                    commands: None,
                    owner: None,
                    process: Some(process),
                    broker,
                    #[cfg(test)]
                    producer,
                },
            })
        }
    }

    #[allow(dead_code)]
    pub(crate) fn render_next(&self) -> Result<LogicalFrame> {
        self.render_at(0.0, 0.0).map(|frame| frame.frame)
    }

    pub(crate) fn render_at(&self, presentation_time: f64, delta: f64) -> Result<TimedFrame> {
        self.render_at_with_input(presentation_time, delta, InputState::default())
    }

    pub(crate) fn render_at_with_input(
        &self,
        presentation_time: f64,
        delta: f64,
        input_state: InputState,
    ) -> Result<TimedFrame> {
        if let Some(process) = &self.process {
            return process.render(presentation_time, delta, input_state);
        }
        let deadline = Instant::now() + Duration::from_millis(50);
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Render(
                presentation_time,
                delta,
                input_state,
                reply_tx,
            ))
            .context("requesting a Lua frame")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while rendering")?
            .map_err(|error| anyhow!(error))?;
        if let Some(frame) = self.take_frame()? {
            return Ok(frame);
        }
        let mut backoff = Duration::from_millis(1);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "candidate frame could not be published within bounded 50 ms slot wait"
                ));
            }
            if self.retry_pending(presentation_time)? {
                if let Some(frame) = self.take_frame()? {
                    return Ok(frame);
                }
            }
            thread::sleep(std::cmp::min(backoff, remaining));
            backoff = std::cmp::min(backoff + backoff, Duration::from_millis(10));
        }
    }

    fn retry_pending(&self, presentation_time: f64) -> Result<bool> {
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::RetryPending(presentation_time, reply_tx))
            .context("retrying a pending Lua frame")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while retrying a frame")?
            .map_err(|error| anyhow!(error))
    }

    #[cfg(test)]
    pub(crate) fn render_to_slots_at(&self, presentation_time: f64, delta: f64) -> Result<()> {
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Render(
                presentation_time,
                delta,
                InputState::default(),
                reply_tx,
            ))
            .context("requesting a Lua frame for the shared slots")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while rendering")?
            .map_err(|error| anyhow!(error))?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn broker_for_test(&self) -> FrameBroker {
        self.broker.clone()
    }

    #[cfg(test)]
    pub(crate) fn hold_slots_for_test(&self) -> Vec<FrameWriter> {
        self.producer.hold_slots_for_test()
    }

    pub(crate) fn pending_backlight(&self) -> Result<Option<f64>> {
        if let Some(process) = &self.process {
            return process.pending_backlight();
        }
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::PendingBacklight(reply_tx))
            .context("reading staged Lua backlight")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while reading backlight")?
            .map_err(|error| anyhow!(error))
    }

    pub(crate) fn commit(&self, now_seconds: f64, input_state: InputState) -> Result<()> {
        if let Some(process) = &self.process {
            return process.commit(now_seconds, input_state);
        }
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Commit(now_seconds, input_state, reply_tx))
            .context("committing Lua worker timers")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while committing")?
            .map_err(|error| anyhow!(error))
    }

    pub(crate) fn drive(&self, request: DriveRequest) -> Result<WorkerEffects> {
        self.drive_until(request, Instant::now() + Duration::from_secs(2))
    }

    pub(crate) fn drive_until(
        &self,
        request: DriveRequest,
        deadline: Instant,
    ) -> Result<WorkerEffects> {
        if let Some(process) = &self.process {
            return process.drive_until(request, deadline);
        }
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Drive(request, reply_tx))
            .context("driving Lua worker")?;
        let effects = reply_rx
            .recv()
            .context("Lua owner thread exited while driving")?
            .map_err(|error| anyhow!(error))?;
        let frame = match effects.frame {
            Some(_) => Some(
                self.take_frame()?
                    .context("Lua worker published no frame")?,
            ),
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

    fn take_frame(&self) -> Result<Option<TimedFrame>> {
        let Some(completed) = self.broker.take_newest()? else {
            return Ok(None);
        };
        let (frame, timing) = LogicalFrame::from_completed(completed);
        Ok(Some(TimedFrame { frame, timing }))
    }

    pub(crate) fn restore_backlight(&self, level: f64) -> Result<()> {
        validate_backlight_level(level)?;
        if let Some(process) = &self.process {
            return process.restore_backlight(level);
        }
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::RestoreBacklight(level, reply_tx))
            .context("restoring Lua backlight state")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while restoring backlight")?
            .map_err(|error| anyhow!(error))
    }

    #[cfg(test)]
    pub(crate) fn shutdown(self, reason: StopReason) -> Result<()> {
        self.shutdown_until(reason, Instant::now() + Duration::from_millis(500))
    }

    pub(crate) fn shutdown_until(mut self, reason: StopReason, deadline: Instant) -> Result<()> {
        if let Some(process) = self.process.take() {
            return process.shutdown_until(reason, deadline);
        }
        let commands = self
            .commands
            .take()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Shutdown(reason, reply_tx))
            .context("requesting Lua worker shutdown")?;
        let result = reply_rx
            .recv()
            .context("Lua owner thread exited during shutdown")?;
        self.join_owner()?;
        result.map_err(|error| anyhow!(error))
    }

    fn abandon(&mut self) {
        if let Some(process) = self.process.take() {
            process.terminate();
        }
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(WorkerCommand::Abandon);
        }
        let _ = self.join_owner();
    }

    pub(crate) fn is_alive(&self) -> bool {
        if let Some(process) = &self.process {
            return process.is_alive();
        }
        self.owner
            .as_ref()
            .is_some_and(|owner| !owner.is_finished())
    }

    pub(crate) fn failure_reason(&self) -> Option<String> {
        self.process
            .as_ref()
            .and_then(worker_process::ProcessWorker::failure_reason)
    }

    pub(crate) fn uses_process_backend(&self) -> bool {
        self.process.is_some()
    }

    #[cfg(test)]
    pub(crate) fn exit_owner_for_test(&mut self) -> Result<()> {
        if let Some(commands) = self.commands.take() {
            commands
                .send(WorkerCommand::Abandon)
                .context("stopping Lua owner for test")?;
        }
        self.join_owner()
    }

    fn join_owner(&mut self) -> Result<()> {
        if let Some(owner) = self.owner.take() {
            owner
                .join()
                .map_err(|_| anyhow!("Lua owner thread panicked"))?;
        }
        Ok(())
    }
}

impl Drop for LuaWorker {
    fn drop(&mut self) {
        self.abandon();
    }
}

#[allow(dead_code)]
fn owner_main(
    source: LuaSource,
    initial_backlight: f64,
    initial_input: InputState,
    producer: FrameProducer,
    commands: mpsc::Receiver<WorkerCommand>,
    ready: mpsc::SyncSender<std::result::Result<(), String>>,
) {
    let runtime = match Runtime::load(&source, initial_backlight, initial_input, producer) {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }

    run_commands(runtime, commands);
}

#[allow(dead_code)]
fn run_commands(mut runtime: Runtime, commands: mpsc::Receiver<WorkerCommand>) {
    loop {
        match commands.recv() {
            Ok(WorkerCommand::Render(presentation_time, delta, input_state, reply)) => {
                let result = runtime.render_and_queue(presentation_time, delta, input_state);
                let _ = reply.send(result);
            }
            Ok(WorkerCommand::Commit(now_seconds, input_state, reply)) => {
                let _ = reply.send(runtime.commit(now_seconds, input_state));
            }
            Ok(WorkerCommand::RetryPending(presentation_time, reply)) => {
                let result = runtime
                    .try_publish_pending(presentation_time)
                    .map(|timing| timing.is_some());
                let _ = reply.send(result);
            }
            Ok(WorkerCommand::PendingBacklight(reply)) => {
                let _ = reply.send(Ok(runtime.pending_backlight()));
            }
            Ok(WorkerCommand::Drive(request, reply)) => {
                let _ = reply.send(runtime.drive(request));
            }
            Ok(WorkerCommand::RestoreBacklight(level, reply)) => {
                let _ = reply.send(runtime.restore_backlight(level));
            }
            Ok(WorkerCommand::Shutdown(reason, reply)) => {
                let _ = reply.send(runtime.stop(reason));
                break;
            }
            Ok(WorkerCommand::Abandon) | Err(_) => break,
        }
    }
}

impl Runtime {
    fn pending_backlight(&self) -> Option<f64> {
        self.controls.pending_backlight.get()
    }

    fn render_and_queue(
        &mut self,
        presentation_time: f64,
        delta: f64,
        input_state: InputState,
    ) -> std::result::Result<(), String> {
        let timing =
            FrameTiming::new(presentation_time, delta).map_err(|error| error.to_string())?;
        self.controls.input_state.set(input_state);
        let frame = self.render_frame(presentation_time, delta)?;
        self.pending_frame = Some(PendingFrame { frame, timing });
        self.try_publish_pending(presentation_time)?;
        Ok(())
    }

    fn try_publish_pending(
        &mut self,
        now_seconds: f64,
    ) -> std::result::Result<Option<FrameTiming>, String> {
        let Some(pending) = self.pending_frame.take() else {
            self.pending_retry_deadline = None;
            return Ok(None);
        };
        if self.publish_frame(&pending.frame, pending.timing)? {
            self.pending_retry_deadline = None;
            Ok(Some(pending.timing))
        } else {
            self.pending_frame = Some(pending);
            self.pending_retry_deadline = Some(now_seconds + PENDING_FRAME_RETRY_SECONDS);
            Ok(None)
        }
    }

    fn next_worker_deadline(&self) -> Option<f64> {
        let pending_retry = self
            .visible
            .then_some(self.pending_retry_deadline)
            .flatten();
        let timer_deadline = (!self.timers_paused)
            .then(|| self.controls.timers.borrow().next_deadline())
            .flatten();
        earliest_deadline(timer_deadline, pending_retry)
    }

    fn restore_backlight(&mut self, level: f64) -> std::result::Result<(), String> {
        validate_backlight_level(level).map_err(|error| error.to_string())?;
        self.controls.backlight_level.set(level);
        self.controls.pending_backlight.set(None);
        Ok(())
    }

    fn commit(
        &mut self,
        now_seconds: f64,
        input_state: InputState,
    ) -> std::result::Result<(), String> {
        validate_now(now_seconds)?;
        FrameTiming::new(now_seconds, 0.0).map_err(|error| error.to_string())?;
        self.controls.input_state.set(input_state);
        if self.controls.committed.replace(true) {
            return Err("Lua worker was already committed".into());
        }
        self.controls.timers.borrow_mut().activate(now_seconds);
        self.controls.pending_backlight.set(None);
        Ok(())
    }

    fn drive(&mut self, request: DriveRequest) -> std::result::Result<RuntimeEffects, String> {
        let DriveRequest {
            now_seconds,
            input_state,
            transitions,
            delta,
            events,
            options,
        } = request;
        let timing = FrameTiming::new(now_seconds, delta).map_err(|error| error.to_string())?;
        if !self.controls.committed.get() {
            return Err("Lua worker has not been committed".into());
        }

        let started = Instant::now();
        self.controls.now_seconds.set(Some(now_seconds));
        self.controls.input_state.set(input_state);
        let result = (|| {
            if options.resume_timers && self.timers_paused {
                if let Some(paused_since) = self.paused_since.take() {
                    self.controls
                        .timers
                        .borrow_mut()
                        .shift((now_seconds - paused_since).max(0.0));
                }
                self.timers_paused = false;
            }
            self.dispatch_keys(now_seconds, started, transitions)?;
            self.dispatch_touch(now_seconds, started, events)?;
            if let Some((visible, reason)) = options.visibility {
                if reason == VisibilityReason::Suspend {
                    if visible {
                        if let Some(paused_since) = self.paused_since.take() {
                            self.controls
                                .timers
                                .borrow_mut()
                                .shift((now_seconds - paused_since).max(0.0));
                        }
                        self.timers_paused = false;
                    } else {
                        self.timers_paused = true;
                        self.paused_since = Some(now_seconds);
                    }
                }
                self.visible = visible;
                self.dispatch_visibility(visible, reason, now_seconds, started)?;
            }
            if !self.timers_paused {
                self.run_due_timers(now_seconds, started)?;
            }
            self.controls
                .now_seconds
                .set(Some(sample_now(now_seconds, started)));
            let redraw_requested = self.controls.redraw_pending.get();
            if self.visible {
                self.controls.redraw_pending.set(false);
                if options.force_render || redraw_requested {
                    let frame = self.render_frame(now_seconds, delta)?;
                    self.pending_frame = Some(PendingFrame { frame, timing });
                }
            }
            let frame = if self.visible {
                self.try_publish_pending(now_seconds)?
            } else {
                None
            };
            self.controls
                .now_seconds
                .set(Some(sample_now(now_seconds, started)));
            let redraw_pending = self.controls.redraw_pending.get();
            let backlight = self.controls.pending_backlight.take();
            let key_requests = self.controls.key_requests.borrow_mut().drain(..).collect();
            let next_worker_deadline = self.next_worker_deadline();
            Ok(RuntimeEffects {
                frame,
                backlight,
                key_requests,
                next_worker_deadline,
                redraw_pending,
            })
        })();
        if result.is_err() {
            self.controls.key_requests.borrow_mut().clear();
        }
        self.controls.now_seconds.set(None);
        result
    }

    fn invoke_callback(
        &self,
        callback: &Function,
        stage: &'static str,
        now_seconds: f64,
        started: Instant,
        table: Table,
    ) -> std::result::Result<(), String> {
        self.controls
            .now_seconds
            .set(Some(sample_now(now_seconds, started)));
        callback
            .call::<()>(table)
            .map_err(|error| diagnostic(stage, &self.source, error.to_string()))
    }

    fn dispatch_keys(
        &self,
        now_seconds: f64,
        started: Instant,
        transitions: Vec<InputTransition>,
    ) -> std::result::Result<(), String> {
        let Some(key) = &self.key else {
            return Ok(());
        };
        for transition in transitions {
            self.controls.input_state.set(transition.state);
            let table = key_event_table(&self._lua, &transition)
                .map_err(|error| diagnostic("key", &self.source, error.to_string()))?;
            self.invoke_callback(key, "key", now_seconds, started, table)?;
        }
        Ok(())
    }

    fn dispatch_visibility(
        &self,
        visible: bool,
        reason: VisibilityReason,
        now_seconds: f64,
        started: Instant,
    ) -> std::result::Result<(), String> {
        let Some(visibility) = &self.visibility else {
            return Ok(());
        };
        let table = self
            ._lua
            .create_table()
            .map_err(|error| diagnostic("visibility", &self.source, error.to_string()))?;
        table
            .set("visible", visible)
            .map_err(|error| diagnostic("visibility", &self.source, error.to_string()))?;
        table
            .set("reason", reason.as_lua_str())
            .map_err(|error| diagnostic("visibility", &self.source, error.to_string()))?;
        self.invoke_callback(visibility, "visibility", now_seconds, started, table)
    }

    fn dispatch_touch(
        &self,
        now_seconds: f64,
        started: Instant,
        events: Vec<TouchEvent>,
    ) -> std::result::Result<(), String> {
        let Some(touch) = &self.touch else {
            return Ok(());
        };
        for event in events {
            let table = touch_event_table(&self._lua, &event)
                .map_err(|error| diagnostic("touch", &self.source, error.to_string()))?;
            self.invoke_callback(touch, "touch", now_seconds, started, table)?;
        }
        Ok(())
    }

    fn run_due_timers(
        &self,
        now_seconds: f64,
        started: Instant,
    ) -> std::result::Result<(), String> {
        let mut fired_repeating = BTreeSet::new();
        loop {
            let current = sample_now(now_seconds, started);
            self.controls.now_seconds.set(Some(current));
            let Some(due) = self.controls.timers.borrow().due(current, &fired_repeating) else {
                return Ok(());
            };
            let result = due
                .callback
                .call::<()>(())
                .map_err(|error| diagnostic("timer", &self.source, error.to_string()));
            let completed = sample_now(now_seconds, started);
            self.controls.now_seconds.set(Some(completed));
            self.controls.timers.borrow_mut().finish(
                due.id,
                due.scheduled_deadline,
                due.interval,
                completed,
            );
            if due.interval.is_some() {
                fired_repeating.insert(due.id);
            }
            result?;
        }
    }

    fn load(
        source: &LuaSource,
        initial_backlight: f64,
        initial_input: InputState,
        producer: FrameProducer,
    ) -> std::result::Result<Self, String> {
        let description = source.describe();
        let bytes = description
            .read_bytes()
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let lua = unsafe { Lua::unsafe_new() };
        if let Some(path) = description.path {
            configure_lua_path(&lua, path)
                .map_err(|error| diagnostic("load", source, error.to_string()))?;
        }
        let controls = RuntimeControls::new(initial_backlight, initial_input);
        let loaded_v1 = install_v1_module(&lua, &controls, description.metadata)
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let source_name = description.chunk_name;
        let entry = lua
            .load(&bytes)
            .set_name(source_name.clone())
            .into_function()
            .map_err(|error| diagnostic("load", source, error.to_string()))?;

        let last_entry_line = Rc::new(Cell::new(None));
        let hook_line = last_entry_line.clone();
        let hook_source = source_name.clone();
        lua.set_hook(HookTriggers::EVERY_LINE, move |_, debug| {
            let source = debug.source();
            if source.source.as_deref() == Some(hook_source.as_str()) {
                hook_line.set(debug.current_line());
            }
            Ok(VmState::Continue)
        })
        .map_err(|error| diagnostic("load", source, error.to_string()))?;

        let validation_started = Rc::new(Cell::new(false));
        let validator_started = validation_started.clone();
        let validator_loaded_v1 = loaded_v1.clone();
        let validator = lua
            .create_function(move |_, value: Value| {
                validator_started.set(true);
                validate_application(value, validator_loaded_v1.as_ref())
            })
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let validation_result = lua
            .load(
                r#"
                local entry, validate = ...
                local application = entry()
                validate(application)
                return application
                "#,
            )
            .set_name("=sliver validation")
            .call::<Value>((entry, validator));
        lua.remove_hook();

        let value = validation_result.map_err(|error| {
            if validation_started.get() {
                validation_diagnostic(source, last_entry_line.get(), error.to_string())
            } else {
                diagnostic("load", source, error.to_string())
            }
        })?;
        let application = match value {
            Value::Table(application) => application,
            value => {
                return Err(validation_diagnostic(
                    source,
                    last_entry_line.get(),
                    format!(
                        "validation wrapper returned an application {}, not a table",
                        value.type_name()
                    ),
                ));
            }
        };
        let CallbackRefs {
            start,
            stop,
            visibility,
            touch,
            key,
            render,
        } = extract_callback_refs(&application).map_err(|error| {
            validation_diagnostic(source, last_entry_line.get(), error.to_string())
        })?;
        if let Some(start) = start {
            start
                .call::<()>(())
                .map_err(|error| diagnostic("start", source, error.to_string()))?;
        }

        let runtime = Self {
            _lua: lua,
            render,
            stop,
            visibility,
            touch,
            key,
            source: source.clone(),
            controls,
            visible: true,
            timers_paused: false,
            paused_since: None,
            producer,
            pending_frame: None,
            pending_retry_deadline: None,
        };
        Ok(runtime)
    }

    fn publish_frame(
        &self,
        frame: &LogicalFrame,
        timing: FrameTiming,
    ) -> std::result::Result<bool, String> {
        let published = self
            .producer
            .try_publish(
                frame.width(),
                frame.height(),
                frame.stride(),
                frame.pixels(),
                timing,
            )
            .map_err(|error| error.to_string())?;
        Ok(published)
    }

    fn render_frame(
        &self,
        presentation_time: f64,
        delta: f64,
    ) -> std::result::Result<LogicalFrame, String> {
        self.controls.redraw_pending.set(false);
        let frame = FrameCanvas::new()
            .map_err(|error| diagnostic("render", &self.source, format!("{error:#}")))?;
        let context = frame.context();
        let canvas = self
            ._lua
            .create_userdata(Canvas::new(context))
            .map_err(|error| diagnostic("render", &self.source, error.to_string()))?;
        let render_result = self
            .render
            .call::<()>((canvas.clone(), presentation_time, delta));
        canvas
            .borrow::<Canvas>()
            .map_err(|error| diagnostic("render", &self.source, error.to_string()))?
            .invalidate();
        render_result.map_err(|error| diagnostic("render", &self.source, error.to_string()))?;
        frame
            .finish()
            .map_err(|error| diagnostic("render", &self.source, format!("{error:#}")))
    }

    fn stop(&self, reason: StopReason) -> std::result::Result<(), String> {
        if let Some(stop) = &self.stop {
            stop.call::<()>(reason.as_str())
                .map_err(|error| diagnostic("stop", &self.source, error.to_string()))?;
        }
        Ok(())
    }
}

fn configure_lua_path(lua: &Lua, source: &Path) -> mlua::Result<()> {
    let directory = source.parent().unwrap_or_else(|| Path::new("."));
    let package: Table = lua.globals().get("package")?;

    let existing_lua: String = package.get("path")?;
    let lua_direct = directory.join("?.lua");
    let lua_nested = directory.join("?/init.lua");
    package.set(
        "path",
        format!(
            "{};{};{existing_lua}",
            lua_direct.to_string_lossy(),
            lua_nested.to_string_lossy()
        ),
    )?;

    let existing_c: String = package.get("cpath")?;
    let c_direct = directory.join("?.so");
    let c_nested = directory.join("?/init.so");
    package.set(
        "cpath",
        format!(
            "{};{};{existing_c}",
            c_direct.to_string_lossy(),
            c_nested.to_string_lossy()
        ),
    )
}

impl RuntimeControls {
    fn new(initial_backlight: f64, initial_input: InputState) -> Self {
        Self {
            redraw_pending: Rc::new(Cell::new(false)),
            timers: Rc::new(RefCell::new(TimerRegistry::new())),
            committed: Rc::new(Cell::new(false)),
            now_seconds: Rc::new(Cell::new(None)),
            backlight_level: Rc::new(Cell::new(initial_backlight)),
            pending_backlight: Rc::new(Cell::new(None)),
            input_state: Rc::new(Cell::new(initial_input)),
            key_requests: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl TimerRegistry {
    fn new() -> Self {
        Self {
            next_id: 1,
            entries: Vec::new(),
        }
    }

    fn add(
        &mut self,
        delay: f64,
        interval: Option<f64>,
        callback: Function,
        now_seconds: Option<f64>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.entries.push(TimerEntry {
            id,
            delay,
            interval,
            next_deadline: now_seconds.map(|now| now + delay),
            callback,
        });
        id
    }

    fn activate(&mut self, now_seconds: f64) {
        for entry in &mut self.entries {
            if entry.next_deadline.is_none() {
                entry.next_deadline = Some(now_seconds + entry.delay);
            }
        }
    }

    fn cancel(&mut self, id: u64) {
        self.entries.retain(|entry| entry.id != id);
    }

    fn due(&self, now_seconds: f64, fired_repeating: &BTreeSet<u64>) -> Option<DueTimer> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let deadline = entry.next_deadline?;
                if entry.interval.is_some() && fired_repeating.contains(&entry.id) {
                    return None;
                }
                (deadline <= now_seconds).then_some((deadline, entry))
            })
            .min_by(|(left, _), (right, _)| left.total_cmp(right))
            .map(|(scheduled_deadline, entry)| DueTimer {
                id: entry.id,
                scheduled_deadline,
                interval: entry.interval,
                callback: entry.callback.clone(),
            })
    }

    fn finish(&mut self, id: u64, scheduled_deadline: f64, interval: Option<f64>, now: f64) {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return;
        };
        match interval {
            None => {
                self.entries.swap_remove(index);
            }
            Some(interval) => {
                let elapsed = (now - scheduled_deadline).max(0.0);
                let skipped = (elapsed / interval).floor() + 1.0;
                let mut next = scheduled_deadline + skipped * interval;
                if !next.is_finite() || next <= now {
                    next = now + interval;
                }
                self.entries[index].next_deadline = Some(next);
            }
        }
    }

    fn next_deadline(&self) -> Option<f64> {
        self.entries
            .iter()
            .filter_map(|entry| entry.next_deadline)
            .min_by(f64::total_cmp)
    }

    fn shift(&mut self, amount: f64) {
        if amount <= 0.0 {
            return;
        }
        for entry in &mut self.entries {
            if let Some(deadline) = entry.next_deadline {
                entry.next_deadline = Some(deadline + amount);
            }
        }
    }
}

impl UserData for TimerHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("cancel", |_, timer, ()| {
            timer.timers.borrow_mut().cancel(timer.id);
            Ok(())
        });
    }
}

fn validate_backlight_level(level: f64) -> Result<()> {
    ensure!(
        level.is_finite() && (0.0..=1.0).contains(&level),
        "backlight level must be finite and between 0.0 and 1.0"
    );
    Ok(())
}

fn sample_now(now_seconds: f64, started: Instant) -> f64 {
    now_seconds + started.elapsed().as_secs_f64()
}

fn validate_now(now_seconds: f64) -> std::result::Result<(), String> {
    if now_seconds.is_finite() {
        Ok(())
    } else {
        Err("worker time must be finite".into())
    }
}
fn validate_after_delay(delay: f64) -> mlua::Result<()> {
    if delay.is_finite() && delay >= 0.0 {
        Ok(())
    } else {
        Err(mlua::Error::runtime(
            "timer.after delay must be finite and non-negative",
        ))
    }
}

fn validate_every_interval(interval: f64) -> mlua::Result<()> {
    if interval.is_finite() && interval > 0.0 {
        Ok(())
    } else {
        Err(mlua::Error::runtime(
            "timer.every interval must be finite and positive",
        ))
    }
}

impl UserData for LuaKey {}

fn create_key_constants(lua: &Lua) -> mlua::Result<Table> {
    let keys = lua.create_table()?;
    let keyboard = lua.create_table()?;
    let consumer = lua.create_table()?;
    for metadata in output_key_metadata() {
        let table = match metadata.key {
            OutputKey::Keyboard(_) => &keyboard,
            OutputKey::Consumer(_) => &consumer,
        };
        table.set(metadata.name, lua.create_userdata(LuaKey(metadata.key))?)?;
    }
    keys.set("keyboard", keyboard)?;
    keys.set("consumer", consumer)?;
    Ok(keys)
}

fn create_key_operation(
    lua: &Lua,
    controls: &RuntimeControls,
    operation: KeyOperation,
) -> mlua::Result<Function> {
    let committed = controls.committed.clone();
    let requests = controls.key_requests.clone();
    lua.create_function(move |_, (value, options): (Value, Option<Table>)| {
        if !committed.get() {
            return Err(mlua::Error::runtime(
                "synthetic key output is unavailable while staging",
            ));
        }
        let key = parse_lua_key(value)?;
        let modifiers = parse_modifier_mode(options)?;
        let mut requests = requests.borrow_mut();
        if requests.len() >= MAX_KEY_REQUESTS_PER_DRIVE {
            return Err(mlua::Error::runtime("synthetic key request limit exceeded"));
        }
        requests.push(KeyRequest {
            operation,
            key,
            modifiers,
        });
        Ok(())
    })
}

fn parse_lua_key(value: Value) -> mlua::Result<OutputKey> {
    match value {
        Value::UserData(value) => Ok(value.borrow::<LuaKey>()?.0),
        value => Err(mlua::Error::runtime(format!(
            "key constant must be a Sliver key, got {}",
            value.type_name()
        ))),
    }
}

fn parse_modifier_mode(options: Option<Table>) -> mlua::Result<ModifierMode> {
    let Some(options) = options else {
        return Ok(ModifierMode::Inherit);
    };
    let value: Value = options.raw_get("modifiers")?;
    match value {
        Value::Nil => Ok(ModifierMode::Inherit),
        Value::Boolean(false) => Ok(ModifierMode::None),
        Value::String(value) => match value.to_str()?.as_ref() {
            "inherit" | "default" => Ok(ModifierMode::Inherit),
            "none" | "suppress" => Ok(ModifierMode::None),
            value => Err(mlua::Error::runtime(format!(
                "unknown modifier mode {value:?}"
            ))),
        },
        Value::Table(values) => values
            .sequence_values::<Value>()
            .map(|value| value.and_then(parse_lua_key))
            .collect::<mlua::Result<Vec<_>>>()
            .map(ModifierMode::Explicit),
        value => Err(mlua::Error::runtime(format!(
            "key modifiers must be omitted, false, a mode, or a key list, got {}",
            value.type_name()
        ))),
    }
}

fn install_v1_module(
    lua: &Lua,
    controls: &RuntimeControls,
    source_metadata: Option<SourceMetadata>,
) -> mlua::Result<Rc<Cell<bool>>> {
    let loaded = Rc::new(Cell::new(false));
    let loaded_by_require = loaded.clone();
    let loader_controls = controls.clone();
    let loader_source_metadata = source_metadata;
    let loader = lua.create_function(move |lua, _: MultiValue| {
        loaded_by_require.set(true);
        let module = lua.create_table()?;
        module.set("api_version", 1)?;
        if let Some(metadata) = &loader_source_metadata {
            let source = lua.create_table()?;
            source.set("path", metadata.path.as_str())?;
            source.set("directory", metadata.directory.as_str())?;
            module.set("source", source)?;
        }
        let path = lua.create_function(create_path)?;
        module.set("path", path)?;
        let image = lua.create_table()?;
        image.set("new", lua.create_function(create_image)?)?;
        module.set("image", image)?;

        let redraw_pending = loader_controls.redraw_pending.clone();
        module.set(
            "redraw",
            lua.create_function(move |_, ()| {
                redraw_pending.set(true);
                Ok(())
            })?,
        )?;

        let input = lua.create_table()?;
        let input_state = loader_controls.input_state.clone();
        input.set(
            "state",
            lua.create_function(move |lua, ()| input_state_table(lua, input_state.get()))?,
        )?;
        module.set("input", input)?;

        let keys = create_key_constants(lua)?;
        let key_api = lua.create_table()?;
        for operation in [KeyOperation::Down, KeyOperation::Up, KeyOperation::Tap] {
            let name = match operation {
                KeyOperation::Down => "down",
                KeyOperation::Up => "up",
                KeyOperation::Tap => "tap",
            };
            key_api.set(
                name,
                create_key_operation(lua, &loader_controls, operation)?,
            )?;
        }
        key_api.set("keyboard", keys.get::<Table>("keyboard")?)?;
        key_api.set("consumer", keys.get::<Table>("consumer")?)?;
        let input: Table = module.get("input")?;
        input.set("keys", keys)?;
        input.set("key", key_api)?;

        let timer = lua.create_table()?;
        let after_controls = loader_controls.clone();
        timer.set(
            "after",
            lua.create_function(move |lua, (delay, callback): (f64, Function)| {
                validate_after_delay(delay)?;
                let now = if after_controls.committed.get() {
                    after_controls.now_seconds.get()
                } else {
                    None
                };
                let id = after_controls
                    .timers
                    .borrow_mut()
                    .add(delay, None, callback, now);
                lua.create_userdata(TimerHandle {
                    id,
                    timers: after_controls.timers.clone(),
                })
            })?,
        )?;
        let every_controls = loader_controls.clone();
        timer.set(
            "every",
            lua.create_function(move |lua, (interval, callback): (f64, Function)| {
                validate_every_interval(interval)?;
                let now = if every_controls.committed.get() {
                    every_controls.now_seconds.get()
                } else {
                    None
                };
                let id =
                    every_controls
                        .timers
                        .borrow_mut()
                        .add(interval, Some(interval), callback, now);
                lua.create_userdata(TimerHandle {
                    id,
                    timers: every_controls.timers.clone(),
                })
            })?,
        )?;
        module.set("timer", timer)?;

        let backlight = lua.create_table()?;
        let get_level = loader_controls.backlight_level.clone();
        backlight.set(
            "get",
            lua.create_function(move |_, ()| Ok(get_level.get()))?,
        )?;
        let set_level = loader_controls.backlight_level.clone();
        let pending_level = loader_controls.pending_backlight.clone();
        backlight.set(
            "set",
            lua.create_function(move |_, level: f64| {
                validate_backlight_level(level)
                    .map_err(|error| mlua::Error::runtime(error.to_string()))?;
                if set_level.get() != level {
                    set_level.set(level);
                    pending_level.set(Some(level));
                }
                Ok(())
            })?,
        )?;
        module.set("backlight", backlight)?;
        Ok(module)
    })?;
    let package: Table = lua.globals().get("package")?;
    let preload: Table = package.get("preload")?;
    preload.set("sliver.v1", loader)?;
    Ok(loaded)
}

fn key_event_table(lua: &Lua, transition: &InputTransition) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let name = match transition.key {
        ObservedKey::Fn => "fn",
        ObservedKey::Modifier(modifier) => modifier_name(modifier),
    };
    table.set("key", name)?;
    table.set("name", name)?;
    table.set("phase", if transition.active { "down" } else { "up" })?;
    table.set("active", transition.active)?;
    table.set("state", input_state_table(lua, transition.state)?)?;
    Ok(table)
}

fn modifier_name(modifier: Modifier) -> &'static str {
    output_key_metadata()
        .iter()
        .find(|metadata| metadata.modifier == Some(modifier))
        .map(|metadata| metadata.name)
        .expect("every modifier has output key metadata")
}

fn input_state_table(lua: &Lua, state: InputState) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("fn", state.fn_active)?;
    let modifiers = lua.create_table()?;
    for modifier in Modifier::ALL {
        modifiers.set(modifier_name(modifier), state.modifiers.is_active(modifier))?;
    }
    table.set("modifiers", modifiers)?;
    Ok(table)
}

fn touch_event_table(lua: &Lua, event: &TouchEvent) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let phase = match event.phase {
        TouchPhase::Down => "down",
        TouchPhase::Move => "move",
        TouchPhase::Up => "up",
        TouchPhase::Cancel => "cancel",
    };
    table.set("phase", phase)?;
    table.set("id", event.id)?;
    table.set("time", event.time)?;
    table.set("x", event.x)?;
    table.set("y", event.y)?;

    let modifiers = lua.create_table()?;
    for modifier in Modifier::ALL {
        modifiers.set(modifier_name(modifier), event.modifiers.is_active(modifier))?;
    }
    table.set("modifiers", modifiers)?;
    if let Some(pressure) = event.pressure {
        table.set("pressure", pressure)?;
    }
    if let Some(width) = event.width {
        table.set("width", width)?;
    }
    if let Some(height) = event.height {
        table.set("height", height)?;
    }
    Ok(table)
}

fn validate_application(value: Value, loaded_v1: &Cell<bool>) -> mlua::Result<()> {
    let application = match value {
        Value::Table(application) => application,
        value => {
            return Err(mlua::Error::runtime(format!(
                "config must return an application table, got {}",
                value.type_name()
            )));
        }
    };
    if !loaded_v1.get() {
        return Err(mlua::Error::runtime("config must load sliver.v1"));
    }
    validate_fields(&application)?;
    match application.raw_get::<Value>("api_version")? {
        Value::Integer(1) => {}
        _ => {
            return Err(mlua::Error::runtime("api_version must be integer 1"));
        }
    }
    for field in ["start", "stop", "visibility", "touch", "key"] {
        validate_callback(&application, field, false)?;
    }
    validate_callback(&application, "render", true)
}

fn validate_fields(application: &Table) -> mlua::Result<()> {
    const ALLOWED: [&str; 7] = [
        "api_version",
        "start",
        "stop",
        "visibility",
        "touch",
        "key",
        "render",
    ];

    for pair in application.pairs::<Value, Value>() {
        let (key, _) = pair?;
        let Value::String(key) = key else {
            return Err(mlua::Error::runtime(format!(
                "unknown non-string application field of type {}",
                key.type_name()
            )));
        };
        let field = key.to_str()?;
        if !ALLOWED.contains(&field.as_ref()) {
            return Err(mlua::Error::runtime(format!(
                "unknown application field {field:?}"
            )));
        }
    }
    Ok(())
}

fn validate_callback(application: &Table, field: &str, required: bool) -> mlua::Result<()> {
    match application.raw_get::<Value>(field)? {
        Value::Nil if !required => Ok(()),
        Value::Function(_) => Ok(()),
        Value::Nil => Err(mlua::Error::runtime(format!(
            "missing required {field} callback"
        ))),
        value => Err(mlua::Error::runtime(format!(
            "{field} must be a function, got {}",
            value.type_name()
        ))),
    }
}

fn extract_callback_refs(application: &Table) -> mlua::Result<CallbackRefs> {
    Ok(CallbackRefs {
        start: application.raw_get("start")?,
        stop: application.raw_get("stop")?,
        visibility: application.raw_get("visibility")?,
        touch: application.raw_get("touch")?,
        key: application.raw_get("key")?,
        render: application.raw_get("render")?,
    })
}

fn traceback_suffix(detail: &str) -> &'static str {
    if detail.contains("stack traceback:") {
        ""
    } else {
        "\nstack traceback: unavailable (no Lua traceback was produced)"
    }
}

fn diagnostic(stage: &str, source: &LuaSource, detail: String) -> String {
    let traceback = traceback_suffix(&detail);
    let label = source.describe().label;
    format!("lua {label} [{stage}]: {detail}{traceback}")
}

fn validation_diagnostic(source: &LuaSource, line: Option<usize>, detail: String) -> String {
    let label = source.describe().label;
    let location = line.map_or_else(
        || format!("{label} (line unavailable)"),
        |line| format!("{label}:{line}"),
    );
    format!(
        "lua {location} [validation]: {detail}{}",
        traceback_suffix(&detail)
    )
}
