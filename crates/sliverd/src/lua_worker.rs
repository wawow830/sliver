use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use mlua::{Function, HookTriggers, Lua, MultiValue, Table, Value, VmState};

use crate::hardware::LogicalFrame;
use crate::lua_canvas::{create_path, Canvas};

pub(crate) struct StagedLuaWorker {
    pub(crate) worker: LuaWorker,
    pub(crate) frame: LogicalFrame,
}

pub(crate) struct LuaWorker {
    commands: Option<mpsc::Sender<WorkerCommand>>,
    owner: Option<thread::JoinHandle<()>>,
}

enum WorkerCommand {
    #[cfg(test)]
    Render(mpsc::SyncSender<std::result::Result<LogicalFrame, String>>),
    Shutdown(mpsc::SyncSender<std::result::Result<(), String>>),
    Abandon,
}

struct Runtime {
    _lua: Lua,
    render: Function,
    stop: Option<Function>,
    _visibility: Option<Function>,
    _touch: Option<Function>,
    _key: Option<Function>,
    source: PathBuf,
}

struct CallbackRefs {
    start: Option<Function>,
    stop: Option<Function>,
    visibility: Option<Function>,
    touch: Option<Function>,
    key: Option<Function>,
    render: Function,
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

    #[cfg(test)]
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

    run_commands(runtime, commands);
}

#[cfg(test)]
fn run_commands(runtime: Runtime, commands: mpsc::Receiver<WorkerCommand>) {
    loop {
        match commands.recv() {
            Ok(WorkerCommand::Render(reply)) => {
                let _ = reply.send(runtime.render_frame());
            }
            Ok(WorkerCommand::Shutdown(reply)) => {
                let _ = reply.send(runtime.stop());
                break;
            }
            Ok(WorkerCommand::Abandon) | Err(_) => break,
        }
    }
}

#[cfg(not(test))]
fn run_commands(runtime: Runtime, commands: mpsc::Receiver<WorkerCommand>) {
    if let Ok(WorkerCommand::Shutdown(reply)) = commands.recv() {
        let _ = reply.send(runtime.stop());
    }
}

impl Runtime {
    fn load_and_render(source: &Path) -> std::result::Result<(Self, LogicalFrame), String> {
        let bytes =
            std::fs::read(source).map_err(|error| diagnostic("load", source, error.to_string()))?;
        let lua = unsafe { Lua::unsafe_new() };
        configure_lua_path(&lua, source)
            .map_err(|error| diagnostic("load", source, error.to_string()))?;
        let loaded_v1 = install_v1_module(&lua)
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
            _touch: touch,
            _key: key,
            source: source.to_path_buf(),
        };
        let frame = runtime.render_frame()?;
        Ok((runtime, frame))
    }

    fn render_frame(&self) -> std::result::Result<LogicalFrame, String> {
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

    fn stop(self) -> std::result::Result<(), String> {
        if let Some(stop) = self.stop {
            stop.call::<()>("shutdown")
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

fn install_v1_module(lua: &Lua) -> mlua::Result<Rc<Cell<bool>>> {
    let loaded = Rc::new(Cell::new(false));
    let loaded_by_require = loaded.clone();
    let loader = lua.create_function(move |lua, _: MultiValue| {
        loaded_by_require.set(true);
        let module = lua.create_table()?;
        module.set("api_version", 1)?;
        let path = lua.create_function(create_path)?;
        module.set("path", path)?;
        Ok(module)
    })?;
    let package: Table = lua.globals().get("package")?;
    let preload: Table = package.get("preload")?;
    preload.set("sliver.v1", loader)?;
    Ok(loaded)
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
