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
    fn lua_canvas_accepts_hex_srgb_colors() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("hex-color.lua");
        std::fs::write(
            &source,
            r##"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(10, 5, 40, 20, "#336699")
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the hex-colored rectangle")?;
        assert_eq!(frame.rgba_at(20, 10), [0x33, 0x66, 0x99, 0xff]);
        Ok(())
    }

    #[test]
    fn lua_canvas_rejects_non_hex_color_digits_without_panicking() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("invalid-hex.lua");
        std::fs::write(
            &source,
            r##"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 10, 10, "#€€€")
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        let error = present_lua_once(&source, &mut hardware)
            .expect_err("invalid hexadecimal color digits were accepted");
        assert!(format!("{error:#}").contains("hexadecimal"));
        assert!(hardware.presented_frames().is_empty());
        Ok(())
    }

    #[test]
    fn lua_canvas_fills_an_immutable_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("path.lua");
        std::fs::write(
            &source,
            r##"
            local sliver = require("sliver.v1")
            local path = sliver.path({
                { "move_to", 10, 5 },
                { "line_to", 50, 5 },
                { "line_to", 50, 25 },
                { "line_to", 10, 25 },
                { "close" },
            })
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:fill(path, "#00ff00")
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the reusable path")?;
        assert_eq!(frame.rgba_at(20, 10), [0, 255, 0, 255]);
        assert_eq!(frame.rgba_at(60, 10), [0, 0, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_strokes_a_reusable_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("stroke.lua");
        std::fs::write(
            &source,
            r##"
            local sliver = require("sliver.v1")
            local path = sliver.path({
                { "move_to", 10, 10 },
                { "line_to", 50, 10 },
                { "line_to", 50, 30 },
                { "line_to", 10, 30 },
                { "close" },
            })
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:stroke(path, 4, "#ff0000")
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the stroked path")?;
        assert_eq!(frame.rgba_at(30, 10), [255, 0, 0, 255]);
        assert_eq!(frame.rgba_at(30, 20), [0, 0, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_transforms_and_restores_drawing_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("transform.lua");
        std::fs::write(
            &source,
            r##"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:save()
                    canvas:translate(10, 0)
                    canvas:scale(2, 1)
                    canvas:rectangle(0, 0, 10, 10, "#ff0000")
                    canvas:restore()
                    canvas:rectangle(0, 0, 5, 5, "#0000ff")
                    canvas:save()
                    canvas:translate(70, 10)
                    canvas:rotate(math.pi / 2)
                    canvas:rectangle(0, 0, 10, 5, "#00ff00")
                    canvas:restore()
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the transformed frame")?;
        assert_eq!(frame.rgba_at(2, 2), [0, 0, 255, 255]);
        assert_eq!(frame.rgba_at(15, 5), [255, 0, 0, 255]);
        assert_eq!(frame.rgba_at(30, 5), [0, 0, 0, 255]);
        assert_eq!(frame.rgba_at(67, 15), [0, 255, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_applies_alpha_and_restores_it() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("alpha.lua");
        std::fs::write(
            &source,
            r##"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:save()
                    canvas:alpha(0.5)
                    canvas:rectangle(0, 0, 20, 20, "#ff0000")
                    canvas:restore()
                    canvas:rectangle(20, 0, 20, 20, "#0000ff")
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the alpha frame")?;
        assert_eq!(frame.rgba_at(10, 10), [128, 0, 0, 255]);
        assert_eq!(frame.rgba_at(30, 10), [0, 0, 255, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_supports_source_over_and_source_replacement() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("operators.lua");
        std::fs::write(
            &source,
            r##"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, "#ffffff")
                    canvas:alpha(0.5)
                    canvas:operator("source")
                    canvas:rectangle(0, 0, 20, 20, "#ff0000")
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the composited frame")?;
        let pixel = frame.rgba_at(10, 10);
        assert!(pixel[0] > 0, "source color was not written: {pixel:?}");
        assert_eq!(pixel[1], 0);
        assert_eq!(pixel[2], 0);
        assert!(
            pixel[3] < 200,
            "source replacement retained the destination alpha: {pixel:?}"
        );
        Ok(())
    }

    #[test]
    fn lua_canvas_clips_to_a_reusable_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("clip.lua");
        std::fs::write(
            &source,
            r##"
            local sliver = require("sliver.v1")
            local clip = sliver.path({
                { "move_to", 10, 10 },
                { "line_to", 30, 10 },
                { "line_to", 30, 30 },
                { "line_to", 10, 30 },
                { "close" },
            })
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:clip(clip)
                    canvas:rectangle(0, 0, 40, 40, "#ff0000")
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let frame = hardware
            .presented_frames()
            .first()
            .context("worker did not present the clipped frame")?;
        assert_eq!(frame.rgba_at(20, 20), [255, 0, 0, 255]);
        assert_eq!(frame.rgba_at(5, 20), [0, 0, 0, 255]);
        assert_eq!(frame.rgba_at(35, 20), [0, 0, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_reuses_paths_across_fresh_frames() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("frames.lua");
        std::fs::write(
            &source,
            r##"
            local sliver = require("sliver.v1")
            local path = sliver.path({
                { "move_to", 0, 0 },
                { "line_to", 20, 0 },
                { "line_to", 20, 20 },
                { "line_to", 0, 20 },
                { "close" },
            })
            local frame = 0
            return {
                api_version = 1,
                render = function(canvas)
                    frame = frame + 1
                    if frame == 1 then
                        canvas:fill(path, "#ff0000")
                    else
                        canvas:save()
                        canvas:scale(0.5, 0.5)
                        canvas:fill(path, "#00ff00")
                        canvas:restore()
                    end
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let crate::lua_worker::StagedLuaWorker { worker, frame } =
            crate::lua_worker::LuaWorker::stage(&source)?;
        hardware.present(&frame)?;
        let frame = worker.render_next()?;
        hardware.present(&frame)?;
        worker.shutdown()?;
        hardware.release()?;

        let frames = hardware.presented_frames();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].rgba_at(15, 15), [255, 0, 0, 255]);
        assert_eq!(frames[1].rgba_at(5, 5), [0, 255, 0, 255]);
        assert_eq!(frames[1].rgba_at(15, 15), [0, 0, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_resets_drawing_state_for_each_frame() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("frame-state.lua");
        std::fs::write(
            &source,
            r##"
            require("sliver.v1")
            local frame = 0
            return {
                api_version = 1,
                render = function(canvas)
                    frame = frame + 1
                    if frame == 1 then
                        canvas:translate(100, 0)
                        canvas:alpha(0.5)
                        canvas:operator("source")
                        canvas:rectangle(0, 0, 10, 10, "#ff0000")
                    else
                        canvas:rectangle(0, 0, 10, 10, "#00ff00")
                    end
                end,
            }
            "##,
        )?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let crate::lua_worker::StagedLuaWorker { worker, frame } =
            crate::lua_worker::LuaWorker::stage(&source)?;
        hardware.present(&frame)?;
        let frame = worker.render_next()?;
        hardware.present(&frame)?;
        worker.shutdown()?;
        hardware.release()?;

        let frames = hardware.presented_frames();
        assert!(frames[0].rgba_at(105, 5)[0] > 0);
        assert_eq!(frames[1].rgba_at(5, 5), [0, 255, 0, 255]);
        assert_eq!(frames[1].rgba_at(105, 5), [0, 0, 0, 255]);
        Ok(())
    }

    #[test]
    fn lua_canvas_is_invalid_after_render_returns() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("invalidated.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            local rendered_canvas
            return {
                api_version = 1,
                render = function(canvas)
                    rendered_canvas = canvas
                end,
                stop = function()
                    rendered_canvas:rectangle(0, 0, 1, 1, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        let mut hardware = FakeTouchBar::new();

        let error = present_lua_once(&source, &mut hardware)
            .expect_err("a frame canvas remained usable after render");
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("cannot be called after canvas invalidation"));
        assert_eq!(hardware.presented_frames().len(), 1);
        Ok(())
    }

    #[test]
    fn lua_canvas_shapes_utf8_text_and_measures_it() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("text.lua");
        std::fs::write(
            &source,
            r##"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    local latin_width, latin_height = canvas:measure_text("A", 28)
                    local mixed_width, mixed_height = canvas:measure_text("A אבג العربية 日本", 28)
                    assert(latin_width > 0 and latin_height > 0)
                    assert(mixed_width > latin_width and mixed_height > 0)
                    canvas:text(100, 5, "A אבג العربية 日本", 28, "#ffffff")
                end,
            }
            "##,
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
    fn lua_worker_loads_relative_modules_with_full_lua_54() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("imports.lua");
        std::fs::write(directory.path().join("helper.lua"), "return { red = 0.25 }")?;
        std::fs::write(
            &source,
            r#"
            local helper = require("helper")
            require("sliver.v1")
            assert(_VERSION == "Lua 5.4")
            for _, library in ipairs({
                coroutine, debug, io, math, os, package,
                string, table, utf8,
            }) do
                assert(type(library) == "table")
            end
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, helper.red, 0, 0, 1)
                end,
            }
            "#,
        )?;
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        assert_eq!(
            hardware
                .presented_frames()
                .first()
                .context("worker did not present the imported config")?
                .rgba_at(10, 10),
            [64, 0, 0, 255]
        );
        Ok(())
    }

    #[test]
    fn lua_worker_loads_c_module_against_vendored_lua() -> Result<()> {
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
                size_t numeric_sizes = sizeof(lua_Integer) * 16 + sizeof(lua_Number);
                luaL_checkversion_(state, 504.0, numeric_sizes);
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
        let mut hardware = FakeTouchBar::new();

        present_lua_once(&source, &mut hardware)?;

        let process_maps = std::fs::read_to_string("/proc/self/maps")?;
        assert!(
            !process_maps.lines().any(|line| line.contains("liblua")),
            "C module resolved against a dynamic Lua library"
        );
        assert_eq!(
            hardware
                .presented_frames()
                .first()
                .context("worker did not present the native-module config")?
                .rgba_at(10, 10),
            [0, 0, 255, 255]
        );
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
                "load",
                "require('sliver.v1'); error('top-level boom')",
                "top-level boom",
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
    fn lua_validation_diagnostic_uses_return_line_and_real_traceback() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("validation-line.lua");
        std::fs::write(
            &source,
            concat!(
                "require('sliver.v1')\n",
                "local app = {\n",
                "    api_version = 1,\n",
                "    renderr = function() end,\n",
                "    render = function() end,\n",
                "}\n",
                "return app\n",
            ),
        )?;
        let mut hardware = FakeTouchBar::new();

        let error = present_lua_once(&source, &mut hardware)
            .expect_err("unknown application field was accepted");

        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains(&format!("{}:7", source.display())),
            "validation diagnostic did not name the config return line: {diagnostic}"
        );
        assert!(diagnostic.contains("stack traceback:"), "{diagnostic}");
        assert!(diagnostic.contains("sliver validation"), "{diagnostic}");
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
