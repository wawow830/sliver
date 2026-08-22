//! The current TOML daemon loop, separated from Touch Bar hardware.
//!
//! The daemon owns widget behavior. A `TouchBarHardware` adapter owns DRM,
//! evdev, uinput, backlight, and device-specific coordinate transforms.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::hardware::{HardwareEvent, LogicalFrame, ModifierState, TouchBarHardware};
use crate::m2_hardware::{self, M2TouchBar};

/// How long a tapped widget stays lit.
const FLASH: Duration = Duration::from_millis(220);

/// A config arriving over the socket, plus the reply line the listener owes
/// its client.
type SockMsg = (String, mpsc::Sender<Result<(), String>>);

/// Read one complete TOML document per connection and hand it to the daemon
/// loop. The listener remains outside the hardware seam.
fn spawn_socket() -> mpsc::Receiver<SockMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let path = sliver_core::socket_path();
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("socket: can't bind {}: {error}", path.display());
                return;
            }
        };
        eprintln!("socket: listening on {}", path.display());
        for connection in listener.incoming() {
            let Ok(mut stream) = connection else {
                continue;
            };
            let mut text = String::new();
            if stream.read_to_string(&mut text).is_err() {
                continue;
            }
            let (reply_tx, reply_rx) = mpsc::channel();
            if tx.send((text, reply_tx)).is_err() {
                break;
            }
            let reply = reply_rx
                .recv()
                .unwrap_or_else(|_| Err("daemon wandered off".into()));
            let message = match reply {
                Ok(()) => "ok\n".to_string(),
                Err(error) => format!("error: {error}\n"),
            };
            let _ = stream.write_all(message.as_bytes());
        }
    });
    rx
}

struct Daemon {
    cfg: sliver_core::Config,
    fn_layer: sliver_core::Config,
    pressed: Option<(usize, Instant)>,
    fn_active: bool,
    modifiers: ModifierState,
    last_tick: String,
    dirty: bool,
}

impl Daemon {
    fn new(cfg: sliver_core::Config) -> Self {
        Self {
            cfg,
            fn_layer: sliver_core::function_row_config(),
            pressed: None,
            fn_active: false,
            modifiers: ModifierState::default(),
            last_tick: String::new(),
            dirty: true,
        }
    }

    fn start<H: TouchBarHardware>(&mut self, hardware: &mut H) -> Result<()> {
        hardware.claim()?;
        self.repaint(hardware)
    }

    #[cfg(test)]
    fn step<H: TouchBarHardware>(&mut self, hardware: &mut H, timeout: Duration) -> Result<()> {
        self.poll_hardware(hardware, timeout)?;
        self.finish_step(hardware)
    }

    fn poll_hardware<H: TouchBarHardware>(
        &mut self,
        hardware: &mut H,
        timeout: Duration,
    ) -> Result<()> {
        for event in hardware.poll(timeout)? {
            match event {
                HardwareEvent::Fn { active } if active != self.fn_active => {
                    self.fn_active = active;
                    self.pressed = None;
                    self.dirty = true;
                    eprintln!("fn layer: {}", if active { "on" } else { "off" });
                }
                HardwareEvent::Fn { .. } => {}
                HardwareEvent::Modifier { modifier, active } => {
                    self.modifiers.set(modifier, active)
                }
                HardwareEvent::TouchTap { x } => self.handle_touch(hardware, x)?,
                HardwareEvent::Device { present } => eprintln!(
                    "hardware device: {}",
                    if present { "available" } else { "unavailable" }
                ),
                HardwareEvent::Visibility { visible } => eprintln!(
                    "hardware visibility: {}",
                    if visible { "visible" } else { "hidden" }
                ),
            }
        }
        Ok(())
    }

    fn finish_step<H: TouchBarHardware>(&mut self, hardware: &mut H) -> Result<()> {
        if let Some((_, since)) = self.pressed {
            if since.elapsed() > FLASH {
                self.pressed = None;
                self.dirty = true;
            }
        }
        let tick = chrono::Local::now().format("%H:%M:%S").to_string();
        if tick != self.last_tick {
            self.last_tick = tick;
            self.dirty = true;
        }
        if self.dirty {
            self.repaint(hardware)?;
        }
        Ok(())
    }

    fn handle_touch<H: TouchBarHardware>(&mut self, hardware: &mut H, x: f64) -> Result<()> {
        let active_cfg = if self.fn_active {
            &self.fn_layer
        } else {
            &self.cfg
        };
        if let Some(index) = sliver_core::hit(active_cfg, x) {
            self.pressed = Some((index, Instant::now()));
            self.dirty = true;
            if self.fn_active {
                eprintln!("fn tap at x={x:.0} -> F{}", index + 1);
                hardware.tap_function_key(index, self.modifiers)?;
            } else {
                eprintln!("tap at x={x:.0} -> widget {index}");
                if let Some(command) = self.cfg.action_at(index) {
                    let command = command.to_string();
                    eprintln!("running action: {command}");
                    std::thread::spawn(move || {
                        let _ = std::process::Command::new("sh")
                            .arg("-c")
                            .arg(&command)
                            .status();
                    });
                }
            }
        }
        Ok(())
    }

    fn apply_pending(&mut self, socket: &mpsc::Receiver<SockMsg>) {
        while let Ok((text, reply)) = socket.try_recv() {
            match sliver_core::parse_config(&text) {
                Ok(cfg) => {
                    let widget_count = cfg.widgets.len();
                    self.cfg = cfg;
                    self.pressed = None;
                    self.dirty = true;
                    eprintln!("socket: applied a config ({widget_count} widgets)");
                    let _ = reply.send(Ok(()));
                }
                Err(error) => {
                    eprintln!("socket: rejected a config: {error}");
                    let _ = reply.send(Err(format!("{error:#}")));
                }
            }
        }
    }

    fn repaint<H: TouchBarHardware>(&mut self, hardware: &mut H) -> Result<()> {
        let surface = cairo::ImageSurface::create(
            cairo::Format::ARgb32,
            sliver_core::STRIP_W as i32,
            sliver_core::STRIP_H as i32,
        )?;
        let context = cairo::Context::new(&surface)?;
        let active_cfg = if self.fn_active {
            &self.fn_layer
        } else {
            &self.cfg
        };
        sliver_core::render(active_cfg, &context, self.pressed.map(|(index, _)| index))?;
        surface.flush();
        hardware.present(&LogicalFrame::from_surface(&surface)?)?;
        self.dirty = false;
        Ok(())
    }
}

fn run_with_hardware<H: TouchBarHardware>(cfg: sliver_core::Config, mut hardware: H) -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let signal_running = running.clone();
    ctrlc::set_handler(move || signal_running.store(false, Ordering::SeqCst))?;

    let mut daemon = Daemon::new(cfg);
    let run_result = (|| -> Result<()> {
        daemon.start(&mut hardware)?;
        let socket = spawn_socket();
        eprintln!("the strip is ours — Ctrl-C to let go");
        while running.load(Ordering::SeqCst) {
            // Preserve the existing event order: physical keys, touch,
            // live config, then one repaint.
            daemon.poll_hardware(&mut hardware, Duration::from_millis(50))?;
            daemon.apply_pending(&socket);
            daemon.finish_step(&mut hardware)?;
        }
        Ok(())
    })();

    eprintln!("releasing the strip");
    let release_result = hardware.release();
    match (run_result, release_result) {
        (Err(error), Err(release_error)) => {
            eprintln!("hardware release failed after daemon error: {release_error:#}");
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("releasing Touch Bar hardware"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Stage one Lua worker frame and pass only the completed frame through the
/// broker's hardware seam. Issue #4 will keep staged workers alive for apply.
#[allow(dead_code)]
fn present_lua_once<H: TouchBarHardware>(source: &std::path::Path, hardware: &mut H) -> Result<()> {
    hardware.claim()?;
    let run_result = (|| -> Result<()> {
        let crate::lua_worker::StagedLuaWorker { worker, frame } =
            crate::lua_worker::LuaWorker::stage(source)?;
        hardware.present(&frame)?;
        worker.shutdown()
    })();
    let release_result = hardware.release();
    match (run_result, release_result) {
        (Err(error), Err(release_error)) => {
            eprintln!("hardware release failed after Lua worker error: {release_error:#}");
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("releasing Touch Bar hardware"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Claim the real M2 strip and keep the current TOML behavior running.
pub fn run(cfg: sliver_core::Config) -> Result<()> {
    run_with_hardware(cfg, M2TouchBar::new())
}

/// Paint the existing M2 calibration pattern and hold until Ctrl-C.
pub fn probe() -> Result<()> {
    m2_hardware::probe()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{
        FakeAction, FakeKey, FakeKeyEvent, FakeTouchBar, HardwareEvent, Modifier,
    };

    #[test]
    fn lua_v1_frame_crosses_the_hardware_seam() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("config.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            assert(sliver.api_version == 1)
            return {
                api_version = 1,
                render = function(_canvas)
                    return "ignored"
                end,
            }
            "#,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        assert_eq!(
            hardware.actions(),
            &[FakeAction::Grab, FakeAction::Present, FakeAction::Release]
        );
        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present its initial Lua frame")?;
        assert_eq!(frame.dimensions(), (2008, 60));
        assert_eq!(frame.rgba_at(0, 0), [0, 0, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_draws_a_filled_rectangle() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("rectangle.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(10, 5, 40, 20, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the rectangle")?;
        assert_eq!(frame.rgba_at(20, 10), [255, 0, 0, 255]);
        assert_eq!(frame.rgba_at(100, 10), [0, 0, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_shapes_text_with_pango() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("text.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:text(100, 5, "Lua", 28, 1, 1, 1, 1)
                end,
            }
            "#,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the text")?;
        let shaped_pixel_exists =
            (5..50).any(|y| (100..180).any(|x| frame.rgba_at(x, y) != [0, 0, 0, 255]));
        assert!(shaped_pixel_exists, "Pango did not draw any text pixels");
        Ok(())
    }

    #[test]
    fn lua_application_rejects_unknown_fields() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("unknown-field.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return {
                api_version = 1,
                renderr = function() end,
                render = function() end,
            }
            "#,
        )?;
        let mut hardware = FakeTouchBar::new();

        let error = present_lua_once(&source, &mut hardware)
            .expect_err("unknown application field was accepted");

        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("[validation]"), "{diagnostic}");
        assert!(diagnostic.contains("renderr"), "{diagnostic}");
        assert!(hardware.presented_frames().is_empty());
        assert_eq!(hardware.actions(), &[FakeAction::Grab, FakeAction::Release]);
        Ok(())
    }

    #[test]
    fn lua_application_rejects_non_function_callbacks() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("wrong-callback.lua");

        for callback in ["start", "stop", "visibility", "touch", "key", "render"] {
            let fields = if callback == "render" {
                "render = 42".to_string()
            } else {
                format!("{callback} = 42, render = function() end")
            };
            std::fs::write(
                &source,
                format!(
                    r#"
                    require("sliver.v1")
                    return {{
                        api_version = 1,
                        {fields}
                    }}
                    "#
                ),
            )?;
            let mut hardware = FakeTouchBar::new();

            let error = present_lua_once(&source, &mut hardware)
                .expect_err("non-function callback was accepted");

            let diagnostic = format!("{error:#}");
            assert!(diagnostic.contains("[validation]"), "{diagnostic}");
            assert!(diagnostic.contains(callback), "{diagnostic}");
            assert!(hardware.presented_frames().is_empty());
        }
        Ok(())
    }

    #[test]
    fn lua_application_requires_v1_and_render() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("invalid-contract.lua");
        let cases = [
            (
                "module was not loaded",
                "return { api_version = 1, render = function() end }",
                "load sliver.v1",
            ),
            (
                "application version was missing",
                "require('sliver.v1'); return { render = function() end }",
                "api_version",
            ),
            (
                "module and application versions differed",
                "local v1 = require('sliver.v1'); return { api_version = v1.api_version + 1, render = function() end }",
                "api_version",
            ),
            (
                "application version was not an integer",
                "require('sliver.v1'); return { api_version = 1.0, render = function() end }",
                "api_version",
            ),
            (
                "render callback was missing",
                "require('sliver.v1'); return { api_version = 1 }",
                "render",
            ),
            (
                "application was not a table",
                "require('sliver.v1'); return 1",
                "application table",
            ),
        ];

        for (failure, config, expected) in cases {
            std::fs::write(&source, config)?;
            let mut hardware = FakeTouchBar::new();

            let error = present_lua_once(&source, &mut hardware).expect_err(failure);

            let diagnostic = format!("{error:#}");
            assert!(diagnostic.contains("[validation]"), "{diagnostic}");
            assert!(diagnostic.contains(expected), "{diagnostic}");
            assert!(hardware.presented_frames().is_empty());
        }
        Ok(())
    }

    #[test]
    fn lua_diagnostics_name_source_line_traceback_and_stage() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("diagnostic.lua");
        let cases = [
            (
                "load",
                "require('sliver.v1')\nlocal broken =",
                "syntax error",
            ),
            (
                "validation",
                "require('sliver.v1'); return { api_version = 1, render = function() end, extra = true }",
                "extra",
            ),
            (
                "start",
                r#"
                require("sliver.v1")
                local function fail() error("start boom") end
                return { api_version = 1, start = fail, render = function() end }
                "#,
                "start boom",
            ),
            (
                "render",
                r#"
                require("sliver.v1")
                local function fail() error("render boom") end
                return {
                    api_version = 1,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 2008, 60, 1, 0, 0, 1)
                        fail()
                    end,
                }
                "#,
                "render boom",
            ),
        ];

        for (stage, config, expected) in cases {
            std::fs::write(&source, config)?;
            let mut hardware = FakeTouchBar::new();

            let error = present_lua_once(&source, &mut hardware)
                .expect_err("failing Lua stage was accepted");

            let diagnostic = format!("{error:#}");
            assert!(diagnostic.contains(&format!("[{stage}]")), "{diagnostic}");
            assert!(diagnostic.contains(expected), "{diagnostic}");
            assert!(
                diagnostic.contains(&format!("{}:", source.display())),
                "diagnostic omitted the source line: {diagnostic}"
            );
            assert!(diagnostic.contains("stack traceback:"), "{diagnostic}");
            assert!(hardware.presented_frames().is_empty());
            assert_eq!(hardware.actions(), &[FakeAction::Grab, FakeAction::Release]);
        }
        Ok(())
    }

    #[test]
    fn lua_callbacks_are_fixed_serial_and_ignore_returns() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("lifecycle.lua");
        let events = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                require("sliver.v1")
                local events = {events:?}
                local function record(event)
                    local file = assert(io.open(events, "a"))
                    file:write(event, "\n")
                    file:close()
                end
                local owner
                local app
                app = {{
                    api_version = 1,
                    start = function()
                        owner = coroutine.running()
                        record("start")
                        app.render = function() error("replacement render ran") end
                        app.stop = function() error("replacement stop ran") end
                        return "ignored"
                    end,
                    stop = function(reason)
                        assert(reason == "shutdown")
                        assert(coroutine.running() == owner)
                        record("stop")
                        return false
                    end,
                    visibility = function() end,
                    touch = function() end,
                    key = function() end,
                    render = function(canvas)
                        assert(coroutine.running() == owner)
                        record("render")
                        canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1)
                        return 42
                    end,
                }}
                return app
                "#,
                events = events.to_string_lossy()
            ),
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        assert_eq!(std::fs::read_to_string(events)?, "start\nrender\nstop\n");
        assert_eq!(
            hardware
                .presented_frames()
                .first()
                .context("worker did not present its lifecycle frame")?
                .rgba_at(10, 10),
            [0, 255, 0, 255]
        );
        Ok(())
    }

    #[test]
    fn lua_callbacks_cannot_yield() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("yield.lua");

        for stage in ["start", "render"] {
            let callbacks = if stage == "start" {
                "start = function() coroutine.yield() end, render = function() end"
            } else {
                "render = function() coroutine.yield() end"
            };
            std::fs::write(
                &source,
                format!(
                    r#"
                    require("sliver.v1")
                    return {{
                        api_version = 1,
                        {callbacks}
                    }}
                    "#
                ),
            )?;
            let mut hardware = FakeTouchBar::new();

            let error = present_lua_once(&source, &mut hardware)
                .expect_err("yielding callback was accepted");

            let diagnostic = format!("{error:#}");
            assert!(diagnostic.contains(&format!("[{stage}]")), "{diagnostic}");
            assert!(diagnostic.contains("yield"), "{diagnostic}");
            assert!(hardware.presented_frames().is_empty());
        }
        Ok(())
    }

    #[test]
    fn live_toml_apply_and_widget_press_cross_the_hardware_seam() -> Result<()> {
        let initial = sliver_core::parse_config(
            r##"
            background = "#ff0000"
            [[widgets]]
            type = "button"
            text = "first"
            width = 300
            "##,
        )?;
        let mut daemon = Daemon::new(initial);
        let mut hardware = FakeTouchBar::new();
        daemon.start(&mut hardware)?;

        let (socket_tx, socket_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        socket_tx.send((
            r##"
            background = "#0000ff"
            [[widgets]]
            type = "button"
            text = "updated"
            width = 300
            "##
            .to_string(),
            reply_tx,
        ))?;
        daemon.apply_pending(&socket_rx);
        daemon.finish_step(&mut hardware)?;
        assert_eq!(
            reply_rx.recv().context("daemon did not reply to apply")?,
            Ok(())
        );

        let applied = hardware
            .presented_frames()
            .last()
            .context("daemon did not present the live config")?
            .clone();
        assert_eq!(applied.rgba_at(0, 0), [0, 0, 255, 255]);

        hardware.inject(HardwareEvent::TouchTap { x: 100.0 });
        daemon.step(&mut hardware, Duration::ZERO)?;
        assert_ne!(
            &applied,
            hardware
                .presented_frames()
                .last()
                .context("daemon did not present widget press feedback")?
        );

        hardware.release()?;
        Ok(())
    }

    #[test]
    fn current_function_row_crosses_the_hardware_seam() -> Result<()> {
        let cfg = sliver_core::parse_config(
            r##"
            background = "#ff0000"
            widgets = []
            "##,
        )?;
        let mut daemon = Daemon::new(cfg);
        let mut hardware = FakeTouchBar::new();

        daemon.start(&mut hardware)?;
        let first_frame = hardware
            .presented_frames()
            .first()
            .context("daemon did not present its initial frame")?;
        assert_eq!(first_frame.dimensions(), (2008, 60));
        assert_eq!(first_frame.rgba_at(0, 0), [255, 0, 0, 255]);

        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: true,
        });
        hardware.inject(HardwareEvent::Fn { active: true });
        daemon.step(&mut hardware, Duration::ZERO)?;
        let function_row = hardware
            .presented_frames()
            .last()
            .context("daemon did not present the function row")?
            .clone();
        assert_eq!(function_row.rgba_at(0, 0), [0, 0, 0, 255]);

        hardware.inject(HardwareEvent::TouchTap { x: 260.0 });
        daemon.step(&mut hardware, Duration::ZERO)?;
        let pressed_function_row = hardware
            .presented_frames()
            .last()
            .context("daemon did not present F2 press feedback")?;
        assert_ne!(&function_row, pressed_function_row);
        let key_events: Vec<_> = hardware
            .actions()
            .iter()
            .filter_map(|action| match action {
                FakeAction::SyntheticKey(event) => Some(*event),
                _ => None,
            })
            .collect();
        assert_eq!(
            key_events,
            vec![
                FakeKeyEvent {
                    key: FakeKey::Modifier(Modifier::LeftCtrl),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Function(1),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Function(1),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Modifier(Modifier::LeftCtrl),
                    active: false,
                },
            ]
        );

        hardware.inject(HardwareEvent::Fn { active: false });
        daemon.step(&mut hardware, Duration::ZERO)?;
        assert_eq!(
            hardware
                .presented_frames()
                .last()
                .context("daemon did not restore the TOML frame")?
                .rgba_at(0, 0),
            [255, 0, 0, 255]
        );

        hardware.inject(HardwareEvent::Device { present: true });
        hardware.inject(HardwareEvent::Visibility { visible: true });
        daemon.step(&mut hardware, Duration::ZERO)?;
        hardware.set_backlight(0.5)?;
        assert!(hardware.actions().contains(&FakeAction::Backlight(0.5)));

        hardware.release()?;
        assert_eq!(hardware.actions().first(), Some(&FakeAction::Grab));
        assert_eq!(
            hardware
                .actions()
                .iter()
                .filter(|action| matches!(action, FakeAction::Release))
                .count(),
            1
        );
        Ok(())
    }
}
