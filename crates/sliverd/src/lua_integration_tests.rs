use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::hardware::{
    FakeAction, FakeTouchBar, InputState, LogicalFrame, ModifierState, TouchBarHardware,
    TouchEvent, TouchPhase,
};
use crate::supervisor::Supervisor;
use anyhow::{Context, Result};

fn present_lua_once<H: TouchBarHardware>(source: &std::path::Path, hardware: &mut H) -> Result<()> {
    hardware.claim()?;
    let run_result = (|| -> Result<()> {
        let crate::lua_worker::StagedLuaWorker { worker } =
            crate::lua_worker::LuaWorker::stage(source)?;
        let frame = worker.render_at(0.0, 0.0)?;
        hardware.present(&frame.frame)?;
        worker.shutdown(crate::lua_worker::StopReason::Shutdown)
    })();
    let release_result = hardware.release();
    match (run_result, release_result) {
        (Err(error), Err(release_error)) => {
            crate::system_log::broker_error(format!(
                "hardware release failed after Lua worker error: {release_error:#}"
            ));
            Err(error)
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("releasing Touch Bar hardware"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

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
fn lua_canvas_draws_a_rectangle_from_normalized_srgb_components() -> Result<()> {
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
    assert_eq!(
        frame.rgba_at(20, 10),
        [255, 0, 0, 255],
        "normalized numeric sRGB components must produce the requested color"
    );
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
                canvas:rectangle(0, 0, 10, 10, "#€€€€€€")
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
fn lua_canvas_rejects_unrequested_operator_and_color_aliases() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let cases = [
        (
            "source-replace operator",
            r##"canvas:operator("source-replace")"##,
        ),
        (
            "short hexadecimal color",
            r##"canvas:rectangle(0, 0, 10, 10, "#fff")"##,
        ),
        (
            "0x hexadecimal color",
            r##"canvas:rectangle(0, 0, 10, 10, "0xff0000")"##,
        ),
        (
            "three numeric color components",
            "canvas:rectangle(0, 0, 10, 10, 1, 0, 0)",
        ),
        (
            "eight-digit hexadecimal color",
            r##"canvas:rectangle(0, 0, 10, 10, "#ff000080")"##,
        ),
    ];

    for (name, operation) in cases {
        let source = directory.path().join(format!("{name}.lua"));
        std::fs::write(
            &source,
            format!(
                r##"
                require("sliver.v1")
                return {{
                    api_version = 1,
                    render = function(canvas)
                        {operation}
                    end,
                }}
                "##,
                operation = operation
            ),
        )?;
        let mut hardware = FakeTouchBar::new();

        let error = present_lua_once(&source, &mut hardware)
            .expect_err("an unrequested canvas alias was accepted");
        assert!(
            format!("{error:#}").contains("[render]"),
            "{name} failed at an unexpected stage: {error:#}"
        );
        assert!(hardware.presented_frames().is_empty());
    }
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
fn lua_canvas_source_over_composites_over_non_black_destination() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source-over.lua");
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        return {
            api_version = 1,
            render = function(canvas)
                canvas:rectangle(0, 0, 20, 20, "#204060")
                canvas:alpha(0.5)
                canvas:operator("source-over")
                canvas:rectangle(0, 0, 20, 20, "#e08040")
            end,
        }
        "##,
    )?;
    let mut hardware = FakeTouchBar::new();

    present_lua_once(&source, &mut hardware)?;

    let frame = hardware
        .presented_frames()
        .first()
        .context("source-over fixture did not present a frame")?;
    assert_eq!(
        frame.rgba_at(10, 10),
        [128, 96, 80, 255],
        "source-over must blend with the existing non-black destination"
    );
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
fn candidate_render_rejects_permanently_held_slots_with_bounded_error() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("candidate-pressure.lua");
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        return {
            api_version = 1,
            render = function(canvas)
                canvas:rectangle(0, 0, 20, 20, "#0000ff")
            end,
        }
        "##,
    )?;
    let crate::lua_worker::StagedLuaWorker { worker } =
        crate::lua_worker::LuaWorker::stage(&source)?;
    let _held = worker.hold_slots_for_test();
    let started = Instant::now();
    let error = match worker.render_at(1.0, 0.0) {
        Ok(_) => panic!("candidate render succeeded with permanently held slots"),
        Err(error) => error,
    };
    assert!(started.elapsed() < Duration::from_millis(75));
    assert!(format!("{error:#}").contains("candidate frame could not be publish"));
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    Ok(())
}

#[test]
fn dropped_lua_frame_does_not_poison_later_render_commands() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("dropped-frame.lua");
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        local renders = 0
        return {
            api_version = 1,
            render = function(canvas)
                renders = renders + 1
                if renders == 1 then
                    canvas:rectangle(0, 0, 20, 20, "#ff0000")
                else
                    canvas:rectangle(0, 0, 20, 20, "#0000ff")
                end
            end,
        }
        "##,
    )?;
    let crate::lua_worker::StagedLuaWorker { worker } =
        crate::lua_worker::LuaWorker::stage(&source)?;
    let mut held = worker.hold_slots_for_test();
    assert_eq!(held.len(), 3);
    worker.render_to_slots_at(1.0, 0.0)?;
    drop(held.pop());
    worker.render_to_slots_at(2.0, 0.0)?;

    let completed = worker
        .broker_for_test()
        .take_newest()?
        .context("later complete frame was dropped")?;
    let (frame, _) = LogicalFrame::from_completed(completed);
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&frame)?;
    assert_eq!(
        hardware
            .presented_frames()
            .last()
            .context("later frame was not presented")?
            .rgba_at(10, 10),
        [0, 0, 255, 255]
    );
    hardware.release()?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    Ok(())
}

#[test]
fn pending_lua_frame_retries_without_rerender_after_slots_free() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("pending-frame.lua");
    let log = directory.path().join("pending-renders");
    std::fs::write(
        &source,
        format!(
            r##"
            local sliver = require("sliver.v1")
            local log = {log:?}
            local function record(name)
                local file = assert(io.open(log, "a"))
                file:write(name, "\n")
                file:close()
            end
            sliver.timer.after(0.001, function() record("timer") end)
            return {{
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        record("touch")
                        sliver.redraw()
                    end
                end,
                render = function(canvas)
                    record("render")
                    canvas:rectangle(0, 0, 20, 20, "#0000ff")
                end,
            }}
            "##,
            log = log.to_string_lossy(),
        ),
    )?;
    let crate::lua_worker::StagedLuaWorker { worker } =
        crate::lua_worker::LuaWorker::stage(&source)?;
    worker.commit(0.0, InputState::default())?;
    let mut held = worker.hold_slots_for_test();
    let started = Instant::now();
    let effects = worker.drive(crate::lua_worker::DriveRequest::new(
        1.0,
        InputState::default(),
        Vec::new(),
        0.0,
        vec![TouchEvent {
            phase: TouchPhase::Down,
            id: 1,
            time: 1.0,
            x: 1.0,
            y: 1.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        }],
    ))?;
    assert!(started.elapsed() < Duration::from_millis(20));
    assert_eq!(std::fs::read_to_string(&log)?, "touch\ntimer\nrender\n");
    assert!(effects.frame.is_none());
    drop(held.pop());
    let effects = worker.drive(crate::lua_worker::DriveRequest::new(
        2.0,
        InputState::default(),
        Vec::new(),
        0.0,
        Vec::new(),
    ))?;
    let frame = effects.frame.expect("pending frame was not retried");
    assert_eq!(std::fs::read_to_string(log)?, "touch\ntimer\nrender\n");
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&frame.frame)?;
    assert_eq!(
        hardware
            .presented_frames()
            .last()
            .context("pending frame was not presented")?
            .rgba_at(10, 10),
        [0, 0, 255, 255]
    );
    hardware.release()?;
    drop(held);
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    Ok(())
}

#[test]
fn lua_raw_decoded_frames_hold_native_rate_under_broker_contention() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("raw-video.lua");
    std::fs::write(
        &source,
        r#"
        local sliver = require("sliver.v1")
        local frame = 0
        return {
            api_version = 1,
            render = function(canvas)
                frame = frame + 1
                local pixels = string.rep(string.char(frame, 0, 0, 255), 2008 * 60)
                canvas:raw_pixels(
                    pixels,
                    "rgba8",
                    2008,
                    60,
                    2008 * 4,
                    { x = 0, y = 0, width = 2008, height = 60 },
                    { x = 0, y = 0, width = 2008, height = 60 },
                    "nearest"
                )
            end,
        }
        "#,
    )?;
    let crate::lua_worker::StagedLuaWorker { worker, .. } =
        crate::lua_worker::LuaWorker::stage(&source)?;
    let broker = worker.broker_for_test();
    let started = Instant::now();
    let done = Arc::new(AtomicBool::new(false));
    let consumer_done = done.clone();
    let consumer = thread::spawn(move || -> Result<(usize, u8, f64)> {
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let mut max_latency = 0.0_f64;
        loop {
            if let Some(completed) = broker.take_newest()? {
                let (frame, timing) = LogicalFrame::from_completed(completed);
                let intended = started + Duration::from_secs_f64(timing.presentation_time);
                max_latency = max_latency.max(
                    Instant::now()
                        .saturating_duration_since(intended)
                        .as_secs_f64(),
                );
                hardware.present(&frame)?;
            } else if consumer_done.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let count = hardware.presented_frames().len();
        let last = hardware
            .presented_frames()
            .last()
            .map(|frame| frame.rgba_at(0, 0)[0])
            .unwrap_or_default();
        hardware.release()?;
        Ok((count, last, max_latency))
    });

    for frame in 0..60 {
        worker.render_to_slots_at(
            started.elapsed().as_secs_f64(),
            if frame == 0 { 0.0 } else { 1.0 / 60.0 },
        )?;
    }
    let elapsed = started.elapsed();
    done.store(true, Ordering::Release);
    let (presented, last, max_latency) = consumer.join().expect("broker thread panicked")?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;

    let fps = 60.0 / elapsed.as_secs_f64();
    eprintln!(
        "Lua raw decoded 2008x60 producer: {fps:.1} FPS, presented {presented}/60 frames, max latency {max_latency:.3}s"
    );
    assert!(
        elapsed <= Duration::from_secs(1),
        "Lua raw-pixel producer missed the 60 FPS deadline: {elapsed:?}"
    );
    assert!(presented < 60, "broker did not drop any stale frames");
    assert_eq!(last, 60, "broker did not present the newest complete frame");
    assert!(
        max_latency <= 0.15,
        "frame latency exceeded the bounded 150 ms budget: {max_latency:.3}s"
    );
    Ok(())
}

#[test]
fn lua_rejects_oversized_decoded_image_storage_before_conversion() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("oversized-image.lua");
    let too_large = 16 * 1024 * 1024 + 1;
    std::fs::write(
        &source,
        format!(
            r#"
            local sliver = require("sliver.v1")
            local image = sliver.image.new(
                string.rep("\0", {too_large}),
                "rgba8",
                1,
                1,
                {too_large}
            )
            return {{ api_version = 1, render = function() end }}
            "#,
            too_large = too_large
        ),
    )?;

    let error = match crate::lua_worker::LuaWorker::stage(&source) {
        Ok(staged) => {
            staged
                .worker
                .shutdown(crate::lua_worker::StopReason::Shutdown)?;
            panic!("oversized decoded image was accepted")
        }
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("image storage is limited"));
    Ok(())
}

#[test]
fn lua_rejects_oversized_decoded_output_before_conversion() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("oversized-output.lua");
    let too_large = 16 * 1024 * 1024 + 4;
    std::fs::write(
        &source,
        format!(
            r#"
            local sliver = require("sliver.v1")
            local image = sliver.image.new(
                string.rep("\0", {too_large}),
                "rgba8",
                1,
                {too_large} // 4,
                4
            )
            return {{ api_version = 1, render = function() end }}
            "#,
            too_large = too_large
        ),
    )?;

    let error = match crate::lua_worker::LuaWorker::stage(&source) {
        Ok(staged) => {
            staged
                .worker
                .shutdown(crate::lua_worker::StopReason::Shutdown)?;
            panic!("oversized decoded output was accepted")
        }
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("image storage is limited"));
    Ok(())
}

#[test]
fn lua_rejects_decoded_images_beyond_cairo_dimensions() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("wide-image.lua");
    let too_wide = i64::from(i32::MAX) + 1;
    std::fs::write(
        &source,
        format!(
            r#"
            local sliver = require("sliver.v1")
            local image = sliver.image.new(string.char(0, 0, 0, 0), "rgba8", {too_wide}, 1, {too_wide} * 4)
            return {{ api_version = 1, render = function() end }}
            "#,
            too_wide = too_wide
        ),
    )?;

    let error = match crate::lua_worker::LuaWorker::stage(&source) {
        Ok(staged) => {
            staged
                .worker
                .shutdown(crate::lua_worker::StopReason::Shutdown)?;
            panic!("decoded image beyond Cairo dimensions was accepted")
        }
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("Cairo dimension"));
    Ok(())
}

#[test]
fn lua_canvas_draws_reusable_decoded_images_and_borrowed_raw_pixels() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("images.lua");
    std::fs::write(
        &source,
        r#"
        local sliver = require("sliver.v1")
        local image = sliver.image.new {
            data = string.char(255, 0, 0, 255, 0, 0, 255, 255),
            format = "rgba8",
            width = 2,
            height = 1,
            stride = 8,
        }
        return {
            api_version = 1,
            render = function(canvas)
                canvas:image(
                    image,
                    { x = 0, y = 0, width = 2, height = 1 },
                    { x = 0, y = 0, width = 2, height = 1 },
                    "nearest"
                )
                canvas:image(
                    image,
                    { x = 0, y = 0, width = 2, height = 1 },
                    { x = 5, y = 0, width = 2, height = 1 },
                    "nearest"
                )
                local pixels = string.char(0, 255, 0, 255, 99, 99, 99, 99)
                canvas:raw_pixels(
                    pixels,
                    "bgra8",
                    1,
                    1,
                    8,
                    { x = 0, y = 0, width = 1, height = 1 },
                    { x = 3, y = 0, width = 1, height = 1 },
                    "nearest"
                )
                pixels = nil
                collectgarbage("collect")
            end,
        }
        "#,
    )?;
    let state_file = directory.path().join("state/sliver/config-path");
    let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
    supervisor.apply(&source)?;

    let frame = supervisor
        .hardware()
        .presented_frames()
        .last()
        .context("decoded image frame was not presented")?;
    assert_eq!(frame.rgba_at(0, 0), [255, 0, 0, 255]);
    assert_eq!(frame.rgba_at(1, 0), [0, 0, 255, 255]);
    assert_eq!(frame.rgba_at(3, 0), [0, 255, 0, 255]);
    assert_eq!(frame.rgba_at(5, 0), [255, 0, 0, 255]);
    assert_eq!(frame.rgba_at(6, 0), [0, 0, 255, 255]);
    supervisor.shutdown()?;
    Ok(())
}

#[test]
fn lua_canvas_keeps_image_premultiplication_private() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("alpha-image.lua");
    std::fs::write(
        &source,
        r#"
        local sliver = require("sliver.v1")
        local image = sliver.image.new(string.char(255, 0, 0, 128), "rgba8", 1, 1, 4)
        return {
            api_version = 1,
            render = function(canvas)
                canvas:image(
                    image,
                    { x = 0, y = 0, width = 1, height = 1 },
                    { x = 10, y = 10, width = 1, height = 1 },
                    "nearest"
                )
            end,
        }
        "#,
    )?;
    let state_file = directory.path().join("state/sliver/config-path");
    let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
    supervisor.apply(&source)?;
    let frame = supervisor
        .hardware()
        .presented_frames()
        .last()
        .context("alpha image frame was not presented")?;
    assert_eq!(frame.rgba_at(10, 10), [128, 0, 0, 255]);
    supervisor.shutdown()?;
    Ok(())
}

#[test]
fn lua_canvas_defaults_decoded_image_scaling_to_linear_filtering() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("linear-image.lua");
    std::fs::write(
        &source,
        r#"
        local sliver = require("sliver.v1")
        local image = sliver.image.new(string.char(255, 0, 0, 255, 0, 0, 255, 255), "rgba8", 2, 1, 8)
        return {
            api_version = 1,
            render = function(canvas)
                canvas:image(
                    image,
                    { x = 0, y = 0, width = 2, height = 1 },
                    { x = 0, y = 0, width = 4, height = 2 }
                )
            end,
        }
        "#,
    )?;
    let state_file = directory.path().join("state/sliver/config-path");
    let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
    supervisor.apply(&source)?;

    let pixel = supervisor
        .hardware()
        .presented_frames()
        .last()
        .context("linear image frame was not presented")?
        .rgba_at(2, 1);
    assert!(
        pixel[0] > 0 && pixel[2] > 0,
        "default filter was not linear: {pixel:?}"
    );
    supervisor.shutdown()?;
    Ok(())
}

#[test]
fn lua_canvas_clears_complete_frame_before_reusing_immutable_path() -> Result<()> {
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
    let crate::lua_worker::StagedLuaWorker { worker } =
        crate::lua_worker::LuaWorker::stage(&source)?;
    let frame = worker.render_at(0.0, 0.0)?;
    hardware.present(&frame.frame)?;
    let frame = worker.render_next()?;
    hardware.present(&frame)?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    hardware.release()?;

    let frames = hardware.presented_frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(
        frames[0].rgba_at(15, 15),
        [255, 0, 0, 255],
        "immutable path did not render the first frame"
    );
    assert_eq!(
        frames[1].rgba_at(5, 5),
        [0, 255, 0, 255],
        "the same immutable path was not reusable in the next frame"
    );
    assert_eq!(
        frames[1].rgba_at(15, 15),
        [0, 0, 0, 255],
        "complete-frame clearing retained pixels from the previous frame"
    );
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
    let crate::lua_worker::StagedLuaWorker { worker } =
        crate::lua_worker::LuaWorker::stage(&source)?;
    let frame = worker.render_at(0.0, 0.0)?;
    hardware.present(&frame.frame)?;
    let frame = worker.render_next()?;
    hardware.present(&frame)?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    hardware.release()?;

    let frames = hardware.presented_frames();
    assert!(frames[0].rgba_at(105, 5)[0] > 0);
    assert_eq!(frames[1].rgba_at(5, 5), [0, 255, 0, 255]);
    assert_eq!(
        frames[1].rgba_at(105, 5),
        [0, 0, 0, 255],
        "a fresh frame retained the previous transform, alpha, or pixels"
    );
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
fn lua_canvas_shapes_mixed_bidirectional_utf8_and_measures_logical_size() -> Result<()> {
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
                local rtl_width, rtl_height = canvas:measure_text("אבג العربية", 28)
                local mixed_width, mixed_height = canvas:measure_text("A אבג العربية 日本", 28)
                assert(latin_width > 0 and latin_height > 0)
                assert(rtl_width > 0 and rtl_height > 0)
                assert(mixed_width > latin_width and mixed_width > rtl_width)
                assert(mixed_height > 0)
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
    assert!(
        shaped_pixel_exists,
        "mixed bidirectional UTF-8 text produced no shaped glyph pixels"
    );
    Ok(())
}

#[test]
fn lua_canvas_uses_system_fallback_for_non_latin_text() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("fallback-font.lua");
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        return {
            api_version = 1,
            render = function(canvas)
                local width, height = canvas:measure_text("日本語", 28)
                assert(width > 0 and height > 0)
                canvas:text(100, 5, "日本語", 28, 1, 1, 1, 1)
            end,
        }
        "##,
    )?;
    let mut hardware = FakeTouchBar::new();

    present_lua_once(&source, &mut hardware)?;

    let frame = hardware
        .presented_frames()
        .first()
        .context("system fallback fixture did not present a frame")?;
    let fallback_pixels_exist =
        (5..50).any(|y| (100..220).any(|x| frame.rgba_at(x, y) != [0, 0, 0, 255]));
    assert!(
        fallback_pixels_exist,
        "system fallback string produced no glyph pixels"
    );
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

        let error =
            present_lua_once(&source, &mut hardware).expect_err("failing Lua stage was accepted");

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

        let error =
            present_lua_once(&source, &mut hardware).expect_err("yielding callback was accepted");

        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains(&format!("[{stage}]")), "{diagnostic}");
        assert!(diagnostic.contains("yield"), "{diagnostic}");
        assert!(hardware.presented_frames().is_empty());
    }
    Ok(())
}
