use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::hardware::{
    FakeAction, FakeTouchBar, FrameSnapshot, InputState, LogicalFrame, ModifierState,
    TouchBarHardware, TouchEvent, TouchPhase,
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

fn assert_bottom_band_is_solid(frame: &FrameSnapshot, x_end: usize, message: &str) {
    for y in 54..60 {
        for x in 20..x_end {
            assert_eq!(
                frame.rgba_at(x, y),
                [255, 59, 129, 255],
                "{message} at ({x}, {y})"
            );
        }
    }
}

#[test]
fn diagnostic_pixel_marker_crosses_the_lua_canvas_and_fake_hardware_seam() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("marker.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        local marker = require("native_marker")
        return {
            api_version = 1,
            render = function(canvas)
                canvas:rectangle(0, 0, 2008, 60, "#112233")
                marker.draw(canvas, { run_id = "r", generation = "g", frame_id = 1 })
            end,
        }
        "##,
    )?;
    let mut hardware = FakeTouchBar::new();
    present_lua_once(&source, &mut hardware)?;
    let frame = hardware
        .presented_frames()
        .first()
        .context("no marked frame")?;
    // Worked wire vector from native-performance-marker-format.md; not a
    // checksum recomputed using the encoder's implementation.
    let mut packet = b"SLVMRK00".to_vec();
    packet.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0]);
    packet.resize(408, 0);
    packet[20] = b'r';
    packet[148] = b'g';
    packet[404..].copy_from_slice(&[0xaa, 0x94, 0xd7, 0x12]);
    for y in 0..60 {
        for x in 0..2008 {
            let expected = if (188..1820).contains(&x) && (4..12).contains(&y) {
                let bit = ((y - 4) / 2) * 816 + (x - 188) / 2;
                let level = if packet[bit / 8] & (128 >> (bit % 8)) == 0 {
                    0
                } else {
                    255
                };
                [level, level, level, 255]
            } else {
                [17, 34, 51, 255]
            };
            assert_eq!(frame.rgba_at(x, y), expected, "marker pixel ({x}, {y})");
        }
    }
    Ok(())
}

#[test]
fn diagnostic_pixel_marker_preserves_full_capacity_input_identity() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("capacity-marker.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        r#"
        require("sliver.v1")
        local marker = require("native_marker")
        return { api_version = 1, render = function(canvas)
            marker.draw(canvas, {
                run_id = string.rep("r", 128), generation = string.rep("g", 128),
                frame_id = math.maxinteger, input_id = string.rep("i", 128),
            })
        end }
        "#,
    )?;
    let mut hardware = FakeTouchBar::new();
    present_lua_once(&source, &mut hardware)?;
    let frame = hardware
        .presented_frames()
        .first()
        .context("no marked frame")?;
    let mut packet = vec![0_u8; 408];
    for bit in 0..3264 {
        let x = 188 + (bit % 816) * 2;
        let y = 4 + (bit / 816) * 2;
        let pixel = frame.rgba_at(x, y);
        assert!(pixel == [0, 0, 0, 255] || pixel == [255; 4]);
        if pixel[0] == 255 {
            packet[bit / 8] |= 128 >> (bit % 8);
        }
    }
    let mut expected = b"SLVMRK00".to_vec();
    expected.extend_from_slice(&[0x7f, 255, 255, 255, 255, 255, 255, 255, 128, 128, 128, 0]);
    expected.extend_from_slice(&[b'r'; 128]);
    expected.extend_from_slice(&[b'g'; 128]);
    expected.extend_from_slice(&[b'i'; 128]);
    // Independently evaluated capacity vector, including a non-null input.
    expected.extend_from_slice(&[0x47, 0x46, 0x54, 0xe2]);
    assert_eq!(packet, expected);
    Ok(())
}

#[test]
fn diagnostic_pixel_marker_rejects_ambiguous_identities_before_presenting() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("invalid-marker.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    for identity in [
        "nil",
        "{run_id='', generation='g', frame_id=1}",
        "{run_id=string.rep('r',129), generation='g', frame_id=1}",
        "{run_id='r', generation='a/b', frame_id=1}",
        "{run_id='é', generation='g', frame_id=1}",
        "{run_id='r', generation='g', frame_id=0}",
        "{run_id='r', generation='g', frame_id=-1}",
        "{run_id='r', generation='g', frame_id=math.maxinteger+1}",
        "{run_id='r', generation='g', frame_id=1.0}",
        "{run_id='r', generation='g', frame_id='1'}",
        "{run_id='r', generation='g', frame_id=true}",
        "{run_id='r', generation='g', frame_id=1, input_id=false}",
        "{run_id='r', generation='g', frame_id=1, input_id=''}",
        "{run_id='r', generation='g', frame_id=1, extra=true}",
        "setmetatable({run_id='r', generation='g', frame_id=1}, {})",
    ] {
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            local marker = require("native_marker")
            return { api_version = 1, render = function(canvas)
                marker.draw(canvas, IDENTITY)
            end }
            "#
            .replace("IDENTITY", identity),
        )?;
        let mut hardware = FakeTouchBar::new();
        assert!(
            present_lua_once(&source, &mut hardware).is_err(),
            "accepted {identity}"
        );
        assert!(
            hardware.presented_frames().is_empty(),
            "presented {identity}"
        );
    }
    Ok(())
}

#[test]
fn private_p1_pending_retry_preserves_allocation_host_time_in_every_mode() -> Result<()> {
    use crate::diagnostic_fixture::{Clock, Fixture, Mode, Plan, SyntheticClock, SOURCE};
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason};
    for mode in [Mode::A, Mode::B, Mode::C] {
        let fixture = Fixture::new(
            Plan::new("retry", "g", 8_000_000_000, 30, mode)?,
            Clock::Synthetic(SyntheticClock::new(1_000_000_000)),
        )?;
        let capture = (mode == Mode::C).then(|| Capture::new(64)).transpose()?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            capture.clone(),
            TimingCapture::new(8)?,
        )?
        .worker;
        worker.commit(1000.0, InputState::default())?;
        let held = worker.hold_slots_for_test();
        let before = crate::diagnostic_observer::clock_ns(false)?;
        worker.render_to_slots_at(1000.0, 0.25)?;
        let after_render = crate::diagnostic_observer::clock_ns(false)?;
        assert!(worker.broker_for_test().take_newest()?.is_none());
        std::thread::sleep(Duration::from_millis(1));
        drop(held);
        let selected = worker
            .drive(DriveRequest::without_input(1000.0, InputState::default()))?
            .frame
            .context("pending retry did not publish")?;
        let correlation = selected
            .fixture_correlation()
            .context("private identity lost")?;
        assert_eq!(
            (
                correlation.allocation.id,
                correlation.allocation.token,
                correlation.sequence
            ),
            (1, 0, 1)
        );
        assert!(
            (before..=after_render).contains(&correlation.allocation.allocated_ns),
            "retry replaced original host allocation time"
        );
        assert_eq!(selected.timing.delta, 0.25);
        assert_eq!(fixture.report().allocated, 1, "retry allocated new work");
        if mode == Mode::A {
            assert!(crate::diagnostic_observer::decode(&selected.frame).is_err());
        } else {
            assert_eq!(
                crate::diagnostic_observer::decode(&selected.frame)
                    .unwrap()
                    .frame_id(),
                1
            );
        }
        worker.shutdown(StopReason::Shutdown)?;
        if let Some(capture) = capture {
            let report = capture.close();
            let at = report
                .records
                .iter()
                .find_map(|record| match record.kind {
                    EventKind::RenderAllocated { attempt: 1 } => Some(record.at_ns),
                    _ => None,
                })
                .context("allocation observation missing")?;
            assert_eq!(correlation.allocation.allocated_ns, at);
            assert!(report.records.iter().any(|record| matches!(record.kind,
                EventKind::Published { sequence: 1, allocation: Some(value), .. }
                    if value == correlation.allocation)));
        }
    }
    Ok(())
}

#[test]
fn private_p1_pending_shutdown_retains_unpublished_allocation_disposition() -> Result<()> {
    use crate::diagnostic_fixture::{Clock, Fixture, Mode, Plan, SyntheticClock, SOURCE};
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    let fixture = Fixture::new(
        Plan::new("shutdown", "g", 8_000_000_000, 30, Mode::C)?,
        Clock::Synthetic(SyntheticClock::new(1_000_000_000)),
    )?;
    let capture = Capture::new(64)?;
    let worker = LuaWorker::stage_fixture(
        &LuaSource::embedded(SOURCE.to_vec()),
        fixture.clone(),
        Some(capture.clone()),
        TimingCapture::new(8)?,
    )?
    .worker;
    let held = worker.hold_slots_for_test();
    worker.render_to_slots_at(1000.0, 0.0)?;
    worker.shutdown(StopReason::Shutdown)?;
    drop(held);
    let report = capture.close();
    let allocated_ns = report
        .records
        .iter()
        .find_map(|record| match record.kind {
            EventKind::RenderAllocated { attempt: 1 } => Some(record.at_ns),
            _ => None,
        })
        .context("allocation observation missing")?;
    let pending: Vec<_> = report
        .records
        .iter()
        .filter_map(|record| match record.kind {
            EventKind::PendingDiscarded { allocation, .. } => allocation,
            _ => None,
        })
        .collect();
    assert_eq!(
        pending,
        vec![crate::frame_slots::FrameAllocation {
            id: 1,
            token: 0,
            allocated_ns
        }]
    );
    assert!(!report
        .records
        .iter()
        .any(|record| matches!(record.kind, EventKind::Published { .. })));
    assert!(
        !fixture.report().closed,
        "shutdown invented semantic closure"
    );
    Ok(())
}

#[test]
fn private_p1_pending_supersession_does_not_relabel_allocation_or_close_dropped_work() -> Result<()>
{
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Plan, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason};
    for mode in [Mode::A, Mode::B, Mode::C] {
        let clock = SyntheticClock::new(1_000_000_000);
        let fixture = Fixture::new(
            Plan::new("superseded", "g", 8_000_000_000, 30, mode)?,
            Clock::Synthetic(clock.clone()),
        )?;
        let capture = (mode == Mode::C).then(|| Capture::new(64)).transpose()?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            capture.clone(),
            TimingCapture::new(8)?,
        )?
        .worker;
        worker.commit(1000.0, InputState::default())?;
        let held = worker.hold_slots_for_test();
        worker.render_to_slots_at(1000.0, 0.0)?;
        worker.render_to_slots_at(1000.0, 0.0)?;
        drop(held);
        let selected = worker
            .drive(DriveRequest::without_input(1000.0, InputState::default()))?
            .frame
            .context("replacement was not published")?;
        let correlation = selected
            .fixture_correlation()
            .context("replacement identity lost")?;
        assert_eq!(
            (correlation.allocation.id, correlation.sequence),
            (2, 1),
            "publication success count was mistaken for allocation identity"
        );
        fixture.resolve_frame(
            correlation.allocation.id,
            Resolution::Presented,
            1_000_000_000,
        )?;
        clock.set(6_000_000_000);
        assert!(
            fixture.confirm_warmup_closed().is_err(),
            "successful successor silently closed unresolved dropped allocation"
        );
        worker.shutdown(StopReason::Shutdown)?;
        if let Some(capture) = capture {
            let report = capture.close();
            let pending: Vec<_> = report
                .records
                .iter()
                .filter_map(|record| match record.kind {
                    EventKind::PendingDiscarded { allocation, .. } => allocation,
                    _ => None,
                })
                .collect();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].id, 1);
            assert!(pending[0].allocated_ns <= correlation.allocation.allocated_ns);
        }
    }
    Ok(())
}

#[test]
fn private_p1_fixture_binds_exact_source_and_uses_private_marker_and_allocation_ids() -> Result<()>
{
    use crate::diagnostic_fixture::{Clock, Fixture, Mode, Plan, SyntheticClock, SOURCE};
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("fixture.lua");
    std::fs::write(&source, SOURCE)?;
    std::fs::write(
        directory.path().join("native_marker.lua"),
        "error('untrusted marker loaded')",
    )?;
    let clock = SyntheticClock::new(0);
    let fixture = Fixture::new(
        Plan::new("fixture", "g", 8_000_000_000, 30, Mode::B)?,
        Clock::Synthetic(clock.clone()),
    )?;
    let timing = TimingCapture::new(16)?;
    let worker = LuaWorker::stage_fixture(
        &LuaSource::file(source.clone()),
        fixture.clone(),
        None,
        timing.clone(),
    )?
    .worker;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    for id in [1, 2] {
        // This intended scheduler value is not the fixture's phase clock.
        let frame = worker.render_at(987654321.0, 0.0)?;
        let marker = crate::diagnostic_observer::decode(&frame.frame).unwrap();
        assert_eq!(marker.frame_id(), id);
        assert_eq!(marker.run_id(), b"fixture");
        assert_eq!(marker.input_id(), None);
        hardware.present(&frame.frame)?;
    }
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    assert_eq!(fixture.report().allocated, 2);
    assert!(fixture.report().synthetic);
    assert_eq!(timing.close().completed_spans, 2);
    assert!(LuaWorker::stage(&source)
        .err()
        .context("canonical fixture accepted without capability")?
        .to_string()
        .contains("private P1 capability required"));
    let marker = directory.path().join("executed");
    let wrong = LuaSource::embedded(
        format!(
            "local f=assert(io.open({:?}, 'w')); f:write('bad'); f:close(); return {{}}",
            marker.to_string_lossy()
        )
        .into_bytes(),
    );
    let rejected = Fixture::new(
        Plan::new("wrong", "g", 8_000_000_000, 30, Mode::A)?,
        Clock::Synthetic(clock),
    )?;
    assert!(
        LuaWorker::stage_fixture(&wrong, rejected.clone(), None, TimingCapture::new(8)?).is_err()
    );
    assert!(
        !marker.exists(),
        "wrong source executed before byte binding"
    );
    assert!(
        rejected.report().failed,
        "rejected preflight disappeared from fixture report"
    );
    std::fs::write(
        &source,
        r#"
        assert(select('#', ...) == 0, 'ordinary entry received an argument')
        local sliver = require('sliver.v1')
        assert(sliver.capture == nil and sliver.fixture == nil)
        return { api_version=1, render=function() end }
    "#,
    )?;
    LuaWorker::stage(&source)?
        .worker
        .shutdown(StopReason::Shutdown)?;
    Ok(())
}

#[test]
fn private_p1_fixture_quiesces_warmup_and_keeps_abc_scene_and_actual_ids_equal() -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Phase, Plan, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason, VisibilityReason};
    let mut scenes = Vec::new();
    let before = crate::diagnostic_observer::clock_ns(false)?;
    for mode in [Mode::A, Mode::B, Mode::C] {
        let clock = SyntheticClock::new(1_000_000_000);
        let fixture = Fixture::new(
            Plan::new("abc", "g", 8_000_000_000, 30, mode)?,
            Clock::Synthetic(clock.clone()),
        )?;
        let observer = (mode == Mode::C).then(|| Capture::new(128)).transpose()?;
        let timing = TimingCapture::new(32)?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            observer.clone(),
            timing.clone(),
        )?
        .worker;
        worker.commit(9000.0, InputState::default())?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let initial = worker.render_at(9000.0, 0.0)?;
        hardware.present(&initial.frame)?;
        fixture.resolve_frame(1, Resolution::Presented, 1_000_000_000)?;
        clock.set(6_000_000_000); // exactly five seconds since first warmup allocation
        assert!(worker
            .drive(DriveRequest::without_input(9010.0, InputState::default()))?
            .frame
            .is_none());
        assert_eq!(fixture.report().phase, Phase::Quiescing);
        // Explicit trusted coordinator attestation, not Lua or worker inference.
        fixture.confirm_warmup_closed()?;
        clock.set(7_999_999_999);
        assert!(worker
            .drive(
                DriveRequest::without_input(9011.0, InputState::default()).with_visibility(
                    true,
                    VisibilityReason::Device,
                    true
                )
            )?
            .frame
            .is_none());
        clock.set(8_000_000_000);
        let measured = worker
            .drive(
                DriveRequest::without_input(9012.0, InputState::default()).with_visibility(
                    true,
                    VisibilityReason::Device,
                    true,
                ),
            )?
            .frame
            .context("T allocation missing")?;
        hardware.present(&measured.frame)?;
        fixture.resolve_frame(2, Resolution::Presented, 8_000_000_000)?;
        if mode != Mode::A {
            assert_eq!(
                crate::diagnostic_observer::decode(&measured.frame)
                    .unwrap()
                    .frame_id(),
                2
            );
        }
        scenes.push(measured.frame.pixels().to_vec());
        clock.set(68_000_000_000);
        let stopped = worker.drive(
            DriveRequest::without_input(9100.0, InputState::default()).with_visibility(
                true,
                VisibilityReason::Device,
                true,
            ),
        )?;
        assert!(
            stopped.frame.is_none(),
            "periodic/forced render allocated at S"
        );
        assert_eq!(
            stopped.next_worker_deadline, None,
            "periodic timer was not retired"
        );
        clock.set(70_000_000_000);
        fixture.finish()?;
        worker.shutdown(StopReason::Shutdown)?;
        hardware.release()?;
        let report = fixture.report();
        assert_eq!(report.allocated, 2);
        assert_eq!(report.warmup_closed_ns, Some(6_000_000_000));
        assert_eq!(report.phase, Phase::Ended);
        assert!(!report.failed && report.closed && report.synthetic);
        assert_eq!(timing.close().completed_spans, 2);
        if let Some(observer) = observer {
            let raw = observer.close();
            let allocations: Vec<_> = raw
                .records
                .iter()
                .filter_map(|r| match r.kind {
                    EventKind::RenderAllocated { attempt } => Some((attempt, r.at_ns)),
                    _ => None,
                })
                .collect();
            let admitted: Vec<_> = report
                .observations
                .iter()
                .filter_map(|o| match o.kind {
                    crate::diagnostic_fixture::ObservationKind::Allocated { frame_id, .. } => {
                        Some((frame_id, o.host_ns))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                allocations, admitted,
                "capability and raw allocation must share actual clock sample"
            );
            let after = crate::diagnostic_observer::clock_ns(false)?;
            assert!(
                allocations
                    .iter()
                    .all(|(_, at)| (before..=after).contains(at)),
                "synthetic phase time was substituted for actual CLOCK_MONOTONIC allocation"
            );
            assert_eq!(raw.lost, 0);
        } else {
            assert!(
                report.observations.is_empty(),
                "detailed fixture recorder enabled in A/B"
            );
        }
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
    assert_eq!(
        scenes[1], scenes[2],
        "B/C must include identical marker pixels"
    );
    Ok(())
}

#[test]
fn private_p1_fixture_mutates_only_delivered_down_and_drains_confirmed_pre_stop_input() -> Result<()>
{
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, ObservationKind, Plan, Receipt, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::Capture;
    use crate::diagnostic_timing::TimingCapture;
    use crate::hardware::HardwareEvent;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason, VisibilityReason};
    let clock = SyntheticClock::new(1_000_000_000);
    let fixture = Fixture::new(
        Plan::new("causal", "g", 8_000_000_000, 30, Mode::C)?.causal_response()?,
        Clock::Synthetic(clock.clone()),
    )?;
    use crate::diagnostic_capture_transport::{Collector, MappedWriter};
    use crate::diagnostic_observer::EventKind;
    let mut collector = Collector::new(128)?;
    let capture = Capture::from_mapped(MappedWriter::receive(collector.take_worker_storage()?)?)?;
    let host_before = crate::diagnostic_observer::clock_ns(false)?;
    let worker = LuaWorker::stage_fixture(
        &LuaSource::embedded(SOURCE.to_vec()),
        fixture.clone(),
        Some(capture.clone()),
        TimingCapture::new(32)?,
    )?
    .worker;
    worker.commit(1000.0, InputState::default())?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    let first = worker.render_at(1000.0, 0.0)?;
    capture.present(&mut hardware, &first.frame)?;
    fixture.resolve_frame(1, Resolution::Presented, 1_000_000_000)?;
    clock.set(6_000_000_000);
    fixture.confirm_warmup_closed()?;
    let down = TouchEvent {
        phase: TouchPhase::Down,
        id: 42,
        time: 987654321.0,
        x: 0.0,
        y: 30.0,
        modifiers: ModifierState::default(),
        pressure: None,
        width: None,
        height: None,
    };
    clock.set(67_999_999_999);
    hardware.inject(HardwareEvent::Touch(down));
    assert_eq!(
        capture.poll(&mut hardware, Duration::ZERO)?,
        [HardwareEvent::Touch(down)]
    );
    fixture.receive(Receipt {
        sequence: 1,
        received_ns: 67_999_999_999,
        event: down,
    })?;
    clock.set(68_000_000_000);
    let response = worker.drive(
        DriveRequest::without_input(9999.0, InputState::default()).with_events(vec![down]),
    )?;
    let frame = response
        .frame
        .context("confirmed pre-S down must receive a causal-only drain frame")?;
    let marker = crate::diagnostic_observer::decode(&frame.frame).unwrap();
    assert_eq!(marker.frame_id(), 2);
    assert_eq!(marker.input_id(), Some(b"1".as_slice()));
    capture.present(&mut hardware, &frame.frame)?;
    fixture.resolve_frame(2, Resolution::Presented, 68_000_000_000)?;
    let up = TouchEvent {
        phase: TouchPhase::Up,
        ..down
    };
    clock.set(68_000_000_001);
    hardware.inject(HardwareEvent::Touch(up));
    capture.poll(&mut hardware, Duration::ZERO)?;
    fixture.receive(Receipt {
        sequence: 2,
        received_ns: 68_000_000_001,
        event: up,
    })?;
    assert!(worker
        .drive(
            DriveRequest::without_input(10000.0, InputState::default())
                .with_events(vec![up])
                .with_visibility(true, VisibilityReason::Device, true)
        )?
        .frame
        .is_none());
    clock.set(70_000_000_000);
    fixture.finish()?;
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    let report = fixture.report();
    assert!(!report.failed && report.closed && report.synthetic);
    let mutations: Vec<_> = report
        .observations
        .iter()
        .filter_map(|o| match o.kind {
            ObservationKind::TokenMutated {
                receipt_sequence,
                previous,
                token,
            } => Some((receipt_sequence, previous, token, o.decision_ns)),
            _ => None,
        })
        .collect();
    assert_eq!(mutations, [(1, 0, 1, 68_000_000_000)]);
    assert!(report.observations.iter().any(|o| matches!(
        o.kind,
        ObservationKind::ResponseConfirmed {
            receipt_sequence: 1,
            frame_id: 2,
            token: 1,
            responded_ns: 68_000_000_000,
        }
    )));
    let frames = hardware.presented_frames();
    assert_eq!(frames[0].rgba_at(0, 30), [1, 0, 113, 255]);
    assert_eq!(frames[1].rgba_at(0, 30), [2, 37, 113, 255]);
    capture.finish();
    let host_after = crate::diagnostic_observer::clock_ns(false)?;
    let raw = collector.snapshot()?;
    assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
    assert_eq!(raw.report.lost, 0);
    let exported: Vec<_> = raw
        .report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::FixtureObserved {
                synthetic,
                decision_ns,
                kind,
            } => {
                assert!(synthetic);
                assert!((host_before..=host_after).contains(&r.at_ns));
                Some((r.at_ns, decision_ns, kind))
            }
            _ => None,
        })
        .collect();
    assert!(
        !exported.is_empty(),
        "fixture observations never reached raw tag32 sink"
    );
    assert_eq!(exported.len(), report.observations.len());
    for ((host_ns, decision_ns, kind), local) in exported.iter().zip(&report.observations) {
        assert_eq!(
            (*host_ns, *decision_ns, *kind),
            (local.host_ns, local.decision_ns, local.kind)
        );
    }
    assert!(
        exported
            .iter()
            .any(|(_, ns, kind)| *ns == 6_000_000_000
                && matches!(kind, ObservationKind::WarmupClosed))
    );
    assert!(exported.iter().any(|(_, ns, kind)| *ns == 68_000_000_000
        && matches!(
            kind,
            ObservationKind::TokenMutated {
                receipt_sequence: 1,
                previous: 0,
                token: 1
            }
        )));
    assert!(exported.iter().any(|(_, _, kind)| matches!(
        kind,
        ObservationKind::ResponseConfirmed {
            receipt_sequence: 1,
            frame_id: 2,
            token: 1,
            responded_ns: 68_000_000_000
        }
    )));
    assert!(exported
        .iter()
        .any(|(_, ns, kind)| *ns == 70_000_000_000 && matches!(kind, ObservationKind::Closed)));
    Ok(())
}

#[test]
fn private_p1_fixture_never_acknowledges_a_closure_lost_by_raw_storage() -> Result<()> {
    use crate::diagnostic_capture_transport::{Collector, MappedWriter};
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Plan, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::Capture;
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    // Normal staging, render/selection and resolution produce seven records;
    // Quiescing and WarmupClosed add two; Ended and Closed add another two.
    for capacity in [8, 10] {
        let clock = SyntheticClock::new(1_000_000_000);
        let fixture = Fixture::new(
            Plan::new("terminal-loss", "g", 8_000_000_000, 30, Mode::C)?,
            Clock::Synthetic(clock.clone()),
        )?;
        let mut collector = Collector::new(capacity)?;
        let capture =
            Capture::from_mapped(MappedWriter::receive(collector.take_worker_storage()?)?)?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            Some(capture.clone()),
            TimingCapture::new(8)?,
        )?
        .worker;
        worker.render_at(0.0, 0.0)?;
        fixture.resolve_frame(1, Resolution::Discarded, 1_000_000_000)?;
        assert_eq!(collector.snapshot()?.report.attempted_records, 7);
        clock.set(6_000_000_000);
        let warmup = fixture.confirm_warmup_closed();
        if capacity == 8 {
            assert!(warmup.is_err(), "lost WarmupClosed was acknowledged");
        } else {
            warmup?;
            clock.set(70_000_000_000);
            assert!(fixture.finish().is_err(), "lost Closed was acknowledged");
        }
        let report = fixture.report();
        assert!(report.failed && !report.closed);
        assert_eq!(report.lost, 0, "raw overflow is not local observation loss");
        worker.shutdown(StopReason::Shutdown)?;
        capture.finish();
        let raw = collector.snapshot()?;
        assert!(raw.report.lost > 0 && raw.report.failed_operations > 0);
        assert_eq!(raw.report.records.len(), capacity);
    }
    Ok(())
}

#[test]
fn private_p1_fixture_fails_the_operation_that_exhausts_local_observations() -> Result<()> {
    use crate::diagnostic_capture_transport::{Collector, MappedWriter};
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, ObservationKind, Plan, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    let clock = SyntheticClock::new(1_000_000_000);
    let fixture = Fixture::new(
        Plan::new("local-bound", "g", 8_000_000_000, 30, Mode::C)?,
        Clock::Synthetic(clock.clone()),
    )?;
    let mut collector = Collector::new(32768)?;
    let capture = Capture::from_mapped(MappedWriter::receive(collector.take_worker_storage()?)?)?;
    let worker = LuaWorker::stage_fixture(
        &LuaSource::embedded(SOURCE.to_vec()),
        fixture.clone(),
        Some(capture.clone()),
        TimingCapture::new(8)?,
    )?
    .worker;
    worker.render_at(0.0, 0.0)?;
    fixture.resolve_frame(1, Resolution::Discarded, 1_000_000_000)?;
    clock.set(6_000_000_000);
    fixture.confirm_warmup_closed()?;
    assert_eq!(fixture.report().observations.len(), 4);
    for _ in 0..16380 {
        worker.render_to_slots_at(0.0, 0.0)?;
    }
    let full = fixture.report();
    assert_eq!(full.observations.len(), 16384);
    assert_eq!(full.lost, 0);
    assert!(!full.failed);
    assert!(
        worker.render_to_slots_at(0.0, 0.0).is_err(),
        "overflowing fixture observation returned success"
    );
    let failed = fixture.report();
    assert!(failed.failed && !failed.closed);
    assert_eq!(failed.observations.len(), 16384);
    assert!(failed.lost > 0);
    assert_eq!(failed.allocated, 1);
    worker.shutdown(StopReason::Shutdown)?;
    capture.finish();
    let raw = collector.snapshot()?;
    assert!(raw.metadata_consistent && raw.report.closed);
    assert_eq!(
        raw.report.lost, 0,
        "local loss must not be confused with raw storage loss"
    );
    assert!(raw.report.failed_operations > 0);
    assert!(raw.report.records.iter().any(|r| matches!(
        r.kind,
        EventKind::FixtureObserved {
            kind: ObservationKind::Failed,
            ..
        }
    )));
    Ok(())
}

#[test]
fn private_p1_fixture_bounds_unresolved_frames_and_queued_receipts_in_all_modes() -> Result<()> {
    use crate::diagnostic_fixture::{Clock, Fixture, Mode, Plan, Receipt, SyntheticClock, SOURCE};
    use crate::diagnostic_observer::Capture;
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    for mode in [Mode::A, Mode::B, Mode::C] {
        for queued_input in [false, true] {
            let fixture = Fixture::new(
                Plan::new("bounds", "g", 8_000_000_000, 30, mode)?,
                Clock::Synthetic(SyntheticClock::new(1_000_000_000)),
            )?;
            let capture = (mode == Mode::C).then(|| Capture::new(1024)).transpose()?;
            let worker = LuaWorker::stage_fixture(
                &LuaSource::embedded(SOURCE.to_vec()),
                fixture.clone(),
                capture.clone(),
                TimingCapture::new(128)?,
            )?
            .worker;
            let receipt = |sequence| Receipt {
                sequence,
                received_ns: 1_000_000_000,
                event: TouchEvent {
                    phase: if sequence == 1 {
                        TouchPhase::Down
                    } else {
                        TouchPhase::Move
                    },
                    id: 1,
                    time: 0.0,
                    x: 0.0,
                    y: 30.0,
                    modifiers: ModifierState::default(),
                    pressure: None,
                    width: None,
                    height: None,
                },
            };
            for sequence in 1..=64 {
                if queued_input {
                    fixture.receive(receipt(sequence))?;
                } else {
                    worker.render_at(0.0, 0.0)?;
                }
            }
            assert!(!fixture.report().failed);
            let overflow = if queued_input {
                fixture.receive(receipt(65))
            } else {
                worker.render_at(0.0, 0.0).map(|_| ())
            };
            assert!(overflow.is_err());
            let report = fixture.report();
            assert!(report.failed);
            assert_eq!(report.allocated, if queued_input { 0 } else { 64 });
            if mode != Mode::C {
                assert!(report.observations.is_empty());
            }
            assert!(worker.render_at(0.0, 0.0).is_err());
            worker.shutdown(StopReason::Shutdown)?;
            if let Some(capture) = capture {
                let raw = capture.close();
                assert_eq!(raw.lost, 0);
                assert!(raw.failed_operations > 0);
            }
        }
    }
    Ok(())
}

#[test]
fn private_p1_fixture_retains_runtime_failure_and_rejects_later_work() -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, ObservationKind, Plan, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::TimingCapture;
    use crate::hardware::{InputTransition, ObservedKey};
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason, VisibilityReason};
    for case in ["invalid-render", "uncommitted-drive", "key", "hidden"] {
        let fixture = Fixture::new(
            Plan::new("runtime-failure", "g", 8_000_000_000, 30, Mode::C)?,
            Clock::Synthetic(SyntheticClock::new(1_000_000_000)),
        )?;
        let capture = Capture::new(64)?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            Some(capture.clone()),
            TimingCapture::new(8)?,
        )?
        .worker;
        if case != "uncommitted-drive" {
            worker.commit(0.0, InputState::default())?;
        }
        let result = match case {
            "invalid-render" => worker.render_at(f64::NAN, 0.0).map(|_| ()),
            "uncommitted-drive" => worker
                .drive(DriveRequest::without_input(0.0, InputState::default()))
                .map(|_| ()),
            "key" => worker
                .drive(DriveRequest::new(
                    0.0,
                    InputState::default(),
                    vec![InputTransition {
                        key: ObservedKey::Fn,
                        active: true,
                        state: InputState {
                            fn_active: true,
                            ..InputState::default()
                        },
                    }],
                    0.0,
                    vec![],
                ))
                .map(|_| ()),
            "hidden" => worker
                .drive(
                    DriveRequest::without_input(0.0, InputState::default()).with_visibility(
                        false,
                        VisibilityReason::Device,
                        false,
                    ),
                )
                .map(|_| ()),
            _ => unreachable!(),
        };
        assert!(
            result.is_err(),
            "unexpected {case} accepted by canonical fixture"
        );
        assert!(
            fixture.report().failed,
            "runtime {case} error escaped fixture failure accounting"
        );
        assert_eq!(fixture.report().allocated, 0);
        assert!(
            worker.render_at(0.0, 0.0).is_err(),
            "failed fixture resumed rendering"
        );
        assert_eq!(fixture.report().allocated, 0);
        worker.shutdown(StopReason::Shutdown)?;
        assert!(fixture
            .report()
            .observations
            .iter()
            .any(|o| matches!(o.kind, ObservationKind::Failed)));
        let raw = capture.close();
        assert_eq!(raw.lost, 0);
        assert!(raw.failed_operations > 0);
        assert!(raw.records.iter().any(|r| matches!(
            r.kind,
            EventKind::FixtureObserved {
                synthetic: true,
                kind: ObservationKind::Failed,
                ..
            }
        )));
    }
    Ok(())
}

#[test]
fn private_p1_fixture_rejects_raw_loss_or_premature_storage_closure() -> Result<()> {
    use crate::diagnostic_capture_transport::{Collector, MappedWriter};
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Plan, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::Capture;
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    for capacity in [1, 128] {
        let clock = SyntheticClock::new(1_000_000_000);
        let fixture = Fixture::new(
            Plan::new("raw-health", "g", 8_000_000_000, 30, Mode::C)?,
            Clock::Synthetic(clock.clone()),
        )?;
        let mut collector = Collector::new(capacity)?;
        let capture =
            Capture::from_mapped(MappedWriter::receive(collector.take_worker_storage()?)?)?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            Some(capture.clone()),
            TimingCapture::new(8)?,
        )?
        .worker;
        worker.commit(0.0, InputState::default())?;
        let rendered = worker.render_at(0.0, 0.0);
        if capacity == 1 {
            assert!(
                rendered.is_err(),
                "known raw loss must reject further allocation"
            );
            assert!(collector.snapshot()?.report.lost > 0);
        } else {
            rendered?;
            fixture.resolve_frame(1, Resolution::Discarded, 1_000_000_000)?;
            assert_eq!(collector.snapshot()?.report.lost, 0);
            capture.finish();
        }
        clock.set(6_000_000_000);
        assert!(
            fixture.confirm_warmup_closed().is_err(),
            "unhealthy raw storage was accepted"
        );
        assert!(fixture.report().failed);
        clock.set(70_000_000_000);
        assert!(fixture.finish().is_err());
        worker.shutdown(StopReason::Shutdown)?;
        capture.finish();
        let raw = collector.snapshot()?;
        assert!(raw.metadata_consistent && raw.report.closed);
        assert!(raw.report.failed_operations > 0);
        assert!(raw.report.lost > 0);
        assert!(raw.report.records.len() <= capacity);
        if capacity == 1 {
            assert_eq!(raw.report.records.len(), 1);
        }
    }
    Ok(())
}

#[test]
fn private_p1_fixture_rejects_early_unresolved_and_contradicted_warmup_closure() -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Plan, Receipt, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    for case in ["early", "unresolved", "at-T", "late-receipt"] {
        let clock = SyntheticClock::new(1_000_000_000);
        let fixture = Fixture::new(
            Plan::new("warmup", "g", 8_000_000_000, 30, Mode::B)?,
            Clock::Synthetic(clock.clone()),
        )?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            None,
            TimingCapture::new(8)?,
        )?
        .worker;
        worker.commit(0.0, InputState::default())?;
        let initial = worker.render_at(0.0, 0.0)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        hardware.present(&initial.frame)?;
        if case != "unresolved" {
            fixture.resolve_frame(1, Resolution::Presented, 1_000_000_000)?;
        }
        clock.set(match case {
            "early" => 5_999_999_999,
            "at-T" => 8_000_000_000,
            _ => 6_000_000_000,
        });
        if case == "late-receipt" {
            fixture.confirm_warmup_closed()?;
            clock.set(8_000_000_000);
            let down = TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 123.0,
                x: 0.0,
                y: 0.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            };
            assert!(
                fixture
                    .receive(Receipt {
                        sequence: 1,
                        received_ns: 7_000_000_000,
                        event: down
                    })
                    .is_err(),
                "late pre-T receipt contradicts the coordinator's quiescence assertion"
            );
        } else {
            assert!(
                fixture.confirm_warmup_closed().is_err(),
                "accepted {case} closure"
            );
        }
        assert!(
            fixture.report().failed,
            "failed preflight must remain failed: {case}"
        );
        worker.shutdown(StopReason::Shutdown)?;
        hardware.release()?;
    }
    Ok(())
}

#[test]
fn private_p1_fixture_suppresses_request_crossing_stop_before_allocation() -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, ObservationKind, Plan, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason};
    let clock = SyntheticClock::new(1_000_000_000);
    let fixture = Fixture::new(
        Plan::new("cross-stop", "g", 8_000_000_000, 60, Mode::C)?,
        Clock::Synthetic(clock.clone()),
    )?;
    let capture = Capture::new(64)?;
    let worker = LuaWorker::stage_fixture(
        &LuaSource::embedded(SOURCE.to_vec()),
        fixture.clone(),
        Some(capture.clone()),
        TimingCapture::new(16)?,
    )?
    .worker;
    worker.commit(0.0, InputState::default())?;
    let frame = worker.render_at(0.0, 0.0)?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&frame.frame)?;
    fixture.resolve_frame(1, Resolution::Presented, 1_000_000_000)?;
    clock.set(6_000_000_000);
    fixture.confirm_warmup_closed()?;
    // Fake clock crosses S after periodic admission and the Lua phase check,
    // before allocation. All subsequent decisions stay at S. No native claim.
    clock.script(&[67_999_999_999, 67_999_999_999, 68_000_000_000]);
    let effects = worker.drive(DriveRequest::without_input(1000.0, InputState::default()))?;
    assert!(effects.frame.is_none());
    assert_eq!(effects.next_worker_deadline, None);
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    let raw = capture.close();
    assert_eq!(
        raw.records
            .iter()
            .filter(|r| matches!(r.kind, EventKind::RedrawRequested { .. }))
            .count(),
        1,
        "test must really request a redraw before S, not merely skip a late callback"
    );
    assert_eq!(
        raw.records
            .iter()
            .filter(|r| matches!(r.kind, EventKind::TimerDispatched { .. }))
            .count(),
        1
    );
    assert_eq!(fixture.report().allocated, 1);
    assert!(fixture
        .report()
        .observations
        .iter()
        .any(|o| o.decision_ns == 68_000_000_000 && matches!(o.kind, ObservationKind::Suppressed)));
    assert_eq!(hardware.presented_frames().len(), 1);
    Ok(())
}

#[test]
fn private_p1_fixture_rejects_missing_or_invalid_input_context_and_requires_end_closure(
) -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Plan, Receipt, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason};
    for case in [
        "missing-receipt",
        "post-S-down",
        "duplicate-contact",
        "unanswered",
        "unreleased",
        "release-at-E",
        "release-after-E",
        "late-response",
    ] {
        let clock = SyntheticClock::new(1_000_000_000);
        let fixture = Fixture::new(
            Plan::new("input-closure", "g", 8_000_000_000, 30, Mode::B)?.causal_response()?,
            Clock::Synthetic(clock.clone()),
        )?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            None,
            TimingCapture::new(16)?,
        )?
        .worker;
        worker.commit(0.0, InputState::default())?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        hardware.present(&worker.render_at(0.0, 0.0)?.frame)?;
        fixture.resolve_frame(1, Resolution::Presented, 1_000_000_000)?;
        clock.set(6_000_000_000);
        fixture.confirm_warmup_closed()?;
        let down = TouchEvent {
            phase: TouchPhase::Down,
            id: 1,
            time: 0.0,
            x: 2007.0,
            y: 59.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        clock.set(8_000_000_000);
        if case == "missing-receipt" {
            assert!(worker
                .drive(
                    DriveRequest::without_input(1000.0, InputState::default())
                        .with_events(vec![down])
                )
                .is_err());
        } else if case == "post-S-down" {
            clock.set(68_000_000_000);
            assert!(fixture
                .receive(Receipt {
                    sequence: 1,
                    received_ns: 68_000_000_000,
                    event: down
                })
                .is_err());
        } else {
            fixture.receive(Receipt {
                sequence: 1,
                received_ns: 8_000_000_000,
                event: down,
            })?;
            let response = worker
                .drive(
                    DriveRequest::without_input(1000.0, InputState::default())
                        .with_events(vec![down]),
                )?
                .frame
                .context("input response missing")?;
            if case != "late-response" && case != "unanswered" {
                hardware.present(&response.frame)?;
                fixture.resolve_frame(2, Resolution::Presented, 8_000_000_000)?;
            }
            let up = TouchEvent {
                phase: TouchPhase::Up,
                ..down
            };
            match case {
                "duplicate-contact" => {
                    assert!(fixture
                        .receive(Receipt {
                            sequence: 2,
                            received_ns: 8_000_000_000,
                            event: down,
                        })
                        .is_err());
                }
                "unanswered" => {
                    fixture.receive(Receipt {
                        sequence: 2,
                        received_ns: 8_000_000_000,
                        event: up,
                    })?;
                    worker.drive(
                        DriveRequest::without_input(1000.0, InputState::default())
                            .with_events(vec![up]),
                    )?;
                    assert!(fixture
                        .receive(Receipt {
                            sequence: 3,
                            received_ns: 8_000_000_000,
                            event: down,
                        })
                        .is_err());
                }
                "unreleased" => {
                    clock.set(70_000_000_000);
                    assert!(
                        fixture.finish().is_err(),
                        "unreleased contact disappeared at E"
                    );
                }
                "release-at-E" | "release-after-E" => {
                    clock.set(70_000_000_000);
                    fixture.receive(Receipt {
                        sequence: 2,
                        received_ns: 70_000_000_000,
                        event: up,
                    })?;
                    if case == "release-after-E" {
                        clock.set(70_000_000_001);
                    }
                    let delivery = worker.drive(
                        DriveRequest::without_input(2000.0, InputState::default())
                            .with_events(vec![up]),
                    );
                    if case == "release-after-E" {
                        assert!(delivery.is_err(), "callback delivery after E was accepted");
                        assert!(fixture.finish().is_err());
                    } else {
                        assert!(delivery?.frame.is_none());
                        fixture.finish()?;
                    }
                }
                "late-response" => {
                    clock.set(70_000_000_001);
                    hardware.present(&response.frame)?;
                    assert!(
                        fixture
                            .resolve_frame(2, Resolution::Presented, 70_000_000_001)
                            .is_err(),
                        "late coordinator response was backdated"
                    );
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(
            fixture.report().failed,
            case != "release-at-E",
            "case {case}"
        );
        worker.shutdown(StopReason::Shutdown)?;
        hardware.release()?;
    }
    Ok(())
}

#[test]
fn private_p1_fixture_rejects_regressed_coordinator_clock_without_resolving_work() -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Plan, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    let clock = SyntheticClock::new(1_000_000_000);
    let fixture = Fixture::new(
        Plan::new("clock", "g", 8_000_000_000, 30, Mode::B)?,
        Clock::Synthetic(clock.clone()),
    )?;
    let worker = LuaWorker::stage_fixture(
        &LuaSource::embedded(SOURCE.to_vec()),
        fixture.clone(),
        None,
        TimingCapture::new(8)?,
    )?
    .worker;
    let frame = worker.render_at(0.0, 0.0)?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&frame.frame)?;
    clock.set(999_999_999);
    assert!(
        fixture
            .resolve_frame(1, Resolution::Presented, 1_000_000_000)
            .is_err(),
        "response timestamp preceded allocation"
    );
    clock.set(6_000_000_000);
    assert!(
        fixture.confirm_warmup_closed().is_err(),
        "clock regression disappeared after time advanced again"
    );
    assert!(fixture.report().failed);
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    Ok(())
}

#[test]
fn private_p1_fixture_rejects_receipt_overlap_before_callback_or_response_delivery() -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, Plan, Receipt, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason};
    for delivered in [false, true] {
        let clock = SyntheticClock::new(1_000_000_000);
        let fixture = Fixture::new(
            Plan::new("receipt-overlap", "g", 8_000_000_000, 30, Mode::B)?.causal_response()?,
            Clock::Synthetic(clock.clone()),
        )?;
        let worker = LuaWorker::stage_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            fixture.clone(),
            None,
            TimingCapture::new(16)?,
        )?
        .worker;
        worker.commit(0.0, InputState::default())?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        hardware.present(&worker.render_at(0.0, 0.0)?.frame)?;
        fixture.resolve_frame(1, Resolution::Presented, 1_000_000_000)?;
        clock.set(6_000_000_000);
        fixture.confirm_warmup_closed()?;
        let down = TouchEvent {
            phase: TouchPhase::Down,
            id: 1,
            time: 0.0,
            x: 0.0,
            y: 0.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        let up = TouchEvent {
            phase: TouchPhase::Up,
            ..down
        };
        clock.set(8_000_000_000);
        fixture.receive(Receipt {
            sequence: 1,
            received_ns: 8_000_000_000,
            event: down,
        })?;
        let response = if delivered {
            worker
                .drive(
                    DriveRequest::without_input(1000.0, InputState::default())
                        .with_events(vec![down]),
                )?
                .frame
        } else {
            None
        };
        fixture.receive(Receipt {
            sequence: 2,
            received_ns: 8_000_000_000,
            event: up,
        })?;
        assert!(
            fixture
                .receive(Receipt {
                    sequence: 3,
                    received_ns: 8_000_000_000,
                    event: down
                })
                .is_err(),
            "new receipt while previous down unanswered was accepted (delivered={delivered})"
        );
        if let Some(response) = response {
            hardware.present(&response.frame)?;
            fixture.resolve_frame(2, Resolution::Presented, 8_000_000_000)?;
            assert!(
                fixture.report().failed,
                "later response confirmation erased known receipt overlap"
            );
        }
        worker.shutdown(StopReason::Shutdown)?;
        hardware.release()?;
    }
    Ok(())
}

#[test]
fn private_p1_fixture_preserves_supplied_response_endpoint_separate_from_confirmation_time(
) -> Result<()> {
    use crate::diagnostic_fixture::{
        Clock, Fixture, Mode, ObservationKind, Plan, Receipt, Resolution, SyntheticClock, SOURCE,
    };
    use crate::diagnostic_observer::Capture;
    use crate::diagnostic_timing::TimingCapture;
    use crate::lua_worker::{DriveRequest, LuaSource, LuaWorker, StopReason};
    let clock = SyntheticClock::new(1_000_000_000);
    let fixture = Fixture::new(
        Plan::new("response-time", "g", 8_000_000_000, 30, Mode::C)?.causal_response()?,
        Clock::Synthetic(clock.clone()),
    )?;
    let worker = LuaWorker::stage_fixture(
        &LuaSource::embedded(SOURCE.to_vec()),
        fixture.clone(),
        Some(Capture::new(128)?),
        TimingCapture::new(16)?,
    )?
    .worker;
    worker.commit(0.0, InputState::default())?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&worker.render_at(0.0, 0.0)?.frame)?;
    fixture.resolve_frame(1, Resolution::Presented, 1_000_000_000)?;
    clock.set(6_000_000_000);
    fixture.confirm_warmup_closed()?;
    let down = TouchEvent {
        phase: TouchPhase::Down,
        id: 1,
        time: 987654321.0,
        x: 0.0,
        y: 0.0,
        modifiers: ModifierState::default(),
        pressure: None,
        width: None,
        height: None,
    };
    clock.set(8_000_000_000);
    fixture.receive(Receipt {
        sequence: 1,
        received_ns: 8_000_000_000,
        event: down,
    })?;
    let response = worker
        .drive(DriveRequest::without_input(1000.0, InputState::default()).with_events(vec![down]))?
        .frame
        .context("response missing")?;
    clock.set(8_010_000_000);
    hardware.present(&response.frame)?;
    clock.set(8_020_000_000); // the coordinator delivers its confirmation later
    fixture.resolve_frame(2, Resolution::Presented, 8_010_000_000)?;
    let report = fixture.report();
    assert!(report
        .observations
        .iter()
        .any(|o| o.decision_ns == 8_020_000_000
            && matches!(
                o.kind,
                ObservationKind::ResponseConfirmed {
                    receipt_sequence: 1,
                    frame_id: 2,
                    token: 1,
                    responded_ns: 8_010_000_000
                }
            )));
    // Calls must preserve source event order; a later-reported earlier receipt
    // cannot be used to erase an overlap or invent a negative response interval.
    assert!(fixture
        .receive(Receipt {
            sequence: 2,
            received_ns: 8_005_000_000,
            event: TouchEvent {
                phase: TouchPhase::Up,
                ..down
            }
        })
        .is_err());
    assert!(fixture.report().failed);
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    Ok(())
}

#[test]
fn private_diagnostic_capture_records_timer_registration_activation_and_cancellation() -> Result<()>
{
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::lua_worker::{DriveRequest, LuaWorker, StopReason};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("timers.lua");
    std::fs::write(
        &source,
        r##"
        local sliver = require("sliver.v1")
        local cancelled = sliver.timer.after(2.5, function() error("cancelled") end)
        cancelled:cancel()
        cancelled:cancel()
        sliver.timer.every(1 / 30, function() end)
        return { api_version=1,
            start=function() sliver.timer.after(4, function() end) end,
            touch=function()
                sliver.timer.after(0.125, function() end)
                sliver.redraw()
            end,
            render=function(canvas) canvas:rectangle(0, 0, 8, 8, "#ff0000") end }
    "##,
    )?;
    let capture = Capture::new(64)?;
    let before = capture.monotonic_ns()?;
    let worker = LuaWorker::stage_observed(&source, capture.clone())?.worker;
    // Deliberately unrelated intended scheduler time must not become a host timestamp.
    worker.commit(987654321.0, InputState::default())?;
    let effects = worker.drive(
        DriveRequest::without_input(987654321.0, InputState::default()).with_events(vec![
            TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 88.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            },
        ]),
    )?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&effects.frame.context("touch redraw missing")?.frame)?;
    hardware.release()?;
    worker.shutdown(StopReason::Shutdown)?;
    let after = capture.monotonic_ns()?;
    let report = capture.close();
    let registered: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::TimerRegistered {
                timer_id,
                delay_seconds,
                interval_seconds,
                scheduler_now_seconds,
            } => Some((
                timer_id,
                delay_seconds,
                interval_seconds,
                scheduler_now_seconds,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(registered.len(), 4);
    assert_eq!(
        registered[..3],
        [
            (1, 2.5, None, None),
            (2, 1.0 / 30.0, Some(1.0 / 30.0), None),
            (3, 4.0, None, None)
        ]
    );
    assert_eq!(
        (registered[3].0, registered[3].1, registered[3].2),
        (4, 0.125, None)
    );
    assert!(registered[3]
        .3
        .is_some_and(|now| (987654321.0..987654322.0).contains(&now)));
    let activated: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::TimerActivated {
                timer_id,
                scheduler_now_seconds,
                deadline_seconds,
            } => Some((timer_id, scheduler_now_seconds, deadline_seconds)),
            _ => None,
        })
        .collect();
    assert_eq!(activated.len(), 3);
    assert_eq!(
        activated[..2],
        [
            (2, 987654321.0, 987654321.0 + 1.0 / 30.0),
            (3, 987654321.0, 987654325.0)
        ]
    );
    assert_eq!(
        activated[2],
        (
            4,
            registered[3].3.unwrap(),
            registered[3].3.unwrap() + 0.125
        )
    );
    let cancellations: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::TimerCancelled { timer_id, removed } => Some((timer_id, removed)),
            _ => None,
        })
        .collect();
    assert_eq!(cancellations, [(1, true), (1, false)]);
    assert!(report
        .records
        .iter()
        .all(|r| (before..=after).contains(&r.at_ns)));
    assert_eq!(
        (report.lost, report.clock_failures, report.failed_operations),
        (0, 0, 0)
    );
    assert_eq!(
        hardware.presented_frames()[0].rgba_at(1, 1),
        [255, 0, 0, 255]
    );
    Ok(())
}

#[test]
fn private_diagnostic_capture_records_timer_dispatch_skips_and_redraw_coalescing() -> Result<()> {
    use crate::diagnostic_observer::{Capture, EventKind, TimerDisposition};
    use crate::lua_worker::{DriveRequest, LuaWorker, StopReason};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("dispatch.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        r#"
        local sliver = require("sliver.v1")
        local marker = require("native_marker")
        local ticks, frames = 0, 0
        local function tick()
            ticks = ticks + 1
            sliver.redraw()
            sliver.redraw()
        end
        sliver.timer.every(10, tick)
        sliver.timer.after(12, tick)
        return { api_version=1, render=function(canvas)
            frames = frames + 1
            marker.draw(canvas, {run_id="timers", generation="g", frame_id=frames,
                input_id=tostring(ticks)})
        end }
    "#,
    )?;
    let capture = Capture::new(128)?;
    let before = capture.monotonic_ns()?;
    let worker = LuaWorker::stage_observed(&source, capture.clone())?.worker;
    worker.commit(1000.0, InputState::default())?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    let first = worker.drive(DriveRequest::without_input(1045.0, InputState::default()))?;
    assert_eq!(first.next_worker_deadline, Some(1050.0));
    capture.present(
        &mut hardware,
        &first.frame.context("coalesced redraw missing")?.frame,
    )?;
    assert!(worker
        .drive(DriveRequest::without_input(1045.0, InputState::default()))?
        .frame
        .is_none());
    let second = worker.drive(DriveRequest::without_input(1055.0, InputState::default()))?;
    assert_eq!(second.next_worker_deadline, Some(1060.0));
    capture.present(
        &mut hardware,
        &second.frame.context("next redraw missing")?.frame,
    )?;
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    let after = capture.monotonic_ns()?;
    let report = capture.close();
    let dispatched: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::TimerDispatched {
                timer_id,
                scheduled_deadline_seconds,
                scheduler_now_seconds,
            } => {
                assert!((if timer_id == 2 {
                    1045.0..1050.0
                } else {
                    1045.0..1060.0
                })
                .contains(&scheduler_now_seconds));
                Some((timer_id, scheduled_deadline_seconds))
            }
            _ => None,
        })
        .collect();
    assert_eq!(dispatched, [(1, 1010.0), (2, 1012.0), (1, 1050.0)]);
    let finished: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::TimerFinished {
                timer_id,
                success,
                scheduler_now_seconds,
                disposition,
            } => {
                assert!(success);
                assert!((1045.0..1060.0).contains(&scheduler_now_seconds));
                Some((timer_id, disposition))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        finished,
        [
            (
                1,
                TimerDisposition::Rescheduled {
                    next_deadline_seconds: 1050.0,
                    intervals_advanced: 4.0,
                    skipped_intervals: 3.0,
                    fallback: false
                }
            ),
            (2, TimerDisposition::Completed),
            (
                1,
                TimerDisposition::Rescheduled {
                    next_deadline_seconds: 1060.0,
                    intervals_advanced: 1.0,
                    skipped_intervals: 0.0,
                    fallback: false
                }
            ),
        ]
    );
    let redraws: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::RedrawRequested { coalesced } => Some(coalesced),
            _ => None,
        })
        .collect();
    assert_eq!(redraws, [false, true, true, true, false, true]);
    let pixels: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match &r.kind {
            EventKind::PresentEntered {
                marker: Ok(marker), ..
            } => Some((marker.frame_id(), marker.input_id())),
            _ => None,
        })
        .collect();
    // The token here counts timer callbacks, NOT physical input or causal evidence.
    assert_eq!(
        pixels,
        [(1, Some(b"2".as_slice())), (2, Some(b"3".as_slice()))]
    );
    assert_eq!(hardware.presented_frames().len(), 2);
    assert!(report
        .records
        .iter()
        .all(|r| (before..=after).contains(&r.at_ns)));
    assert_eq!(
        (report.lost, report.clock_failures, report.failed_operations),
        (0, 0, 0)
    );
    Ok(())
}

#[test]
fn private_diagnostic_capture_retains_timer_errors_and_self_cancellation_outcomes() -> Result<()> {
    use crate::diagnostic_observer::{Capture, EventKind, TimerDisposition};
    use crate::lua_worker::{DriveRequest, LuaWorker, StopReason};
    for cancel in [false, true] {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("timer-error.lua");
        std::fs::write(
            &source,
            format!(
                r##"
            local sliver = require("sliver.v1")
            local timer
            timer = sliver.timer.every(10, function()
                if {cancel} then timer:cancel() end
                sliver.timer.after(0, function() error("nested must not run") end)
                sliver.redraw()
                error("actual timer failure")
            end)
            sliver.timer.after(12, function() error("later must not run") end)
            return {{ api_version=1, render=function(canvas)
                canvas:rectangle(0, 0, 8, 8, "#0000ff")
            end }}
        "##
            ),
        )?;
        let capture = Capture::new(64)?;
        let worker = LuaWorker::stage_observed(&source, capture.clone())?.worker;
        let initial = worker.render_at(1000.0, 0.0)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        hardware.present(&initial.frame)?;
        worker.commit(1000.0, InputState::default())?;
        let error = worker
            .drive(DriveRequest::without_input(1045.0, InputState::default()))
            .err()
            .context("timer callback error was swallowed")?;
        assert!(error.to_string().contains("actual timer failure"));
        worker.shutdown(StopReason::Shutdown)?;
        hardware.release()?;
        let report = capture.close();
        assert_eq!(
            report.failed_operations, 1,
            "timer failure must invalidate diagnostics"
        );
        let dispatches: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::TimerDispatched { timer_id, .. } => Some(timer_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            dispatches,
            [1],
            "failure must not claim dispatch of later/nested work"
        );
        let outcomes: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::TimerFinished {
                    timer_id,
                    success,
                    disposition,
                    ..
                } => Some((timer_id, success, disposition)),
                _ => None,
            })
            .collect();
        assert_eq!(
            outcomes,
            [(
                1,
                false,
                if cancel {
                    TimerDisposition::Cancelled
                } else {
                    TimerDisposition::Rescheduled {
                        next_deadline_seconds: 1050.0,
                        intervals_advanced: 4.0,
                        skipped_intervals: 3.0,
                        fallback: false,
                    }
                }
            )]
        );
        assert_eq!(
            report
                .records
                .iter()
                .filter(|r| matches!(r.kind, EventKind::RenderAllocated { .. }))
                .count(),
            1
        );
        assert!(report.records.iter().any(|r| matches!(r.kind,
            EventKind::TimerActivated { timer_id: 3, scheduler_now_seconds, deadline_seconds } if scheduler_now_seconds == deadline_seconds)));
        assert_eq!(report.lost, 0);
        assert_eq!(hardware.presented_frames().len(), 1);
    }
    Ok(())
}

#[test]
fn private_diagnostic_capture_records_shifted_deadlines_and_nested_timer_completion() -> Result<()>
{
    use crate::diagnostic_observer::{Capture, EventKind, TimerDisposition};
    use crate::lua_worker::{DriveRequest, LuaWorker, StopReason, VisibilityReason};
    for explicit_resume in [false, true] {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("shifted-timers.lua");
        std::fs::write(
            &source,
            r##"
            local sliver = require("sliver.v1")
            local repeating, later
            local ticks = 0
            repeating = sliver.timer.every(10, function()
                ticks = ticks + 1
                repeating:cancel()
                later:cancel()
                sliver.redraw()
                sliver.timer.after(0, function() ticks = ticks + 1; sliver.redraw() end)
            end)
            later = sliver.timer.after(20, function() error("cancelled") end)
            return { api_version=1, render=function(canvas)
                assert(ticks == 2)
                canvas:rectangle(0, 0, 8, 8, "#ff0000")
            end }
        "##,
        )?;
        let capture = Capture::new(64)?;
        let before = capture.monotonic_ns()?;
        let worker = LuaWorker::stage_observed(&source, capture.clone())?.worker;
        worker.commit(1000.0, InputState::default())?;
        worker.drive(
            DriveRequest::without_input(1002.0, InputState::default()).with_visibility(
                false,
                VisibilityReason::Suspend,
                false,
            ),
        )?;
        let paused = worker.drive(DriveRequest::without_input(1100.0, InputState::default()))?;
        assert!(paused.frame.is_none());
        assert_eq!(paused.next_worker_deadline, None);
        let resume = DriveRequest::without_input(1102.0, InputState::default());
        let resumed = worker.drive(if explicit_resume {
            resume
                .with_timer_resume()
                .with_visibility(true, VisibilityReason::Device, false)
        } else {
            resume.with_visibility(true, VisibilityReason::Suspend, false)
        })?;
        assert!(resumed.frame.is_none());
        assert_eq!(resumed.next_worker_deadline, Some(1110.0));
        let response = worker.drive(DriveRequest::without_input(1111.0, InputState::default()))?;
        assert_eq!(response.next_worker_deadline, None);
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        hardware.present(&response.frame.context("nested timer redraw missing")?.frame)?;
        worker.shutdown(StopReason::Shutdown)?;
        hardware.release()?;
        let after = capture.monotonic_ns()?;
        let report = capture.close();
        let shifts: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::TimerDeadlineShifted {
                    timer_id,
                    previous_deadline_seconds,
                    shift_seconds,
                    deadline_seconds,
                } => Some((
                    timer_id,
                    previous_deadline_seconds,
                    shift_seconds,
                    deadline_seconds,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            shifts,
            [(1, 1010.0, 100.0, 1110.0), (2, 1020.0, 100.0, 1120.0)]
        );
        let dispatches: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::TimerDispatched {
                    timer_id,
                    scheduled_deadline_seconds,
                    ..
                } => Some((timer_id, scheduled_deadline_seconds)),
                _ => None,
            })
            .collect();
        assert_eq!(dispatches.len(), 2);
        assert_eq!(dispatches[0], (1, 1110.0));
        assert_eq!(dispatches[1].0, 3);
        assert!((1111.0..1112.0).contains(&dispatches[1].1));
        let outcomes: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::TimerFinished {
                    timer_id,
                    success,
                    disposition,
                    ..
                } => Some((timer_id, success, disposition)),
                _ => None,
            })
            .collect();
        assert_eq!(
            outcomes,
            [
                (1, true, TimerDisposition::Cancelled),
                (3, true, TimerDisposition::Completed)
            ]
        );
        assert!(report
            .records
            .iter()
            .all(|r| (before..=after).contains(&r.at_ns)));
        assert_eq!(
            (report.lost, report.clock_failures, report.failed_operations),
            (0, 0, 0)
        );
        assert_eq!(
            hardware.presented_frames()[0].rgba_at(1, 1),
            [255, 0, 0, 255]
        );
    }
    Ok(())
}

#[test]
fn private_diagnostic_capture_preserves_raw_timer_float_rounding_and_overflow() -> Result<()> {
    use crate::diagnostic_observer::{Capture, EventKind, TimerDisposition};
    use crate::lua_worker::{DriveRequest, LuaWorker, StopReason};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("raw-timers.lua");
    std::fs::write(
        &source,
        r##"
        local sliver = require("sliver.v1")
        sliver.timer.after(-0.0, function() end)
        local ticks = 0
        sliver.timer.every(2.2250738585072014e-308, function()
            ticks = ticks + 1
            sliver.redraw()
        end)
        return { api_version=1, render=function(canvas)
            assert(ticks == 1, "repeat may fire only once per drive")
            canvas:rectangle(0, 0, 8, 8, "#0000ff")
        end }
    "##,
    )?;
    let capture = Capture::new(64)?;
    let before = capture.monotonic_ns()?;
    let worker = LuaWorker::stage_observed(&source, capture.clone())?.worker;
    worker.commit(0.0, InputState::default())?;
    let response = worker.drive(DriveRequest::without_input(1e30, InputState::default()))?;
    // Existing fallback rounds now + tiny interval back to now; the observer
    // must neither fix the scheduler nor sanitize this into an exact count.
    assert_eq!(response.next_worker_deadline, Some(1e30));
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&response.frame.context("raw timer redraw missing")?.frame)?;
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    let after = capture.monotonic_ns()?;
    let report = capture.close();
    let registered: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::TimerRegistered {
                delay_seconds,
                interval_seconds,
                ..
            } => Some((delay_seconds.to_bits(), interval_seconds.map(f64::to_bits))),
            _ => None,
        })
        .collect();
    assert_eq!(
        registered,
        [
            ((-0.0_f64).to_bits(), None),
            (
                f64::MIN_POSITIVE.to_bits(),
                Some(f64::MIN_POSITIVE.to_bits())
            )
        ]
    );
    assert!(report.records.iter().any(|r| matches!(r.kind,
        EventKind::TimerFinished { timer_id: 2, success: true, scheduler_now_seconds: 1e30,
            disposition: TimerDisposition::Rescheduled { next_deadline_seconds: 1e30,
                intervals_advanced, skipped_intervals, fallback: true } }
        if intervals_advanced == f64::INFINITY && skipped_intervals == f64::INFINITY)));
    assert!(report
        .records
        .iter()
        .all(|r| (before..=after).contains(&r.at_ns)));
    assert_eq!(
        (report.lost, report.clock_failures, report.failed_operations),
        (0, 0, 0)
    );
    assert_eq!(
        hardware.presented_frames()[0].rgba_at(1, 1),
        [0, 0, 255, 255]
    );
    Ok(())
}

#[test]
fn private_minimal_callback_timing_covers_lua_body_and_error_without_detailed_capture() -> Result<()>
{
    use crate::diagnostic_observer::clock_ns;
    use crate::diagnostic_timing::{SpanKind, SpanStatus, TimingCapture};
    use crate::lua_worker::{LuaSource, LuaWorker, StopReason};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("callback-timing.lua");
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        local calls = 0
        return { api_version=1, render=function(canvas, intended)
            assert(intended == 987654321)
            calls = calls + 1
            -- Wall-time wait, not process CPU time (other tests may run threads).
            assert(os.execute("sleep 0.02"))
            if calls == 2 then error("timed callback failure") end
            canvas:rectangle(0, 0, 8, 8, "#00ff00")
        end }
    "##,
    )?;
    let timing = TimingCapture::new(16)?;
    let before = clock_ns(false)?;
    let worker = LuaWorker::stage_source_with_timing(
        &LuaSource::file(source),
        1.0,
        InputState::default(),
        timing.clone(),
    )?
    .worker;
    let frame = worker.render_at(987654321.0, 0.0)?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    hardware.present(&frame.frame)?;
    assert!(worker
        .render_at(987654321.0, 0.0)
        .err()
        .context("expected render error")?
        .to_string()
        .contains("timed callback failure"));
    worker.shutdown(StopReason::Shutdown)?;
    hardware.release()?;
    let after = clock_ns(false)?;
    let report = timing.close();
    assert_eq!(report.records.len(), 2, "missing actual callback spans");
    assert_eq!(report.records[0].status, SpanStatus::Succeeded);
    assert_eq!(report.records[1].status, SpanStatus::Failed);
    for sample in &report.records {
        assert_eq!(sample.kind, SpanKind::RenderCallback);
        assert!(sample.frame_bearing);
        assert!(
            before <= sample.start_ns && sample.start_ns <= sample.end_ns && sample.end_ns <= after
        );
        // Generous lower bound detects measuring only canvas finalization instead
        // of the deliberately long Lua body. This is not an overhead benchmark.
        assert!(sample.end_ns - sample.start_ns >= 10_000_000);
    }
    assert!(report.records[0].end_ns <= report.records[1].start_ns);
    assert_eq!(
        (
            report.lost,
            report.open_spans,
            report.clock_failures,
            report.failed_spans
        ),
        (0, 0, 0, 1)
    );
    assert_eq!(
        hardware.presented_frames()[0].rgba_at(1, 1),
        [0, 255, 0, 255]
    );
    Ok(())
}

#[test]
fn private_diagnostic_capture_observes_real_render_and_complete_fake_present() -> Result<()> {
    use crate::diagnostic_observer::{Capture, EventKind};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("observed.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        r#"
        require("sliver.v1")
        local marker = require("native_marker")
        return { api_version = 1, render = function(canvas)
            marker.draw(canvas, {run_id="r", generation="g", frame_id=1})
        end }
    "#,
    )?;
    let capture = Capture::new(64)?;
    let before = capture.monotonic_ns()?;
    let worker = crate::lua_worker::LuaWorker::stage_observed(&source, capture.clone())?.worker;
    // Deliberately unrelated intended time must never become an observation.
    let frame = worker.render_at(987654321.0, 0.0)?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    capture.present(&mut hardware, &frame.frame)?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    hardware.release()?;
    let after = capture.monotonic_ns()?;
    let report = capture.close();
    assert_eq!(report.lost, 0);
    assert_eq!(report.clock_failures, 0);
    assert!(report.closed);
    assert!(report.clock_resolution_ns > 0);
    assert!(report
        .records
        .iter()
        .all(|r| (before..=after).contains(&r.at_ns)));
    assert!(report
        .records
        .windows(2)
        .all(|r| r[0].sequence + 1 == r[1].sequence));
    assert!(matches!(
        report.records[0].kind,
        EventKind::RenderAllocated { attempt: 1 }
    ));
    let returned = report
        .records
        .iter()
        .find_map(|r| match &r.kind {
            EventKind::PresentEntered {
                marker: Ok(marker), ..
            } => Some(marker),
            _ => None,
        })
        .context("missing independently decoded broker-entry pixels")?;
    assert_eq!(returned.run_id(), b"r");
    assert_eq!(returned.generation(), b"g");
    assert_eq!(returned.frame_id(), 1);
    assert_eq!(returned.input_id(), None);
    assert!(report
        .records
        .iter()
        .any(|r| matches!(r.kind, EventKind::Published { sequence: 1, .. })));
    assert!(report
        .records
        .iter()
        .any(|r| matches!(r.kind, EventKind::Selected { sequence: 1 })));
    assert!(matches!(
        report.records.last().unwrap().kind,
        EventKind::PresentReturned {
            call: 1,
            success: true
        }
    ));
    assert_eq!(hardware.presented_frames().len(), 1);
    Ok(())
}

#[test]
fn private_diagnostic_capture_accounts_for_three_slot_reclamation_and_selection() -> Result<()> {
    use crate::diagnostic_observer::{Capture, EventKind};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("observed.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        r#"
        require("sliver.v1")
        local marker = require("native_marker")
        local id = 0
        return { api_version = 1, render = function(canvas)
            id = id + 1
            marker.draw(canvas, {run_id="r", generation="g", frame_id=id})
        end }
    "#,
    )?;
    for shared in [false, true] {
        let capture = Capture::new(64)?;
        let worker = if shared {
            crate::lua_worker::LuaWorker::stage_observed_shared(
                &source,
                &directory.path().join("frames"),
                capture.clone(),
            )?
            .worker
        } else {
            crate::lua_worker::LuaWorker::stage_observed(&source, capture.clone())?.worker
        };
        for _ in 0..5 {
            worker.render_to_slots_at(0.0, 0.0)?;
        }
        let frame = worker
            .broker_for_test()
            .take_newest()?
            .context("newest frame missing")?;
        let (frame, _) = LogicalFrame::from_completed(frame);
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        capture.present(&mut hardware, &frame)?;
        worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
        hardware.release()?;
        let report = capture.close();
        let mut discarded: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::Discarded { sequence, .. } => Some(sequence),
                _ => None,
            })
            .collect();
        let reasons: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::Discarded { reason, .. } => Some(reason),
                _ => None,
            })
            .collect();
        use crate::diagnostic_observer::DiscardReason;
        assert_eq!(
            reasons,
            [
                DiscardReason::ProducerReclaim,
                DiscardReason::ProducerReclaim,
                DiscardReason::ConsumerSuperseded,
                DiscardReason::ConsumerSuperseded
            ]
        );
        discarded.sort_unstable();
        assert_eq!(discarded, [1, 2, 3, 4]);
        let selected: Vec<_> = report
            .records
            .iter()
            .filter_map(|r| match r.kind {
                EventKind::Selected { sequence } => Some(sequence),
                _ => None,
            })
            .collect();
        assert_eq!(selected, [5]);
        assert_eq!(
            crate::diagnostic_observer::decode(&frame)
                .unwrap()
                .frame_id(),
            5
        );
        assert_eq!(report.lost, 0);
    }
    Ok(())
}

#[test]
fn private_diagnostic_capture_links_normalized_touch_delivery_to_fixture_pixels() -> Result<()> {
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::hardware::HardwareEvent;
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("observed.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        include_str!("../../../scripts/native-performance-observer-smoke.lua"),
    )?;
    let capture = Capture::new(64)?;
    let worker = crate::lua_worker::LuaWorker::stage_observed(&source, capture.clone())?.worker;
    worker.commit(0.0, InputState::default())?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    let initial = worker.render_at(0.0, 0.0)?;
    capture.present(&mut hardware, &initial.frame)?;
    let touch = TouchEvent {
        phase: TouchPhase::Down,
        id: 42,
        time: 123.0,
        x: 0.0,
        y: 30.0,
        modifiers: ModifierState::default(),
        pressure: None,
        width: None,
        height: None,
    };
    hardware.inject(HardwareEvent::Touch(touch));
    let events = capture.poll(&mut hardware, Duration::ZERO)?;
    assert_eq!(events, [HardwareEvent::Touch(touch)]);
    let response = worker.drive(
        crate::lua_worker::DriveRequest::without_input(0.0, InputState::default())
            .with_events(vec![touch]),
    )?;
    capture.present(
        &mut hardware,
        &response.frame.context("missing causal frame")?.frame,
    )?;
    // A repeated presentation remains a separate call, not a new identity.
    capture.present(&mut hardware, &initial.frame)?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    hardware.release()?;
    let report = capture.close();
    let received = report
        .records
        .iter()
        .position(
            |r| matches!(r.kind, EventKind::InputReceived(HardwareEvent::Touch(e)) if e == touch),
        )
        .context("missing normalized receipt")?;
    let delivered = report
        .records
        .iter()
        .position(|r| matches!(r.kind, EventKind::TouchCallbackEntered(e) if e == touch))
        .context("missing worker delivery")?;
    let returned = report
        .records
        .iter()
        .position(|r| {
            matches!(
                r.kind,
                EventKind::TouchCallbackReturned {
                    contact: 42,
                    success: true
                }
            )
        })
        .context("missing callback outcome")?;
    let decoded: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match &r.kind {
            EventKind::PresentEntered {
                marker: Ok(marker), ..
            } => Some((marker.frame_id(), marker.input_id())),
            _ => None,
        })
        .collect();
    assert_eq!(decoded, [(1, None), (2, Some(b"1".as_slice())), (1, None)]);
    assert!(received < delivered && delivered < returned);
    assert_eq!(report.lost, 0);
    Ok(())
}

#[test]
fn private_diagnostic_capture_retains_pending_replacement_and_shutdown_dispositions() -> Result<()>
{
    use crate::diagnostic_observer::{Capture, EventKind};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("observed.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        include_str!("../../../scripts/native-performance-observer-smoke.lua"),
    )?;
    let capture = Capture::new(64)?;
    let worker = crate::lua_worker::LuaWorker::stage_observed(&source, capture.clone())?.worker;
    let held = worker.hold_slots_for_test();
    worker.render_to_slots_at(0.0, 0.0)?;
    worker.render_to_slots_at(0.0, 0.0)?;
    drop(held);
    worker.render_to_slots_at(0.0, 0.0)?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    let report = capture.close();
    let pending: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match &r.kind {
            EventKind::PendingDiscarded {
                marker: Ok(marker), ..
            } => Some(marker.frame_id()),
            _ => None,
        })
        .collect();
    assert_eq!(pending, [1, 2]);
    let discarded: Vec<_> = report
        .records
        .iter()
        .filter_map(|r| match r.kind {
            EventKind::Discarded { sequence, .. } => Some(sequence),
            _ => None,
        })
        .collect();
    assert_eq!(
        discarded,
        [1],
        "unselected published frame must resolve on teardown"
    );
    assert_eq!(report.lost, 0);
    Ok(())
}

#[test]
fn private_diagnostic_capture_is_bounded_and_retains_overflow_and_late_writes() -> Result<()> {
    use crate::diagnostic_observer::Capture;
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("observed.lua");
    std::fs::write(
        directory.path().join("native_marker.lua"),
        include_str!("../../../scripts/native-performance-marker.lua"),
    )?;
    std::fs::write(
        &source,
        include_str!("../../../scripts/native-performance-observer-smoke.lua"),
    )?;
    assert!(Capture::new(0).is_err());
    assert!(Capture::new(65_537).is_err());
    let capture = Capture::new(1)?;
    let worker = crate::lua_worker::LuaWorker::stage_observed(&source, capture.clone())?.worker;
    let frame = worker.render_at(0.0, 0.0)?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    let first = capture.close();
    assert_eq!(first.records.len(), 1);
    assert_eq!(first.capacity, 1);
    assert_eq!(first.attempted_records, 4);
    assert_eq!(first.lost, 3);
    assert_eq!(first.after_close, 0);
    assert!(
        first.record_bytes <= 512,
        "fixed-size record storage grew unexpectedly"
    );
    assert_eq!(first.clock_read_samples_ns.len(), 32);
    // A mistakenly live source cannot mutate already closed records or silently
    // turn its writes into evidence. Its hardware behaviour is unchanged.
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    capture.present(&mut hardware, &frame.frame)?;
    hardware.release()?;
    let last = capture.close();
    assert_eq!(last.records.len(), 1);
    assert_eq!(last.lost, 5);
    assert_eq!(last.after_close, 2);
    assert_eq!(last.closed_at_ns, first.closed_at_ns);
    Ok(())
}

#[test]
fn private_diagnostic_capture_retains_decode_render_and_complete_present_failures() -> Result<()> {
    use crate::diagnostic_observer::{Capture, DecodeError, EventKind};
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("failure.lua");
    std::fs::write(
        &source,
        r#"
        require("sliver.v1")
        local count = 0
        return { api_version=1, render=function(canvas)
            count = count + 1
            if count == 2 then error("deliberate render failure") end
            -- No marker: this must remain an undecodable actual frame.
        end }
    "#,
    )?;
    let capture = Capture::new(32)?;
    let worker = crate::lua_worker::LuaWorker::stage_observed(&source, capture.clone())?.worker;
    let frame = worker.render_at(0.0, 0.0)?;
    let mut hardware = FakeTouchBar::new(); // Intentionally unclaimed.
    assert!(capture.present(&mut hardware, &frame.frame).is_err());
    assert!(worker.render_at(0.0, 0.0).is_err());
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    let report = capture.close();
    assert_eq!(report.failed_operations, 2);
    assert_eq!(report.decode_failures, 3); // render, publication, adapter entry
    assert!(report.records.iter().any(|r| matches!(
        r.kind,
        EventKind::PresentEntered {
            marker: Err(DecodeError::Packet),
            ..
        }
    )));
    assert!(report.records.iter().any(|r| matches!(
        r.kind,
        EventKind::PresentReturned {
            call: 1,
            success: false
        }
    )));
    assert!(report.records.iter().any(|r| matches!(
        r.kind,
        EventKind::RenderFinished {
            attempt: 2,
            marker: None
        }
    )));
    assert_eq!(report.lost, 0);
    assert!(hardware.presented_frames().is_empty());
    Ok(())
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
fn lua_canvas_text_y_is_a_layout_origin_not_a_baseline() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("text-origin.lua");
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        local frame = 0
        return {
            api_version = 1,
            render = function(canvas)
                frame = frame + 1
                canvas:rectangle(0, 54, 2008, 6, "#ff3b81")
                if frame == 1 then
                    canvas:text(20, 36, "gjpqy", 24, "#ffffff")
                else
                    local _, height = canvas:measure_text("gjpqy", 24)
                    canvas:text(20, 60 - height - 8, "gjpqy", 24, "#ffffff")
                end
            end,
        }
        "##,
    )?;
    let mut hardware = FakeTouchBar::new();
    hardware.claim()?;
    let crate::lua_worker::StagedLuaWorker { worker } =
        crate::lua_worker::LuaWorker::stage(&source)?;
    let first = worker.render_at(0.0, 0.0)?;
    hardware.present(&first.frame)?;
    let second = worker.render_next()?;
    hardware.present(&second)?;
    worker.shutdown(crate::lua_worker::StopReason::Shutdown)?;
    hardware.release()?;

    let frames = hardware.presented_frames();
    assert_eq!(frames.len(), 2);
    let unsafe_text_overlaps_band =
        (20..150).any(|x| (54..60).any(|y| frames[0].rgba_at(x, y) != [255, 59, 129, 255]));
    assert!(
        unsafe_text_overlaps_band,
        "a y=36 text origin did not reproduce the clipped descender case"
    );
    assert_bottom_band_is_solid(
        &frames[1],
        150,
        "measure_text-based placement painted into the bottom band",
    );
    Ok(())
}

#[test]
fn lua_canvas_capture_keeps_safe_text_and_edge_bands_inside_the_frame() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("text-bounds.lua");
    std::fs::write(
        &source,
        r##"
        require("sliver.v1")
        return {
            api_version = 1,
            render = function(canvas)
                canvas:rectangle(0, 0, 2008, 60, "#102040")
                canvas:rectangle(0, 0, 2008, 6, "#00e5ff")
                canvas:rectangle(0, 54, 2008, 6, "#ff3b81")
                canvas:text(20, 24, "release verifier", 24, "#ffffff")
            end,
        }
        "##,
    )?;
    let mut hardware = FakeTouchBar::new();

    present_lua_once(&source, &mut hardware)?;

    let frame = hardware
        .presented_frames()
        .first()
        .context("worker did not present the text bounds fixture")?;
    assert_eq!(frame.dimensions(), (2008, 60));
    for x in [0, 1000, 2007] {
        assert_eq!(frame.rgba_at(x, 0), [0, 229, 255, 255]);
        assert_eq!(frame.rgba_at(x, 59), [255, 59, 129, 255]);
    }
    let text_pixels = (6..54).any(|y| (20..200).any(|x| frame.rgba_at(x, y) != [16, 32, 64, 255]));
    assert!(text_pixels, "safe text origin produced no text pixels");
    assert_bottom_band_is_solid(
        frame,
        200,
        "safe text origin painted into the bottom edge band",
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
