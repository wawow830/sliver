use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::hardware::LogicalFrame;
use crate::lua_canvas::Canvas;

pub(crate) struct StagedLuaWorker {
    pub(crate) worker: LuaWorker,
    pub(crate) frame: LogicalFrame,
}

pub(crate) struct LuaWorker {
    commands: Option<mpsc::Sender<WorkerCommand>>,
    owner: Option<thread::JoinHandle<()>>,
}

enum WorkerCommand {
    Shutdown(mpsc::SyncSender<std::result::Result<(), String>>),
    Abandon,
}

struct Runtime {
    _lua: Lua,
    stop: Option<Function>,
    source: PathBuf,
}

impl LuaWorker {
    pub(crate) fn stage(source: &Path) -> Result<StagedLuaWorker> {
        let source = source.to_path_buf();
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let owner = thread::Builder::new()
            .name("sliver-lua".into())
            .spawn(move || owner_main(source, command_rx, ready_tx))
            .context("starting Lua owner thread")?;

        let mut worker = Self {
            commands: Some(command_tx),
            owner: Some(owner),
        };
        match ready_rx.recv() {
            Ok(Ok(frame)) => Ok(StagedLuaWorker { worker, frame }),
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

    pub(crate) fn shutdown(mut self) -> Result<()> {
        let commands = self
            .commands
            .take()
            .context("Lua worker command channel is closed")?;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        commands
            .send(WorkerCommand::Shutdown(reply_tx))
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

fn owner_main(
    source: PathBuf,
    commands: mpsc::Receiver<WorkerCommand>,
    ready: mpsc::SyncSender<std::result::Result<LogicalFrame, String>>,
) {
    let (runtime, frame) = match Runtime::load_and_render(&source) {
        Ok(staged) => staged,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(frame)).is_err() {
        return;
    }

    match commands.recv() {
        Ok(WorkerCommand::Shutdown(reply)) => {
            let _ = reply.send(runtime.stop());
        }
        Ok(WorkerCommand::Abandon) | Err(_) => {}
    }
}

impl Runtime {
    fn load_and_render(source: &Path) -> std::result::Result<(Self, LogicalFrame), String> {
        let bytes =
            std::fs::read(source).map_err(|error| diagnostic("load", source, error.to_string()))?;
        let lua = unsafe { Lua::unsafe_new() };
        let loaded_v1 = install_v1_module(&lua)
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let value = lua
            .load(&bytes)
            .set_name(format!("@{}", source.display()))
            .eval::<Value>()
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let application = match value {
            Value::Table(application) => application,
            value => {
                return Err(diagnostic(
                    "validation",
                    source,
                    format!(
                        "config must return an application table, got {}",
                        value.type_name()
                    ),
                ));
            }
        };
        if !loaded_v1.get() {
            return Err(diagnostic(
                "validation",
                source,
                "config must load sliver.v1".into(),
            ));
        }
        match application.raw_get::<Value>("api_version") {
            Ok(Value::Integer(1)) => {}
            Ok(_) => {
                return Err(diagnostic(
                    "validation",
                    source,
                    "api_version must be integer 1".into(),
                ));
            }
            Err(error) => return Err(diagnostic("validation", source, error.to_string())),
        }

        let start = optional_function(&application, "start", source)?;
        let stop = optional_function(&application, "stop", source)?;
        let render = required_function(&application, "render", source)?;
        if let Some(start) = start {
            start
                .call::<()>(())
                .map_err(|error| diagnostic("start", source, error.to_string()))?;
        }

        let surface = cairo::ImageSurface::create(
            cairo::Format::ARgb32,
            sliver_core::STRIP_W as i32,
            sliver_core::STRIP_H as i32,
        )
        .map_err(|error| diagnostic("render", source, error.to_string()))?;
        let context = cairo::Context::new(&surface)
            .map_err(|error| diagnostic("render", source, error.to_string()))?;
        context.set_source_rgb(0.0, 0.0, 0.0);
        context
            .paint()
            .map_err(|error| diagnostic("render", source, error.to_string()))?;
        let canvas = lua
            .create_userdata(Canvas::new(&context))
            .map_err(|error| diagnostic("render", source, error.to_string()))?;
        let render_result = render.call::<()>(canvas.clone());
        canvas
            .borrow::<Canvas>()
            .map_err(|error| diagnostic("render", source, error.to_string()))?
            .invalidate();
        render_result.map_err(|error| diagnostic("render", source, error.to_string()))?;
        surface.flush();
        let frame = LogicalFrame::from_surface(&surface)
            .map_err(|error| diagnostic("render", source, format!("{error:#}")))?;

        Ok((
            Self {
                _lua: lua,
                stop,
                source: source.to_path_buf(),
            },
            frame,
        ))
    }

    fn stop(self) -> std::result::Result<(), String> {
        if let Some(stop) = self.stop {
            stop.call::<()>("shutdown")
                .map_err(|error| diagnostic("stop", &self.source, error.to_string()))?;
        }
        Ok(())
    }
}

fn install_v1_module(lua: &Lua) -> mlua::Result<Rc<Cell<bool>>> {
    let loaded = Rc::new(Cell::new(false));
    let loaded_by_require = loaded.clone();
    let loader = lua.create_function(move |lua, _: MultiValue| {
        loaded_by_require.set(true);
        let module = lua.create_table()?;
        module.set("api_version", 1)?;
        Ok(module)
    })?;
    let package: Table = lua.globals().get("package")?;
    let preload: Table = package.get("preload")?;
    preload.set("sliver.v1", loader)?;
    Ok(loaded)
}

fn optional_function(
    application: &Table,
    field: &str,
    source: &Path,
) -> std::result::Result<Option<Function>, String> {
    match application.raw_get::<Value>(field) {
        Ok(Value::Nil) => Ok(None),
        Ok(Value::Function(function)) => Ok(Some(function)),
        Ok(value) => Err(diagnostic(
            "validation",
            source,
            format!("{field} must be a function, got {}", value.type_name()),
        )),
        Err(error) => Err(diagnostic("validation", source, error.to_string())),
    }
}

fn required_function(
    application: &Table,
    field: &str,
    source: &Path,
) -> std::result::Result<Function, String> {
    optional_function(application, field, source)?.ok_or_else(|| {
        diagnostic(
            "validation",
            source,
            format!("missing required {field} callback"),
        )
    })
}

fn diagnostic(stage: &str, source: &Path, detail: String) -> String {
    let traceback = if detail.contains("stack traceback:") {
        String::new()
    } else {
        format!("\nstack traceback:\n\t{}:1: in {stage}", source.display())
    };
    format!("lua {} [{stage}]: {detail}{traceback}", source.display())
}
