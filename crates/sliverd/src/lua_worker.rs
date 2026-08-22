use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use anyhow::{anyhow, ensure, Context, Result};
use mlua::{
    Function, HookTriggers, Lua, MultiValue, Table, UserData, UserDataMethods, Value, VmState,
};

use crate::hardware::{LogicalFrame, Modifier, TouchEvent, TouchPhase};
use crate::lua_canvas::{create_path, Canvas};

pub(crate) struct StagedLuaWorker {
    pub(crate) worker: LuaWorker,
    pub(crate) frame: LogicalFrame,
    pub(crate) pending_backlight: Option<f64>,
}

pub(crate) struct WorkerEffects {
    pub(crate) frame: Option<LogicalFrame>,
    pub(crate) backlight: Option<f64>,
    pub(crate) next_timer_deadline: Option<f64>,
    pub(crate) redraw_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    Replaced,
    Shutdown,
}

impl StopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Shutdown => "shutdown",
        }
    }
}

pub(crate) struct LuaWorker {
    commands: Option<mpsc::Sender<WorkerCommand>>,
    owner: Option<thread::JoinHandle<()>>,
}

enum WorkerCommand {
    #[allow(dead_code)]
    Render(mpsc::SyncSender<std::result::Result<LogicalFrame, String>>),
    Commit(f64, mpsc::SyncSender<std::result::Result<(), String>>),
    Drive(
        f64,
        Vec<TouchEvent>,
        mpsc::SyncSender<std::result::Result<WorkerEffects, String>>,
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
    _visibility: Option<Function>,
    touch: Option<Function>,
    _key: Option<Function>,
    source: PathBuf,
    controls: RuntimeControls,
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

struct DueTimer {
    id: u64,
    scheduled_deadline: f64,
    interval: Option<f64>,
    callback: Function,
}

impl LuaWorker {
    pub(crate) fn stage(source: &Path) -> Result<StagedLuaWorker> {
        Self::stage_with_backlight(source, 0.0)
    }

    pub(crate) fn stage_with_backlight(
        source: &Path,
        initial_backlight: f64,
    ) -> Result<StagedLuaWorker> {
        validate_backlight_level(initial_backlight)?;
        let source = source.to_path_buf();
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let owner = thread::Builder::new()
            .name("sliver-lua".into())
            .spawn(move || owner_main(source, initial_backlight, command_rx, ready_tx))
            .context("starting Lua owner thread")?;

        let mut worker = Self {
            commands: Some(command_tx),
            owner: Some(owner),
        };
        match ready_rx.recv() {
            Ok(Ok(staged)) => Ok(StagedLuaWorker {
                worker,
                frame: staged.frame,
                pending_backlight: staged.pending_backlight,
            }),
            Ok(Err(error)) => {
                worker.abandon();
                Err(anyhow!(error))
            }
            Err(_) => {
                worker.abandon();
                Err(anyhow!("Lua owner thread exited before staging completed"))
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn render_next(&self) -> Result<LogicalFrame> {
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Render(reply_tx))
            .context("requesting the next Lua frame")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while rendering")?
            .map_err(|error| anyhow!(error))
    }

    pub(crate) fn commit(&self, now_seconds: f64) -> Result<()> {
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Commit(now_seconds, reply_tx))
            .context("committing Lua worker timers")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while committing")?
            .map_err(|error| anyhow!(error))
    }

    pub(crate) fn drive(&self, now_seconds: f64, events: Vec<TouchEvent>) -> Result<WorkerEffects> {
        let commands = self
            .commands
            .as_ref()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Drive(now_seconds, events, reply_tx))
            .context("driving Lua worker")?;
        reply_rx
            .recv()
            .context("Lua owner thread exited while driving")?
            .map_err(|error| anyhow!(error))
    }

    pub(crate) fn restore_backlight(&self, level: f64) -> Result<()> {
        validate_backlight_level(level)?;
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

    pub(crate) fn shutdown(mut self, reason: StopReason) -> Result<()> {
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
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(WorkerCommand::Abandon);
        }
        let _ = self.join_owner();
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

struct StagedRuntime {
    frame: LogicalFrame,
    pending_backlight: Option<f64>,
}

fn owner_main(
    source: PathBuf,
    initial_backlight: f64,
    commands: mpsc::Receiver<WorkerCommand>,
    ready: mpsc::SyncSender<std::result::Result<StagedRuntime, String>>,
) {
    let (runtime, frame) = match Runtime::load_and_render(&source, initial_backlight) {
        Ok(staged) => staged,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let staged = StagedRuntime {
        frame,
        pending_backlight: runtime.pending_backlight(),
    };
    if ready.send(Ok(staged)).is_err() {
        return;
    }

    run_commands(runtime, commands);
}

fn run_commands(mut runtime: Runtime, commands: mpsc::Receiver<WorkerCommand>) {
    loop {
        match commands.recv() {
            Ok(WorkerCommand::Render(reply)) => {
                let _ = reply.send(runtime.render_frame());
            }
            Ok(WorkerCommand::Commit(now_seconds, reply)) => {
                let _ = reply.send(runtime.commit(now_seconds));
            }
            Ok(WorkerCommand::Drive(now_seconds, events, reply)) => {
                let _ = reply.send(runtime.drive(now_seconds, events));
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

    fn restore_backlight(&mut self, level: f64) -> std::result::Result<(), String> {
        validate_backlight_level(level).map_err(|error| error.to_string())?;
        self.controls.backlight_level.set(level);
        self.controls.pending_backlight.set(None);
        Ok(())
    }

    fn commit(&mut self, now_seconds: f64) -> std::result::Result<(), String> {
        validate_now(now_seconds)?;
        if self.controls.committed.replace(true) {
            return Err("Lua worker was already committed".into());
        }
        self.controls.timers.borrow_mut().activate(now_seconds);
        self.controls.pending_backlight.set(None);
        Ok(())
    }

    fn drive(
        &mut self,
        now_seconds: f64,
        events: Vec<TouchEvent>,
    ) -> std::result::Result<WorkerEffects, String> {
        validate_now(now_seconds)?;
        if !self.controls.committed.get() {
            return Err("Lua worker has not been committed".into());
        }

        let started = Instant::now();
        self.controls.now_seconds.set(Some(now_seconds));
        let result = (|| {
            self.dispatch_touch(now_seconds, started, events)?;
            self.run_due_timers(now_seconds, started)?;
            self.controls
                .now_seconds
                .set(Some(sample_now(now_seconds, started)));
            let frame = if self.controls.redraw_pending.replace(false) {
                Some(self.render_frame()?)
            } else {
                None
            };
            self.controls
                .now_seconds
                .set(Some(sample_now(now_seconds, started)));
            let redraw_pending = self.controls.redraw_pending.get();
            let backlight = self.controls.pending_backlight.take();
            let next_timer_deadline = self.controls.timers.borrow().next_deadline();
            Ok(WorkerEffects {
                frame,
                backlight,
                next_timer_deadline,
                redraw_pending,
            })
        })();
        self.controls.now_seconds.set(None);
        result
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
            self.controls
                .now_seconds
                .set(Some(sample_now(now_seconds, started)));
            let table = touch_event_table(&self._lua, &event)
                .map_err(|error| diagnostic("touch", &self.source, error.to_string()))?;
            touch
                .call::<()>(table)
                .map_err(|error| diagnostic("touch", &self.source, error.to_string()))?;
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

    fn load_and_render(
        source: &Path,
        initial_backlight: f64,
    ) -> std::result::Result<(Self, LogicalFrame), String> {
        let bytes =
            std::fs::read(source).map_err(|error| diagnostic("load", source, error.to_string()))?;
        let lua = unsafe { Lua::unsafe_new() };
        configure_lua_path(&lua, source)
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let controls = RuntimeControls::new(initial_backlight);
        let loaded_v1 = install_v1_module(&lua, &controls)
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let source_name = format!("@{}", source.display());
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
            _visibility: visibility,
            touch,
            _key: key,
            source: source.to_path_buf(),
            controls,
        };
        let frame = runtime.render_frame()?;
        Ok((runtime, frame))
    }

    fn render_frame(&self) -> std::result::Result<LogicalFrame, String> {
        self.controls.redraw_pending.set(false);
        let surface = cairo::ImageSurface::create(
            cairo::Format::ARgb32,
            sliver_core::STRIP_W as i32,
            sliver_core::STRIP_H as i32,
        )
        .map_err(|error| diagnostic("render", &self.source, error.to_string()))?;
        let context = cairo::Context::new(&surface)
            .map_err(|error| diagnostic("render", &self.source, error.to_string()))?;
        context.set_operator(cairo::Operator::Source);
        context.set_source_rgb(0.0, 0.0, 0.0);
        context
            .paint()
            .map_err(|error| diagnostic("render", &self.source, error.to_string()))?;
        context.set_operator(cairo::Operator::Over);
        let canvas = self
            ._lua
            .create_userdata(Canvas::new(&context))
            .map_err(|error| diagnostic("render", &self.source, error.to_string()))?;
        let render_result = self.render.call::<()>(canvas.clone());
        canvas
            .borrow::<Canvas>()
            .map_err(|error| diagnostic("render", &self.source, error.to_string()))?
            .invalidate();
        render_result.map_err(|error| diagnostic("render", &self.source, error.to_string()))?;
        surface.flush();
        LogicalFrame::from_surface(&surface)
            .map_err(|error| diagnostic("render", &self.source, format!("{error:#}")))
    }

    fn stop(self, reason: StopReason) -> std::result::Result<(), String> {
        if let Some(stop) = self.stop {
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
    fn new(initial_backlight: f64) -> Self {
        Self {
            redraw_pending: Rc::new(Cell::new(false)),
            timers: Rc::new(RefCell::new(TimerRegistry::new())),
            committed: Rc::new(Cell::new(false)),
            now_seconds: Rc::new(Cell::new(None)),
            backlight_level: Rc::new(Cell::new(initial_backlight)),
            pending_backlight: Rc::new(Cell::new(None)),
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

fn install_v1_module(lua: &Lua, controls: &RuntimeControls) -> mlua::Result<Rc<Cell<bool>>> {
    let loaded = Rc::new(Cell::new(false));
    let loaded_by_require = loaded.clone();
    let loader_controls = controls.clone();
    let loader = lua.create_function(move |lua, _: MultiValue| {
        loaded_by_require.set(true);
        let module = lua.create_table()?;
        module.set("api_version", 1)?;
        let path = lua.create_function(create_path)?;
        module.set("path", path)?;

        let redraw_pending = loader_controls.redraw_pending.clone();
        module.set(
            "redraw",
            lua.create_function(move |_, ()| {
                redraw_pending.set(true);
                Ok(())
            })?,
        )?;

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
    for (name, modifier) in [
        ("left_ctrl", Modifier::LeftCtrl),
        ("right_ctrl", Modifier::RightCtrl),
        ("left_alt", Modifier::LeftAlt),
        ("right_alt", Modifier::RightAlt),
        ("left_shift", Modifier::LeftShift),
        ("right_shift", Modifier::RightShift),
        ("left_super", Modifier::LeftSuper),
        ("right_super", Modifier::RightSuper),
    ] {
        modifiers.set(name, event.modifiers.is_active(modifier))?;
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

fn diagnostic(stage: &str, source: &Path, detail: String) -> String {
    let traceback = traceback_suffix(&detail);
    format!("lua {} [{stage}]: {detail}{traceback}", source.display())
}

fn validation_diagnostic(source: &Path, line: Option<usize>, detail: String) -> String {
    let location = line.map_or_else(
        || format!("{} (line unavailable)", source.display()),
        |line| format!("{}:{line}", source.display()),
    );
    format!(
        "lua {location} [validation]: {detail}{}",
        traceback_suffix(&detail)
    )
}
