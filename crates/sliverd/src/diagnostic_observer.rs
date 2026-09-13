//! Private experimental userspace observation, not a P1 collector or evaluator.
//!
//! Authority is an explicitly supplied Capture, never an environment variable,
//! Lua API, or broker payload. No service currently provisions that authority.
//! Records are fixed-size, preallocated, and retained in memory until explicit
//! closure. This module performs no filesystem IO. Marker CRC is not provenance.
//!
//! Scope: the Lua/canvas seam (including real worker bootstrap provisioned only
//! by private tests), shared-frame handoff and explicitly wrapped hardware calls.
//! diagnostic_capture_transport supplies bounded mapped raw export. No production
//! arming, native fixture scheduler, lifecycle/provenance verifier or release
//! artifact exporter is provided. Minimal callback/drive timing lives separately
//! in diagnostic_timing so detailed observation is not required in modes A/B.
//! Existing production constructors leave observation off. Publication is sampled
//! immediately before releasing READY, selection after acquiring READING, render
//! completion after snapshot finishing/decoding, and present return immediately
//! after the complete adapter call. These are not optical timestamps or the
//! overhead protocol's minimal callback/drive durations.
//!
//! Timer records observe the existing registry and real Lua callback path:
//! registration, deadline activation, cancellation (including no-op attempts),
//! dispatch, callback outcome and post-callback advancement. Redraw records
//! distinguish setting the pending bit from coalescing into an existing request.
//! Timer IDs are worker-local. Raw f64 scheduler values may round or overflow;
//! they are never reinterpreted as CLOCK_MONOTONIC event timestamps, normalized
//! into rational periods, or assigned scheduled-opportunity credit here. A
//! callback's error and its registry disposition are separate facts. Teardown
//! and cross-worker identity/semantic work closure still require an assembler.
//!
//! A recorder retains at most capacity * record_bytes (currently <=32 MiB), plus
//! fixed metadata. Marker snapshots are transient complete frame copies at slot
//! publication; no pixels are retained in records. Mapped storage replaces the
//! heap record vector rather than mirroring it. finish() closes metadata without
//! copying records, after sources stop; close() materializes a bounded report for
//! callers explicitly requesting one. Storage closure is not operation success.
//! Worker load/control-loop failures are separate raw events. Mutex contention,
//! copying, decoding and recorder overhead have not been qualified on hardware.
#![allow(dead_code)] // Private provisioning currently exercised only at the test seam.

use std::sync::{Arc, Mutex};

use anyhow::{ensure, Context, Result};

use crate::hardware::{HardwareEvent, LogicalFrame, TouchBarHardware, TouchEvent};

const MAX_RECORDS: usize = 65_536;
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Marker([u8; 408]);

impl Marker {
    pub(crate) fn bytes(&self) -> &[u8; 408] {
        &self.0
    }

    pub(crate) fn run_id(&self) -> &[u8] {
        &self.0[20..20 + self.0[16] as usize]
    }
    pub(crate) fn generation(&self) -> &[u8] {
        &self.0[148..148 + self.0[17] as usize]
    }
    pub(crate) fn input_id(&self) -> Option<&[u8]> {
        (self.0[18] != 0).then(|| &self.0[276..276 + self.0[18] as usize])
    }
    pub(crate) fn frame_id(&self) -> u64 {
        u64::from_be_bytes(self.0[8..16].try_into().expect("fixed packet"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecodeError {
    Geometry,
    Cell,
    Packet,
}

/// Read only a complete, owned logical snapshot; no shared live mmap or label
/// can supply identity. Every pixel in every cell must agree exactly.
pub(crate) fn decode(frame: &LogicalFrame) -> std::result::Result<Marker, DecodeError> {
    if frame.width() != 2008
        || frame.height() != 60
        || frame.stride() < 8032
        || !frame.stride().is_multiple_of(4)
        || frame.pixels().len() > MAX_FRAME_BYTES
        || frame.stride().checked_mul(60) != Some(frame.pixels().len())
    {
        return Err(DecodeError::Geometry);
    }
    let mut bytes = [0_u8; 408];
    for bit in 0..3264 {
        let x = 188 + bit % 816 * 2;
        let y = 4 + bit / 816 * 2;
        let offset = y * frame.stride() + x * 4;
        let cell = &frame.pixels()[offset..offset + 4];
        let white = match cell {
            [0, 0, 0, 255] => false,
            [255, 255, 255, 255] => true,
            _ => return Err(DecodeError::Cell),
        };
        for dy in 0..2 {
            for dx in 0..2 {
                let offset = (y + dy) * frame.stride() + (x + dx) * 4;
                if &frame.pixels()[offset..offset + 4] != cell {
                    return Err(DecodeError::Cell);
                }
            }
        }
        if white {
            bytes[bit / 8] |= 128 >> (bit % 8);
        }
    }
    decode_packet(bytes)
}

/// Validate the same packet when reading explicitly encoded raw export records.
/// This validates digital integrity only, not source authentication.
pub(crate) fn decode_packet(bytes: [u8; 408]) -> std::result::Result<Marker, DecodeError> {
    let marker = Marker(bytes);
    if &bytes[..8] != b"SLVMRK00"
        || bytes[19] != 0
        || !(1..=i64::MAX as u64).contains(&marker.frame_id())
    {
        return Err(DecodeError::Packet);
    }
    for (start, length, nullable) in [
        (20, bytes[16], false),
        (148, bytes[17], false),
        (276, bytes[18], true),
    ] {
        let length = length as usize;
        if length > 128
            || (!nullable && length == 0)
            || !bytes[start..start + length]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(b))
            || bytes[start + length..start + 128].iter().any(|b| *b != 0)
        {
            return Err(DecodeError::Packet);
        }
    }
    let mut crc = !0_u32;
    for byte in &bytes[..404] {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    if !crc != u32::from_be_bytes(bytes[404..].try_into().expect("fixed CRC")) {
        return Err(DecodeError::Packet);
    }
    Ok(marker)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiscardReason {
    ProducerReclaim,
    ConsumerSuperseded,
    MappingClosed,
}

/// Result of the existing scheduler's post-callback bookkeeping, including on
/// callback error. Rescheduled does not promise a later dispatch: the worker may
/// fail or stop. Float advancement is raw scheduler arithmetic, not an exact
/// integer opportunity count; fallback/overflow must not be hidden by conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TimerDisposition {
    Completed,
    Cancelled,
    Rescheduled {
        next_deadline_seconds: f64,
        intervals_advanced: f64,
        skipped_intervals: f64,
        fallback: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum EventKind {
    BrokerConfigured {
        mode: crate::diagnostic_fixture::Mode,
        start_ns: u64,
        rate: u32,
        causal: bool,
    },
    BrokerOperation {
        operation: crate::diagnostic_broker::Operation,
        success: bool,
    },
    MinimalSpan(crate::diagnostic_timing::Sample),
    MinimalSummary(crate::diagnostic_timing::Summary),
    /// Explicit no-op sampler calibration costs, never work credit or an amount
    /// to subtract from measured durations. The calibrator supplies all samples.
    MinimalCalibration {
        cost_ns: [u64; 32],
    },
    /// Fixture decision-clock values remain distinct from Record::at_ns.
    /// Neither the synthetic flag nor these observations authenticate a source.
    FixtureObserved {
        synthetic: bool,
        decision_ns: u64,
        kind: crate::diagnostic_fixture::ObservationKind,
    },
    /// Runtime::load returned an error. Storage closure is not startup success;
    /// retain this fact even if its ordinary READY error reply cannot arrive.
    WorkerLoadFailed,
    /// The actual worker control loop returned an error (e.g. peer EOF).
    WorkerLoopFailed,
    /// handle_command returned an error, even when the loop subsequently exits
    /// normally. May duplicate a more detailed callback failure observation.
    WorkerCommandFailed,
    /// The bootstrapped worker helper returned Err, including setup/READY
    /// errors before entering the control loop. May duplicate a specific error.
    WorkerRunFailed,
    /// Actual private bootstrap recording level, observed before Runtime::load.
    /// This is not marker mode or authenticated source/build provenance.
    WorkerCaptureConfigured {
        detailed: bool,
    },
    // Timer IDs are local to one worker registry, not capture-wide identities.
    // All *_seconds fields preserve scheduler f64 values verbatim. They are
    // NOT host-clock readings, rational periods, or policy opportunity credit.
    TimerRegistered {
        timer_id: u64,
        delay_seconds: f64,
        interval_seconds: Option<f64>,
        scheduler_now_seconds: Option<f64>,
    },
    TimerActivated {
        timer_id: u64,
        scheduler_now_seconds: f64,
        deadline_seconds: f64,
    },
    TimerCancelled {
        timer_id: u64,
        removed: bool,
    },
    TimerDeadlineShifted {
        timer_id: u64,
        previous_deadline_seconds: f64,
        shift_seconds: f64,
        deadline_seconds: f64,
    },
    TimerDispatched {
        timer_id: u64,
        scheduled_deadline_seconds: f64,
        scheduler_now_seconds: f64,
    },
    TimerFinished {
        timer_id: u64,
        success: bool,
        scheduler_now_seconds: f64,
        disposition: TimerDisposition,
    },
    RedrawRequested {
        coalesced: bool,
    },
    RenderAllocated {
        attempt: u64,
    },
    RenderFinished {
        attempt: u64,
        marker: Option<std::result::Result<Marker, DecodeError>>,
    },
    Published {
        sequence: u64,
        allocation: Option<crate::frame_slots::FrameAllocation>,
        marker: std::result::Result<Marker, DecodeError>,
    },
    Selected {
        sequence: u64,
    },
    Discarded {
        sequence: u64,
        reason: DiscardReason,
    },
    PendingDiscarded {
        allocation: Option<crate::frame_slots::FrameAllocation>,
        marker: std::result::Result<Marker, DecodeError>,
    },
    PresentEntered {
        call: u64,
        marker: std::result::Result<Marker, DecodeError>,
    },
    PresentReturned {
        call: u64,
        success: bool,
    },
    InputReceived(HardwareEvent),
    PollFailed,
    TouchCallbackEntered(TouchEvent),
    TouchCallbackReturned {
        contact: u32,
        success: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub(crate) sequence: u64,
    pub(crate) at_ns: u64,
    pub(crate) kind: EventKind,
}

#[derive(Clone, Debug)]
pub(crate) struct Report {
    pub(crate) records: Vec<Record>,
    pub(crate) capacity: usize,
    pub(crate) attempted_records: u64,
    pub(crate) lost: u64,
    pub(crate) clock_failures: u64,
    /// Failed observations, not unique failed operations: a command/run failure
    /// can also have a detailed callback failure and a failed/abandoned minimal
    /// span. Fixture Failed and Resolved(Failed) each count as observations,
    /// regardless of the synthetic flag. Repeated minimal summaries do not add
    /// new failure observations.
    pub(crate) failed_operations: u64,
    /// Failed decode observations, not unique frames (one frame crosses seams).
    pub(crate) decode_failures: u64,
    pub(crate) after_close: u64,
    pub(crate) closed: bool,
    pub(crate) closed_at_ns: Option<u64>,
    pub(crate) clock_resolution_ns: u64,
    pub(crate) record_bytes: usize,
    /// Consecutive clock-read intervals sampled at allocation, including loop
    /// and timestamp placement cost. Diagnostic only; never subtracted.
    pub(crate) clock_read_samples_ns: [u64; 32],
}

struct State {
    report: Report,
    renders: u64,
    calls: u64,
    stored: usize,
    mapped: Option<crate::diagnostic_capture_transport::MappedWriter>,
}

#[derive(Clone)]
pub(crate) struct Capture(Arc<Mutex<State>>);

pub(crate) fn clock_ns(resolution: bool) -> Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let status = unsafe {
        if resolution {
            libc::clock_getres(libc::CLOCK_MONOTONIC, &mut time)
        } else {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time)
        }
    };
    ensure!(status == 0, "reading diagnostic CLOCK_MONOTONIC failed");
    ensure!(
        time.tv_sec >= 0 && (0..1_000_000_000).contains(&time.tv_nsec),
        "invalid monotonic time"
    );
    (time.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|s| s.checked_add(time.tv_nsec as u64))
        .context("monotonic nanoseconds overflow")
}

impl Capture {
    pub(crate) fn new(capacity: usize) -> Result<Self> {
        Self::new_storage(capacity, None)
    }

    pub(crate) fn from_mapped(
        storage: crate::diagnostic_capture_transport::MappedWriter,
    ) -> Result<Self> {
        Self::new_storage(storage.capacity(), Some(storage))
    }

    fn new_storage(
        capacity: usize,
        mapped: Option<crate::diagnostic_capture_transport::MappedWriter>,
    ) -> Result<Self> {
        ensure!(
            (1..=MAX_RECORDS).contains(&capacity),
            "diagnostic capacity outside 1..=65536"
        );
        let mut records = Vec::new();
        if mapped.is_none() {
            records.try_reserve_exact(capacity)?;
        }
        let mut clock_read_samples_ns = [0; 32];
        for sample in &mut clock_read_samples_ns {
            let start = clock_ns(false)?;
            *sample = clock_ns(false)?
                .checked_sub(start)
                .context("monotonic clock regressed")?;
        }
        let capture = Self(Arc::new(Mutex::new(State {
            report: Report {
                records,
                capacity,
                attempted_records: 0,
                lost: 0,
                clock_failures: 0,
                failed_operations: 0,
                decode_failures: 0,
                after_close: 0,
                closed: false,
                closed_at_ns: None,
                clock_resolution_ns: clock_ns(true)?,
                record_bytes: if mapped.is_some() {
                    crate::diagnostic_capture_transport::RECORD_BYTES
                } else {
                    std::mem::size_of::<Record>()
                },
                clock_read_samples_ns,
            },
            renders: 0,
            calls: 0,
            stored: 0,
            mapped,
        })));
        {
            let mut state = capture
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let State { report, mapped, .. } = &mut *state;
            if let Some(mapped) = mapped {
                mapped.update_metadata(report);
            }
        }
        Ok(capture)
    }

    /// Check the shared source without closing it or copying its records. A
    /// healthy snapshot is not a promise about subsequent writes or provenance.
    pub(crate) fn ensure_open_and_healthy(&self) -> Result<()> {
        self.ensure_health(false)
    }

    pub(crate) fn ensure_closed_and_healthy(&self) -> Result<()> {
        self.ensure_health(true)
    }

    fn ensure_health(&self, closed: bool) -> Result<()> {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let report = &state.report;
        ensure!(
            report.closed == closed
                && (!closed || report.closed_at_ns.is_some())
                && report.lost == 0
                && report.clock_failures == 0
                && report.failed_operations == 0
                && report.decode_failures == 0
                && report.after_close == 0,
            "raw diagnostic source has wrong closure state or errors/loss"
        );
        Ok(())
    }

    pub(crate) fn monotonic_ns(&self) -> Result<u64> {
        clock_ns(false)
    }

    pub(crate) fn record(&self, kind: EventKind) {
        // Sample at the seam, not after waiting for the recorder mutex.
        self.record_at(clock_ns(false), kind);
    }

    pub(crate) fn record_at(&self, time: Result<u64>, kind: EventKind) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let State {
            report: r,
            mapped,
            stored,
            ..
        } = &mut *state;
        r.attempted_records = r.attempted_records.saturating_add(1);
        match &kind {
            EventKind::RenderFinished { marker: None, .. }
            | EventKind::PresentReturned { success: false, .. }
            | EventKind::TouchCallbackReturned { success: false, .. }
            | EventKind::TimerFinished { success: false, .. }
            | EventKind::WorkerLoadFailed
            | EventKind::WorkerLoopFailed
            | EventKind::WorkerCommandFailed
            | EventKind::WorkerRunFailed
            | EventKind::PollFailed
            | EventKind::BrokerOperation { success: false, .. }
            | EventKind::FixtureObserved {
                kind:
                    crate::diagnostic_fixture::ObservationKind::Failed
                    | crate::diagnostic_fixture::ObservationKind::Resolved {
                        resolution: crate::diagnostic_fixture::Resolution::Failed,
                        ..
                    },
                ..
            } => r.failed_operations = r.failed_operations.saturating_add(1),
            EventKind::MinimalSpan(sample)
                if sample.status != crate::diagnostic_timing::SpanStatus::Succeeded =>
            {
                r.failed_operations = r.failed_operations.saturating_add(1);
            }
            // Summaries are repeatable snapshots, not new failures or clock
            // observations. Their counters stay separate instead of being summed.
            EventKind::RenderFinished {
                marker: Some(Err(_)),
                ..
            }
            | EventKind::Published { marker: Err(_), .. }
            | EventKind::PresentEntered { marker: Err(_), .. }
            | EventKind::PendingDiscarded { marker: Err(_), .. } => {
                r.decode_failures = r.decode_failures.saturating_add(1)
            }
            _ => {}
        }
        let at_ns = time.unwrap_or_else(|_| {
            r.clock_failures = r.clock_failures.saturating_add(1);
            0
        });
        if r.closed {
            r.after_close = r.after_close.saturating_add(1);
        }
        if r.closed || *stored == r.capacity {
            r.lost = r.lost.saturating_add(1);
        } else {
            let record = Record {
                sequence: r.attempted_records,
                at_ns,
                kind,
            };
            if let Some(mapped) = mapped {
                if mapped.append(&record).is_err() {
                    r.lost = r.lost.saturating_add(1);
                } else {
                    *stored += 1;
                }
            } else {
                r.records.push(record);
                *stored += 1;
            }
        }
        if let Some(mapped) = mapped {
            mapped.update_metadata(r);
        }
    }

    pub(crate) fn allocate_render(&self) -> u64 {
        let time = clock_ns(false);
        let attempt = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.renders = state.renders.saturating_add(1);
            state.renders
        };
        self.record_at(time, EventKind::RenderAllocated { attempt });
        attempt
    }

    /// Complete adapter operation, not ioctl count, optical presentation, or
    /// successful unique-frame credit. Repeated marker identities remain raw.
    pub(crate) fn present<H: TouchBarHardware>(
        &self,
        hardware: &mut H,
        frame: &LogicalFrame,
    ) -> Result<()> {
        let entered = clock_ns(false);
        let call = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.calls = state.calls.saturating_add(1);
            state.calls
        };
        // LogicalFrame already owns the complete immutable snapshot. Decode it
        // independently; the exact same bytes enter the adapter immediately below.
        self.record_at(
            entered,
            EventKind::PresentEntered {
                call,
                marker: decode(frame),
            },
        );
        let result = hardware.present(frame);
        self.record(EventKind::PresentReturned {
            call,
            success: result.is_ok(),
        });
        result
    }

    /// Receipt is immediately after the normalized adapter batch returns, not
    /// the evdev timestamp, contact's supplied time, or physical actuation.
    pub(crate) fn poll<H: TouchBarHardware>(
        &self,
        hardware: &mut H,
        timeout: std::time::Duration,
    ) -> Result<Vec<HardwareEvent>> {
        self.poll_with_clock(hardware, timeout, || clock_ns(false))
    }

    fn poll_with_clock<H: TouchBarHardware>(
        &self,
        hardware: &mut H,
        timeout: std::time::Duration,
        read_clock: impl FnOnce() -> Result<u64>,
    ) -> Result<Vec<HardwareEvent>> {
        let result = hardware.poll(timeout);
        // No receipt timestamp exists for an empty successful batch. Avoid a
        // clock read whose possible failure would otherwise vanish in the loop.
        if result.as_ref().is_ok_and(Vec::is_empty) {
            return result;
        }
        let received = read_clock();
        match &result {
            Ok(events) => {
                for event in events {
                    // All events were available on this batch return. Preserve
                    // batch order and the same observed acquisition instant.
                    self.record_at(
                        received
                            .as_ref()
                            .copied()
                            .map_err(|_| anyhow::anyhow!("clock read failed")),
                        EventKind::InputReceived(*event),
                    );
                }
            }
            Err(_) => self.record_at(received, EventKind::PollFailed),
        }
        result
    }

    /// Caller must stop/join all sources before closure. Closure describes only
    /// storage lifetime, NOT resolved work, cleanup success, or acceptance.
    /// Repeated snapshots expose any attempted writes after closure as loss.
    pub(crate) fn close(&self) -> Report {
        self.finish();
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mapped) = &state.mapped {
            mapped
                .snapshot()
                .expect("observer-owned raw encoding is valid")
                .report
        } else {
            state.report.clone()
        }
    }

    /// Close storage without allocating/copying records. The process worker uses
    /// this only after Runtime (including pending-frame producers) is dropped.
    pub(crate) fn finish(&self) {
        let time = clock_ns(false);
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.report.closed {
            state.report.closed = true;
            state.report.closed_at_ns = time.ok();
            if state.report.closed_at_ns.is_none() {
                state.report.clock_failures += 1;
            }
        }
        let State { report, mapped, .. } = &mut *state;
        if let Some(mapped) = mapped {
            mapped.update_metadata(report);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::FakeTouchBar;

    #[test]
    fn private_empty_poll_does_not_sample_and_discard_a_clock_failure() -> Result<()> {
        let source = Capture::new(8)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let events = source.poll_with_clock(&mut hardware, std::time::Duration::ZERO, || {
            panic!("empty poll must not sample an unrecorded clock")
        })?;
        assert!(events.is_empty());
        assert!(source.close().records.is_empty());
        hardware.release()?;
        Ok(())
    }

    #[test]
    fn private_nonempty_poll_clock_failure_is_retained_without_changing_hardware_result(
    ) -> Result<()> {
        let source = Capture::new(8)?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let event = HardwareEvent::Fn { active: true };
        hardware.inject(event);
        let events = source.poll_with_clock(&mut hardware, std::time::Duration::ZERO, || {
            anyhow::bail!("injected receipt clock failure")
        })?;
        assert_eq!(events, vec![event]);
        let report = source.close();
        assert_eq!(report.clock_failures, 1);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].at_ns, 0);
        assert!(matches!(report.records[0].kind, EventKind::InputReceived(e) if e == event));
        assert!(source.ensure_closed_and_healthy().is_err());
        hardware.release()?;
        Ok(())
    }
}
