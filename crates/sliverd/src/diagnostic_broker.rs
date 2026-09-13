//! Private direct-adapter broker source. The caller, not these writable records,
//! must authenticate the process/build and bind the plan before construction.
//! Installed services do not acquire or arm this path yet.
#![allow(dead_code)]

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, ensure, Result};

use crate::diagnostic_capture_transport::{FixtureBootstrap, MappedWriter, SourceRole};
use crate::diagnostic_fixture::Mode;
use crate::diagnostic_observer::{clock_ns, decode, Capture, EventKind};
use crate::diagnostic_timing::{SpanKind, TimingCapture};
use crate::hardware::{HardwareEvent, LogicalFrame, TouchBarHardware};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Operation {
    Claim = 0,
    Reacquire = 1,
    ConfirmOwner = 2,
    EmitKeys = 3,
    GetBacklight = 4,
    SetBacklight = 5,
    Release = 6,
    PlanIdentity = 7,
    SourceClosed = 8,
    SourceAbandoned = 9,
    AfterClose = 10,
}
impl Operation {
    pub(crate) fn from_wire(value: u8) -> Result<Self> {
        Ok(match value {
            0 => Self::Claim,
            1 => Self::Reacquire,
            2 => Self::ConfirmOwner,
            3 => Self::EmitKeys,
            4 => Self::GetBacklight,
            5 => Self::SetBacklight,
            6 => Self::Release,
            7 => Self::PlanIdentity,
            8 => Self::SourceClosed,
            9 => Self::SourceAbandoned,
            10 => Self::AfterClose,
            _ => bail!("unknown broker observation operation"),
        })
    }
}

struct State {
    plan: FixtureBootstrap,
    raw: Capture,
    timing: TimingCapture,
    calls: u64,
    closed: bool,
}
impl State {
    fn note(&self, operation: Operation, success: bool) {
        self.raw
            .record(EventKind::BrokerOperation { operation, success });
    }
    fn activity(&self) {
        if self.closed {
            // Never silently disarm and hide subsequent activity. Recording into
            // closed storage preserves attempted/loss/after-close counters.
            self.note(Operation::AfterClose, false);
        }
    }
    fn finish(&mut self, explicit: bool) -> Result<()> {
        if self.closed {
            return self.raw.ensure_closed_and_healthy();
        }
        let minimal = self.timing.close();
        let healthy = explicit
            && self.raw.ensure_open_and_healthy().is_ok()
            && minimal.open_spans == 0
            && minimal.failed_spans == 0
            && minimal.lost == 0
            && minimal.after_close == 0
            && minimal.clock_failures == 0
            && minimal.closed_at_ns.is_some()
            && minimal.counter_cost_samples_ns.is_some();
        self.note(
            if explicit {
                Operation::SourceClosed
            } else {
                Operation::SourceAbandoned
            },
            healthy,
        );
        // Check the terminal append too: a summary or closure that overflows
        // cannot establish a clean source even when all work fitted.
        let result = self.raw.ensure_open_and_healthy();
        self.raw.finish();
        self.closed = true;
        result?;
        ensure!(healthy, "broker source incomplete or unhealthy");
        self.raw.ensure_closed_and_healthy()
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.finish(false);
        }
    }
}

/// A single serialized source shared with its lifecycle owner. Finish only after
/// the broker producer has stopped/joined; it is source closure, not cohort or
/// device cleanup proof. Last-handle drop without finish records abandonment.
#[derive(Clone)]
pub(crate) struct BrokerCapture(Arc<Mutex<State>>);
impl BrokerCapture {
    pub(crate) fn new(plan: FixtureBootstrap, storage: OwnedFd) -> Result<Self> {
        Self::from_writer(
            plan,
            MappedWriter::receive_for_role(storage, SourceRole::Broker)?,
        )
    }

    pub(crate) fn from_startup(
        startup: crate::diagnostic_provisioning::StartupCapture,
    ) -> Result<Self> {
        ensure!(
            startup.binding().role() == SourceRole::Broker,
            "startup capture is not broker-bound"
        );
        let plan = startup.plan().clone();
        let (writer, _) = startup.into_writer();
        Self::from_writer(plan, writer)
    }

    fn from_writer(plan: FixtureBootstrap, writer: MappedWriter) -> Result<Self> {
        ensure!(
            writer.role() == SourceRole::Broker,
            "writer is not broker-bound"
        );
        plan.plan()?;
        ensure!(
            clock_ns(false)? < plan.start_ns,
            "broker capture epoch is not future"
        );
        let raw = Capture::from_mapped(writer)?;
        let timing = TimingCapture::exporting(65_536, raw.clone())?;
        let source = Self(Arc::new(Mutex::new(State {
            plan,
            raw,
            timing,
            calls: 0,
            closed: false,
        })));
        {
            let state = source.lock();
            state.raw.record(EventKind::BrokerConfigured {
                mode: state.plan.mode,
                start_ns: state.plan.start_ns,
                rate: state.plan.rate,
                causal: state.plan.causal,
            });
            state.timing.calibrate()?;
            ensure!(
                clock_ns(false)? < state.plan.start_ns,
                "broker calibration reached declared epoch"
            );
        }
        Ok(source)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let state = poisoned.into_inner();
                state.note(Operation::SourceAbandoned, false);
                state
            }
        }
    }

    pub(crate) fn operation<T>(
        &self,
        operation: Operation,
        perform: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let state = self.lock();
        state.activity();
        let result = perform();
        state.note(operation, result.is_ok());
        result
    }

    pub(crate) fn present<H: TouchBarHardware>(
        &self,
        hardware: &mut H,
        frame: &LogicalFrame,
    ) -> Result<()> {
        let mut state = self.lock();
        state.activity();
        state.calls = state.calls.saturating_add(1);
        let call = state.calls;
        if state.plan.mode == Mode::C {
            let entered = clock_ns(false);
            let marker = decode(frame);
            if let Ok(marker) = &marker {
                if marker.run_id() != state.plan.run.as_bytes()
                    || marker.generation() != state.plan.generation.as_bytes()
                {
                    state.note(Operation::PlanIdentity, false);
                }
            }
            state
                .raw
                .record_at(entered, EventKind::PresentEntered { call, marker });
        }
        // Only the complete supplied hardware adapter operation belongs to this
        // minimal span. Decode/logging is nested in the supervisor's full drive,
        // not secretly charged to bare hardware-call duration.
        let span = state.timing.begin(SpanKind::Present);
        let result = hardware.present(frame);
        let returned = clock_ns(false); // BEFORE recorder work or the IPC reply.
        span.finish_at(
            returned
                .as_ref()
                .copied()
                .map_err(|_| anyhow::anyhow!("broker clock read failed")),
            true,
            result.is_ok(),
        );
        if state.plan.mode == Mode::C {
            state.raw.record_at(
                returned,
                EventKind::PresentReturned {
                    call,
                    success: result.is_ok(),
                },
            );
        }
        // Observation failure must not suppress actual hardware errors, invent a
        // successful call, or prevent subsequent release/fencing cleanup.
        result
    }

    pub(crate) fn poll<H: TouchBarHardware>(
        &self,
        hardware: &mut H,
        timeout: Duration,
    ) -> Result<Vec<HardwareEvent>> {
        let state = self.lock();
        state.activity();
        // All modes retain unexpected-input/error evidence. Empty polls append
        // nothing. Every event in a batch has the same actual receipt reading;
        // original order is the raw source sequence, not a guessed timestamp tie.
        state.raw.poll(hardware, timeout)
    }

    pub(crate) fn finish(&self) -> Result<()> {
        self.lock().finish(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic_capture_transport::Collector;
    use crate::diagnostic_hardware::ObservedHardware;
    use crate::diagnostic_timing::SpanStatus;
    use crate::hardware::{FakeTouchBar, ModifierState, TouchEvent, TouchPhase};

    fn source(mode: Mode, capacity: usize) -> Result<(BrokerCapture, Collector)> {
        let mut collector = Collector::for_role(capacity, SourceRole::Broker)?;
        let plan = FixtureBootstrap::new(
            "broker-test",
            "child",
            clock_ns(false)? + 10_000_000_000,
            30,
            mode,
        )?;
        let source = BrokerCapture::new(plan, collector.take_storage(SourceRole::Broker)?)?;
        Ok((source, collector))
    }
    fn frame() -> Result<LogicalFrame> {
        crate::frame_canvas::FrameCanvas::new()?.finish()
    }

    #[test]
    fn private_broker_source_all_modes_retain_actual_errors_and_all_normalized_input() -> Result<()>
    {
        for mode in [Mode::A, Mode::B, Mode::C] {
            let (source, collector) = source(mode, 256)?;
            let mut fake = FakeTouchBar::new();
            let mut expected = Vec::new();
            for phase in [TouchPhase::Down, TouchPhase::Move, TouchPhase::Up] {
                let event = HardwareEvent::Touch(TouchEvent {
                    phase,
                    id: 7,
                    time: 0.25,
                    x: 10.0,
                    y: 20.0,
                    modifiers: ModifierState::default(),
                    pressure: None,
                    width: None,
                    height: None,
                });
                fake.inject(event);
                expected.push(event);
            }
            fake.inject(HardwareEvent::Fn { active: true });
            expected.push(HardwareEvent::Fn { active: true });
            let mut hardware = ObservedHardware::for_broker(fake, source.clone());
            assert!(hardware
                .present(&frame()?)
                .unwrap_err()
                .to_string()
                .contains("not claimed"));
            assert!(hardware.poll(Duration::ZERO).is_err());
            hardware.claim()?;
            assert_eq!(hardware.poll(Duration::ZERO)?, expected);
            hardware.present(&frame()?)?;
            hardware.release()?;
            drop(hardware);
            assert!(source.finish().is_err());
            let raw = collector.snapshot()?;
            assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
            assert_eq!(raw.report.lost, 0);
            let receipts: Vec<_> = raw
                .report
                .records
                .iter()
                .filter_map(|r| match r.kind {
                    EventKind::InputReceived(event) => Some((r.at_ns, event)),
                    _ => None,
                })
                .collect();
            assert_eq!(receipts.iter().map(|r| r.1).collect::<Vec<_>>(), expected);
            assert!(receipts.windows(2).all(|w| w[0].0 == w[1].0));
            let samples: Vec<_> = raw
                .report
                .records
                .iter()
                .filter_map(|r| match &r.kind {
                    EventKind::MinimalSpan(s) if s.kind == SpanKind::Present => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(samples.len(), 2);
            assert_eq!(samples[0].status, SpanStatus::Failed);
            assert_eq!(samples[1].status, SpanStatus::Succeeded);
            assert!(samples.iter().all(|s| s.start_ns <= s.end_ns));
            assert_eq!(
                raw.report.decode_failures,
                if mode == Mode::C { 2 } else { 0 }
            );
            if mode == Mode::C {
                let returns: Vec<_> = raw
                    .report
                    .records
                    .iter()
                    .filter(|r| matches!(r.kind, EventKind::PresentReturned { .. }))
                    .collect();
                assert_eq!(returns.len(), 2);
                for (sample, returned) in samples.iter().zip(returns) {
                    assert_eq!(
                        sample.end_ns, returned.at_ns,
                        "hardware duration and endpoint must use the exact same return reading"
                    );
                }
            } else {
                assert!(!raw.report.records.iter().any(|r| matches!(
                    r.kind,
                    EventKind::PresentEntered { .. } | EventKind::PresentReturned { .. }
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn private_broker_source_panicked_operation_cannot_be_closed_as_healthy() -> Result<()> {
        let (source, collector) = source(Mode::A, 128)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<()> =
                source.operation(Operation::Claim, || panic!("injected adapter panic"));
        }));
        assert!(result.is_err());
        assert!(
            source.finish().is_err(),
            "a panicked source operation disappeared at closure"
        );
        assert!(collector.snapshot()?.report.failed_operations > 0);
        Ok(())
    }

    #[test]
    fn private_broker_source_closure_is_idempotent_and_never_silently_disarms() -> Result<()> {
        let (source, collector) = source(Mode::A, 128)?;
        let mut hardware = ObservedHardware::for_broker(FakeTouchBar::new(), source.clone());
        hardware.claim()?;
        hardware.present(&frame()?)?;
        hardware.release()?;
        source.finish()?;
        let first = collector.snapshot()?.report;
        source.finish()?;
        assert_eq!(
            collector.snapshot()?.report.attempted_records,
            first.attempted_records
        );
        // Even an empty poll after closure must not vanish. Normal cleanup/error
        // semantics remain delegated rather than being blocked by the observer.
        assert!(hardware.poll(Duration::ZERO).is_err());
        assert!(source.finish().is_err());
        let after = collector.snapshot()?.report;
        assert!(after.after_close > 0 && after.lost > 0);
        assert_eq!(after.closed_at_ns, first.closed_at_ns);
        Ok(())
    }

    #[test]
    fn private_broker_source_abandonment_and_setup_or_terminal_overflow_cannot_close_cleanly(
    ) -> Result<()> {
        let (source, collector) = source(Mode::A, 128)?;
        drop(source);
        let report = collector.snapshot()?.report;
        assert!(report.closed && report.failed_operations > 0);
        assert!(report.records.iter().any(|r| matches!(
            r.kind,
            EventKind::BrokerOperation {
                operation: Operation::SourceAbandoned,
                success: false,
            }
        )));
        // Config + 32 probes + aggregate fit in 34 records. Closing requires
        // both a summary and terminal record; neither may silently overflow.
        for capacity in [1, 33, 34, 35] {
            let mut collector = Collector::for_role(capacity, SourceRole::Broker)?;
            let plan = FixtureBootstrap::new(
                "bounded",
                "child",
                clock_ns(false)? + 10_000_000_000,
                30,
                Mode::A,
            )?;
            if let Ok(source) =
                BrokerCapture::new(plan, collector.take_storage(SourceRole::Broker)?)
            {
                assert!(source.finish().is_err());
            }
            let raw = collector.snapshot()?;
            assert!(raw.report.lost > 0);
        }
        Ok(())
    }
}
