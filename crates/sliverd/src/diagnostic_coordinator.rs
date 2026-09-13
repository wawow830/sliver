//! Private lockstep process-fixture coordinator. It uses the existing worker
//! render/commit/drive/watchdog protocol, never an autonomous producer or a new
//! scheduler. Startup acquisition is deliberately not installed.
//!
//! A returned adapter call is NOT authenticated native broker provenance (in
//! particular a BrokerClient returns later than the broker hardware seam).
//! Records here close known lockstep work and bind minimal drive samples in all
//! modes. They must not be converted into native acceptance endpoints.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};

use crate::diagnostic_capture_transport::{FixtureBootstrap, FixtureControl};
use crate::diagnostic_fixture::{Mode, Receipt, Resolution};
use crate::diagnostic_observer::{clock_ns, decode};
use crate::diagnostic_timing::{SpanKind, TimingCapture};
use crate::frame_slots::FrameCorrelation;
use crate::hardware::{InputState, TouchBarHardware};
use crate::lua_worker::{worker_process::ProcessWorker, DriveRequest, StopReason, TimedFrame};

const MAX_FRAMES: usize = 8192;
const MAX_RECEIPTS_PER_DRIVE: usize = 64;
const WARMUP_NS: u64 = 5_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cohort {
    Warmup,
    Measured,
    Drain,
}

#[derive(Clone, Debug)]
pub(crate) struct FrameRecord {
    pub(crate) correlation: FrameCorrelation,
    pub(crate) cohort: Cohort,
    pub(crate) drive_span: u64,
    pub(crate) present_span: u64,
    /// Actual coordinator observation after the complete supplied adapter call.
    /// This is NOT a substitute for an independently captured broker endpoint.
    pub(crate) adapter_return_ns: u64,
    pub(crate) success: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Report {
    pub(crate) frames: Vec<FrameRecord>,
    pub(crate) warmup_closed_ns: Option<u64>,
    pub(crate) fixture_closed_ns: Option<u64>,
    pub(crate) worker_stopped_ns: Option<u64>,
    pub(crate) failure: Option<String>,
}

pub(crate) struct Coordinator {
    worker: Option<ProcessWorker>,
    plan: FixtureBootstrap,
    timing: TimingCapture,
    first_warmup_ns: Option<u64>,
    last_endpoint_ns: u64,
    started: bool,
    report: Report,
}

impl Coordinator {
    /// The caller owns source/build/role binding and hardware ownership. This
    /// does not acquire services, change permissions, or establish provenance.
    pub(crate) fn new(
        worker: ProcessWorker,
        plan: FixtureBootstrap,
        timing: TimingCapture,
    ) -> Result<Self> {
        plan.plan()?;
        ensure!(
            worker.matches_fixture_plan(&plan),
            "worker/bootstrap plan mismatch"
        );
        ensure!(
            clock_ns(false)? < plan.start_ns,
            "coordinator epoch is not future"
        );
        let mut report = Report::default();
        report.frames.try_reserve_exact(MAX_FRAMES)?;
        timing.calibrate()?;
        Ok(Self {
            worker: Some(worker),
            plan,
            timing,
            first_warmup_ns: None,
            last_endpoint_ns: 0,
            started: false,
            report,
        })
    }

    fn worker(&self) -> Result<&ProcessWorker> {
        ensure!(self.report.failure.is_none(), "coordinator already failed");
        self.worker
            .as_ref()
            .context("coordinator worker already stopped")
    }

    fn outcome<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(error) = &result {
            if self.report.failure.is_none() {
                self.report.failure = Some(format!("{error:#}"));
            }
            // No further Lua callbacks after protocol, hardware or correlation
            // failure. The caller can still retain the mapped committed prefix.
            if let Some(worker) = &self.worker {
                worker.terminate();
            }
        }
        result
    }

    pub(crate) fn start<H: TouchBarHardware>(
        &mut self,
        hardware: &mut H,
        now_seconds: f64,
    ) -> Result<()> {
        let span = self.timing.begin(SpanKind::SupervisorDrive);
        let mut frame_bearing = false;
        let result = (|| {
            ensure!(!self.started, "coordinator already started");
            let frame = self
                .worker()?
                .render(now_seconds, 0.0, InputState::default())?;
            frame_bearing = true;
            self.present(hardware, frame, span.id())?;
            self.worker()?.commit(now_seconds, InputState::default())?;
            self.started = true;
            Ok(())
        })();
        span.finish(frame_bearing, result.is_ok());
        self.outcome(result)
    }

    /// Receipts must already be reconciled in original source order. This
    /// bounded lockstep path rejects late forwarding across a resolved endpoint;
    /// it rejects equality with the last response because clock readings alone
    /// cannot order cross-source ties, and never guesses missing notifications.
    /// Returns the existing runtime's deadline, not a new timing policy.
    pub(crate) fn drive<H: TouchBarHardware>(
        &mut self,
        hardware: &mut H,
        now_seconds: f64,
        receipts: &[Receipt],
    ) -> Result<Option<f64>> {
        // Includes controls, worker dispatch, complete-frame copying, optional
        // detailed decode, complete adapter call and resolution acknowledgement.
        let span = self.timing.begin(SpanKind::SupervisorDrive);
        let mut frame_bearing = false;
        let result = (|| {
            ensure!(self.started, "coordinator not started");
            ensure!(
                self.report.fixture_closed_ns.is_none(),
                "fixture already closed"
            );
            ensure!(
                receipts.len() <= MAX_RECEIPTS_PER_DRIVE,
                "coordinator receipt capacity exhausted"
            );
            for receipt in receipts {
                ensure!(
                    receipt.received_ns > self.last_endpoint_ns,
                    "receipt forwarded after a later or ambiguously equal resolved endpoint"
                );
                self.worker()?
                    .fixture_control(FixtureControl::Receive(*receipt))?;
            }
            let effects = self.worker()?.drive_until(
                DriveRequest::without_input(now_seconds, InputState::default())
                    .with_events(receipts.iter().map(|r| r.event).collect()),
                Instant::now() + Duration::from_secs(2),
            )?;
            ensure!(
                effects.backlight.is_none() && effects.key_requests.is_empty(),
                "unexpected effects from reviewed fixture"
            );
            frame_bearing = effects.frame.is_some();
            if let Some(frame) = effects.frame {
                self.present(hardware, frame, span.id())?;
            }
            Ok(if effects.redraw_pending {
                Some(now_seconds)
            } else {
                effects.next_worker_deadline
            })
        })();
        span.finish(frame_bearing, result.is_ok());
        self.outcome(result)
    }

    fn present<H: TouchBarHardware>(
        &mut self,
        hardware: &mut H,
        frame: TimedFrame,
        drive_span: u64,
    ) -> Result<()> {
        let correlation = frame
            .fixture_correlation()
            .context("fixture frame lacks allocation correlation")?;
        let allocation = correlation.allocation;
        let expected = self
            .report
            .frames
            .last()
            .map_or(1, |r| r.correlation.allocation.id + 1);
        let expected_sequence = self
            .report
            .frames
            .last()
            .map_or(1, |r| r.correlation.sequence + 1);
        ensure!(
            allocation.id == expected && correlation.sequence == expected_sequence,
            "lockstep allocation/publication gap or replay"
        );
        ensure!(
            self.report.frames.len() < MAX_FRAMES,
            "coordinator frame capacity exhausted"
        );
        let before = clock_ns(false)?;
        ensure!(
            allocation.allocated_ns > 0
                && allocation.allocated_ns <= before
                && allocation.allocated_ns >= self.last_endpoint_ns,
            "impossible or out-of-order fixture allocation time"
        );
        let plan = self.plan.plan()?;
        let cohort = if allocation.allocated_ns < plan.start_ns {
            ensure!(
                self.report.warmup_closed_ns.is_none(),
                "warmup frame after closure"
            );
            self.first_warmup_ns.get_or_insert(allocation.allocated_ns);
            Cohort::Warmup
        } else {
            ensure!(
                self.report.warmup_closed_ns.is_some(),
                "measured work without warmup closure"
            );
            ensure!(
                allocation.allocated_ns < plan.end_ns,
                "allocation at or after E"
            );
            if allocation.allocated_ns < plan.stop_ns {
                Cohort::Measured
            } else {
                Cohort::Drain
            }
        };
        // Mode B intentionally does NOT decode: detailed observation stays off.
        // C independently checks the stable complete pixels, not supplied labels.
        if self.plan.mode == Mode::C {
            let marker = decode(&frame.frame)
                .map_err(|e| anyhow::anyhow!("fixture marker decode: {e:?}"))?;
            let token = (allocation.token != 0).then(|| allocation.token.to_string());
            ensure!(
                marker.run_id() == self.plan.run.as_bytes()
                    && marker.generation() == self.plan.generation.as_bytes()
                    && marker.frame_id() == allocation.id
                    && marker.input_id() == token.as_deref().map(str::as_bytes),
                "fixture pixels disagree with allocation/publication correlation"
            );
        }
        // Intrinsic in every mode, including a bare adapter. A separately
        // decorated broker may have its own source/sampler, but callers must
        // not record it twice into this coordinator's TimingCapture.
        let present_span = self.timing.begin(SpanKind::Present);
        let present_id = present_span.id();
        let result = hardware.present(&frame.frame);
        let returned = clock_ns(false);
        present_span.finish(true, result.is_ok() && returned.is_ok());
        let returned_ns = returned?;
        ensure!(returned_ns >= before, "coordinator adapter clock regressed");
        self.report.frames.push(FrameRecord {
            correlation,
            cohort,
            drive_span,
            present_span: present_id,
            adapter_return_ns: returned_ns,
            success: result.is_ok(),
        });
        self.last_endpoint_ns = returned_ns;
        // A failed adapter operation remains failed even if the child rejects
        // the explicit Failed resolution. Never manufacture a successful ack.
        let resolution = self.worker()?.fixture_control(FixtureControl::Resolve {
            frame_id: allocation.id,
            resolution: if result.is_ok() {
                Resolution::Presented
            } else {
                Resolution::Failed
            },
            resolved_ns: returned_ns,
        });
        result?;
        resolution?;
        Ok(())
    }

    pub(crate) fn confirm_warmup_closed(&mut self) -> Result<()> {
        let result = (|| {
            let first = self.first_warmup_ns.context("no warmup frame")?;
            let now = clock_ns(false)?;
            ensure!(
                now >= first + WARMUP_NS && now < self.plan.start_ns,
                "warmup needs five seconds strictly before T"
            );
            ensure!(
                self.report.warmup_closed_ns.is_none(),
                "warmup already closed"
            );
            self.worker()?
                .fixture_control(FixtureControl::ConfirmWarmupClosed)?;
            let ack = clock_ns(false)?;
            ensure!(ack < self.plan.start_ns, "warmup acknowledgement reached T");
            self.report.warmup_closed_ns = Some(ack);
            Ok(())
        })();
        self.outcome(result)
    }

    /// E is unchanged. Finish confirms semantic worker closure before graceful
    /// stop/reap; raw source closure/health is a separate collector obligation.
    pub(crate) fn finish(&mut self) -> Result<()> {
        let result = (|| {
            ensure!(self.started, "coordinator not started");
            ensure!(
                clock_ns(false)? >= self.plan.plan()?.end_ns,
                "fixture finish before E"
            );
            self.worker()?.fixture_control(FixtureControl::Finish)?;
            self.report.fixture_closed_ns = Some(clock_ns(false)?);
            self.worker
                .take()
                .context("worker missing at stop")?
                .shutdown_until(
                    StopReason::Shutdown,
                    Instant::now() + Duration::from_millis(500),
                )?;
            self.report.worker_stopped_ns = Some(clock_ns(false)?);
            Ok(())
        })();
        self.outcome(result)
    }

    pub(crate) fn report(&self) -> &Report {
        &self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic_capture_transport::Collector;
    use crate::diagnostic_fixture::{ObservationKind, SOURCE};
    use crate::diagnostic_hardware::ObservedHardware;
    use crate::diagnostic_observer::{Capture, EventKind};
    use crate::diagnostic_timing::SpanStatus;
    use crate::hardware::FakeTouchBar;
    use crate::lua_worker::LuaSource;

    fn child(
        mode: Mode,
        start_ns: u64,
        capacity: usize,
    ) -> Result<(Coordinator, Collector, TimingCapture)> {
        let mut collector = Collector::new(capacity)?;
        let plan = FixtureBootstrap::new("coordinator-test", "child", start_ns, 30, mode)?;
        let worker = ProcessWorker::stage_with_fixture(
            &LuaSource::embedded(SOURCE.to_vec()),
            0.75,
            InputState::default(),
            collector.take_worker_storage()?,
            plan.clone(),
        )?;
        let timing = TimingCapture::new(65_536)?;
        let coordinator = Coordinator::new(worker, plan, timing.clone())?;
        Ok((coordinator, collector, timing))
    }

    #[test]
    fn private_process_coordinator_rejects_plan_mismatch_before_work() -> Result<()> {
        for mutation in 0..6 {
            let mut collector = Collector::new(128)?;
            let actual = FixtureBootstrap::new(
                "bound-run",
                "bound-child",
                clock_ns(false)? + 10_000_000_000,
                30,
                Mode::A,
            )?;
            let worker = ProcessWorker::stage_with_fixture(
                &LuaSource::embedded(SOURCE.to_vec()),
                0.75,
                InputState::default(),
                collector.take_worker_storage()?,
                actual.clone(),
            )?;
            let mut declared = actual;
            match mutation {
                0 => declared.mode = Mode::B,
                1 => declared.rate = 60,
                2 => declared.causal = true,
                3 => declared.start_ns += 1,
                4 => declared.run = "other-run".into(),
                _ => declared.generation = "other-child".into(),
            }
            let error = Coordinator::new(worker, declared, TimingCapture::new(64)?)
                .err()
                .context("accepted mismatched private coordinator plan")?;
            assert!(error.to_string().contains("bootstrap plan mismatch"));
            let raw = collector.snapshot()?;
            assert!(!raw.report.records.iter().any(|r| matches!(
                r.kind,
                EventKind::Published { .. } | EventKind::RenderAllocated { .. }
            )));
        }
        Ok(())
    }

    #[test]
    fn private_process_coordinator_retains_all_mode_identity_and_actual_calibration() -> Result<()>
    {
        for mode in [Mode::A, Mode::B, Mode::C] {
            let (mut coordinator, collector, timing) =
                child(mode, clock_ns(false)? + 10_000_000_000, 512)?;
            let mut hardware = FakeTouchBar::new();
            hardware.claim()?;
            coordinator.start(&mut hardware, 0.0)?;
            let report = coordinator.report();
            assert_eq!(report.frames.len(), 1);
            let frame = &report.frames[0];
            assert_eq!(frame.correlation.allocation.id, 1);
            assert_eq!(frame.correlation.sequence, 1);
            assert_eq!(frame.correlation.allocation.token, 0);
            assert!(frame.correlation.allocation.allocated_ns < frame.adapter_return_ns);
            assert_eq!(frame.cohort, Cohort::Warmup);
            let span_id = frame.drive_span;
            let adapter_return_ns = frame.adapter_return_ns;
            // Early finish must poison this attempt, not close/relabel it.
            assert!(coordinator.finish().is_err());
            assert!(coordinator.report().failure.is_some());
            assert!(coordinator.report().fixture_closed_ns.is_none());
            assert!(coordinator.drive(&mut hardware, 0.1, &[]).is_err());
            drop(coordinator);
            let raw = collector.snapshot()?;
            let probes: Vec<_> = raw
                .report
                .records
                .iter()
                .filter_map(|r| match &r.kind {
                    EventKind::MinimalSpan(s) if s.kind == SpanKind::Calibration => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(probes.len(), 32);
            assert!(probes
                .iter()
                .all(|s| s.status == SpanStatus::Succeeded && !s.frame_bearing));
            assert!(raw
                .report
                .records
                .iter()
                .any(|r| matches!(r.kind, EventKind::MinimalCalibration { .. })));
            let local = timing.close();
            let drive = local.records.iter().find(|s| s.span_id == span_id).unwrap();
            assert_eq!(drive.kind, SpanKind::SupervisorDrive);
            assert!(drive.frame_bearing && drive.status == SpanStatus::Succeeded);
            assert!(drive.end_ns >= adapter_return_ns);
            hardware.release()?;
        }
        Ok(())
    }

    #[test]
    fn private_process_coordinator_forwards_observed_warmup_touch_to_causal_pixels() -> Result<()> {
        use crate::hardware::{HardwareEvent, ModifierState, TouchEvent, TouchPhase};
        let (mut coordinator, collector, _) =
            child(Mode::C, clock_ns(false)? + 10_000_000_000, 512)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        coordinator.start(&mut hardware, 0.0)?;
        // Exercise actual normalized fake-adapter receipt -> existing private
        // control -> actual child callback -> independently decoded pixels.
        // This is warmup software work, not physical-input sampling acceptance.
        for (index, phase) in [TouchPhase::Down, TouchPhase::Up].into_iter().enumerate() {
            let event = TouchEvent {
                phase,
                id: 17,
                time: 0.125,
                x: 19.0,
                y: 23.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            };
            hardware.inject(HardwareEvent::Touch(event));
            let receipt_source = Capture::new(8)?;
            let events = receipt_source.poll(&mut hardware, Duration::ZERO)?;
            assert_eq!(events.len(), 1);
            let observed = receipt_source.close();
            let receipt_record = observed.records.iter().find(|r|
                matches!(r.kind, EventKind::InputReceived(HardwareEvent::Touch(t)) if t == event)).unwrap();
            let receipt = Receipt {
                sequence: index as u64 + 1,
                received_ns: receipt_record.at_ns,
                event,
            };
            coordinator.drive(&mut hardware, 0.25 + index as f64 * 0.1, &[receipt])?;
        }
        let correlated = coordinator
            .report()
            .frames
            .iter()
            .find(|r| r.correlation.allocation.token == 1)
            .context("touch callback produced no correlated token frame")?;
        let frame_id = correlated.correlation.allocation.id;
        let endpoint = correlated.adapter_return_ns;
        let last = hardware.presented_frames().last().unwrap();
        // Outside the marker, reviewed token 1 changes green from 0 to 37.
        // The coordinator independently decoded/compared the marker at entry.
        assert_eq!(last.rgba_at(0, 0)[1], 37);
        drop(coordinator);
        let raw = collector.snapshot()?;
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
        assert!(raw.report.records.iter().any(|r| matches!(r.kind,
            EventKind::FixtureObserved { kind: ObservationKind::ResponseConfirmed { receipt_sequence: 1, frame_id: id, token: 1, responded_ns }, .. }
                if id == frame_id && responded_ns == endpoint)));
        hardware.release()?;
        Ok(())
    }

    #[test]
    fn private_process_coordinator_rejects_ambiguous_equal_timestamp_receipt() -> Result<()> {
        use crate::hardware::{ModifierState, TouchEvent, TouchPhase};
        let (mut coordinator, collector, _) =
            child(Mode::C, clock_ns(false)? + 10_000_000_000, 512)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        coordinator.start(&mut hardware, 0.0)?;
        // An earlier receipt forwarded only after the response cannot use equal
        // clock readings to invent source order. Lockstep has no cross-source
        // order key, so this ambiguous equality must fail before dispatch.
        let receipt = Receipt {
            sequence: 1,
            received_ns: coordinator.report().frames[0].adapter_return_ns,
            event: TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 0.0,
                y: 0.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            },
        };
        assert!(coordinator.drive(&mut hardware, 0.1, &[receipt]).is_err());
        assert!(coordinator.report().failure.is_some());
        drop(coordinator);
        assert!(!collector
            .snapshot()?
            .report
            .records
            .iter()
            .any(|r| matches!(
                r.kind,
                EventKind::FixtureObserved {
                    kind: ObservationKind::ReceiptSupplied { .. },
                    ..
                }
            )));
        hardware.release()?;
        Ok(())
    }

    #[test]
    fn private_process_coordinator_always_retains_complete_adapter_timing() -> Result<()> {
        let (mut coordinator, _, timing) = child(Mode::A, clock_ns(false)? + 10_000_000_000, 128)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        coordinator.start(&mut hardware, 0.0)?;
        let returned = coordinator.report().frames[0].adapter_return_ns;
        drop(coordinator);
        let report = timing.close();
        let presents: Vec<_> = report
            .records
            .iter()
            .filter(|s| s.kind == SpanKind::Present)
            .collect();
        assert_eq!(
            presents.len(),
            1,
            "bare adapters must not bypass minimal Present sampling"
        );
        assert!(presents[0].start_ns <= returned && presents[0].end_ns >= returned);
        assert!(presents[0].frame_bearing && presents[0].status == SpanStatus::Succeeded);
        hardware.release()?;
        Ok(())
    }

    #[test]
    fn private_process_coordinator_adapter_failure_keeps_failed_frame_and_stops_child() -> Result<()>
    {
        let (mut coordinator, collector, timing) =
            child(Mode::C, clock_ns(false)? + 10_000_000_000, 512)?;
        // Deliberately unclaimed adapter: actual present fails.
        let mut hardware = FakeTouchBar::new();
        assert!(coordinator.start(&mut hardware, 0.0).is_err());
        assert_eq!(coordinator.report().frames.len(), 1);
        assert!(!coordinator.report().frames[0].success);
        assert!(coordinator.report().failure.is_some());
        assert!(coordinator.start(&mut hardware, 0.1).is_err());
        assert_eq!(coordinator.report().frames.len(), 1);
        drop(coordinator);
        let raw = collector.snapshot()?;
        assert!(raw.report.failed_operations > 0);
        assert!(raw.report.records.iter().any(|r| matches!(
            r.kind,
            EventKind::FixtureObserved {
                kind: ObservationKind::Resolved {
                    resolution: Resolution::Failed,
                    ..
                },
                ..
            }
        )));
        assert!(!raw.report.records.iter().any(|r| matches!(
            r.kind,
            EventKind::FixtureObserved {
                kind: ObservationKind::Closed,
                ..
            }
        )));
        assert!(timing.close().failed_spans >= 1);
        Ok(())
    }

    #[test]
    fn private_process_coordinator_complete_host_clock_lifecycle_all_modes() -> Result<()> {
        // Actual P1 phase lengths: no synthetic clock and no shortened 60+2 s
        // interval. Three software children share an epoch to keep suite cost
        // bounded. This is NOT a sequential overhead battery or native capture.
        let epoch = clock_ns(false)? + 8_000_000_000;
        let origin = Instant::now();
        let mut runs = Vec::new();
        for mode in [Mode::A, Mode::B, Mode::C] {
            let (mut coordinator, collector, timing) = child(mode, epoch, 65_536)?;
            let capture = Capture::new(16_384)?;
            let mut fake = FakeTouchBar::new();
            fake.claim()?;
            let mut hardware =
                ObservedHardware::new(fake, (mode == Mode::C).then(|| capture.clone()), None);
            coordinator.start(&mut hardware, origin.elapsed().as_secs_f64())?;
            runs.push((coordinator, collector, timing, hardware, capture));
        }
        let end_ns = epoch + 62_000_000_000;
        while clock_ns(false)? < end_ns {
            let mut next = origin.elapsed().as_secs_f64() + 0.05;
            for (coordinator, _, _, hardware, _) in &mut runs {
                let now = clock_ns(false)?;
                if coordinator.report().warmup_closed_ns.is_none()
                    && now
                        >= coordinator.report().frames[0]
                            .correlation
                            .allocation
                            .allocated_ns
                            + WARMUP_NS
                {
                    coordinator.confirm_warmup_closed()?;
                }
                if let Some(deadline) =
                    coordinator.drive(hardware, origin.elapsed().as_secs_f64(), &[])?
                {
                    next = next.min(deadline);
                }
            }
            std::thread::sleep(Duration::from_secs_f64(
                (next - origin.elapsed().as_secs_f64()).clamp(0.002, 0.05),
            ));
        }
        for (mut coordinator, collector, timing, mut hardware, capture) in runs {
            coordinator.finish()?;
            let report = coordinator.report();
            assert!(report.failure.is_none());
            let warmup = report.warmup_closed_ns.unwrap();
            assert!(
                warmup >= report.frames[0].correlation.allocation.allocated_ns + WARMUP_NS
                    && warmup < epoch
            );
            assert!(report.fixture_closed_ns.unwrap() >= end_ns);
            assert!(report.worker_stopped_ns.unwrap() >= report.fixture_closed_ns.unwrap());
            assert!(report.frames.iter().any(|r| r.cohort == Cohort::Measured));
            assert!(!report.frames.iter().any(|r| r.cohort == Cohort::Drain));
            assert!(report
                .frames
                .iter()
                .all(|r| r.success && r.adapter_return_ns <= end_ns));
            let raw = collector.snapshot()?;
            assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
            assert_eq!(raw.report.lost, 0);
            assert_eq!(raw.report.after_close, 0);
            assert_eq!(raw.report.clock_failures, 0);
            assert_eq!(raw.report.failed_operations, 0);
            assert_eq!(raw.report.decode_failures, 0);
            let summaries: Vec<_> = raw
                .report
                .records
                .iter()
                .filter_map(|r| match &r.kind {
                    EventKind::MinimalSummary(s) => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(summaries.len(), 1);
            assert_eq!(summaries[0].open_spans, 0);
            assert_eq!(summaries[0].lost, 0);
            assert_eq!(summaries[0].failed_spans, 0);
            assert_eq!(summaries[0].after_close, 0);
            assert_eq!(summaries[0].clock_failures, 0);
            assert!(summaries[0].closed_at_ns.is_some());
            assert!(raw
                .report
                .records
                .iter()
                .any(|r| matches!(r.kind, EventKind::MinimalCalibration { .. })));
            if coordinator.plan.mode == Mode::C {
                assert_eq!(
                    raw.report
                        .records
                        .iter()
                        .filter(|r| matches!(
                            r.kind,
                            EventKind::FixtureObserved {
                                kind: ObservationKind::Closed,
                                synthetic: false,
                                ..
                            }
                        ))
                        .count(),
                    1
                );
            }
            let minimal = timing.close();
            assert_eq!(minimal.lost, 0);
            assert_eq!(minimal.failed_spans, 0);
            assert_eq!(minimal.open_spans, 0);
            assert_eq!(minimal.after_close, 0);
            assert_eq!(minimal.clock_failures, 0);
            assert!(minimal.closed && minimal.closed_at_ns.is_some());
            assert!(minimal.counter_cost_samples_ns.is_some());
            let calibration: Vec<_> = minimal
                .records
                .iter()
                .filter(|s| s.kind == SpanKind::Calibration)
                .collect();
            assert_eq!(calibration.len(), 32);
            assert!(calibration
                .iter()
                .all(|s| s.status == SpanStatus::Succeeded && !s.frame_bearing));
            assert_eq!(
                minimal
                    .records
                    .iter()
                    .filter(|s| s.kind == SpanKind::Present)
                    .count(),
                report.frames.len()
            );
            for frame in &report.frames {
                let sample = minimal
                    .records
                    .iter()
                    .find(|s| s.span_id == frame.drive_span)
                    .unwrap();
                assert_eq!(sample.kind, SpanKind::SupervisorDrive);
                assert!(sample.frame_bearing && sample.status == SpanStatus::Succeeded);
                assert!(sample.start_ns <= frame.correlation.allocation.allocated_ns);
                assert!(sample.end_ns >= frame.adapter_return_ns);
                let present = minimal
                    .records
                    .iter()
                    .find(|s| s.span_id == frame.present_span)
                    .unwrap();
                assert_eq!(present.kind, SpanKind::Present);
                assert!(present.frame_bearing && present.status == SpanStatus::Succeeded);
                assert!(present.start_ns >= sample.start_ns && present.end_ns <= sample.end_ns);
                assert!(
                    present.start_ns <= frame.adapter_return_ns
                        && present.end_ns >= frame.adapter_return_ns
                );
            }
            let observed = capture.close();
            assert_eq!(observed.lost, 0);
            assert_eq!(observed.failed_operations, 0);
            assert_eq!(observed.decode_failures, 0);
            assert_eq!(observed.after_close, 0);
            assert_eq!(observed.clock_failures, 0);
            assert!(observed.closed);
            hardware.release()?;
        }
        Ok(())
    }
}
