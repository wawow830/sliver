//! Private bounded minimal timing common to P1 modes A/B/C. No markers, IO,
//! source authentication or release verdict. Provisioning remains explicit.
//! Spans sample CLOCK_MONOTONIC before recorder entry work and after the complete
//! operation. The final minimal record append itself is not in its duration;
//! detailed per-drive work must be nested INSIDE the supervisor drive span.
#![allow(dead_code)] // Private production provisioning is not yet installed.

use std::sync::{Arc, Mutex};

use anyhow::{ensure, Result};

use crate::diagnostic_observer::clock_ns;
use crate::hardware::{LogicalFrame, TouchBarHardware};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpanKind {
    RenderCallback,
    Present,
    SupervisorDrive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpanStatus {
    Succeeded,
    Failed,
    Abandoned,
}

#[derive(Clone, Debug)]
pub(crate) struct Sample {
    /// Completion-record sequence, independent of nested span allocation order.
    pub(crate) sequence: u64,
    pub(crate) span_id: u64,
    pub(crate) kind: SpanKind,
    pub(crate) start_ns: u64,
    pub(crate) end_ns: u64,
    pub(crate) frame_bearing: bool,
    pub(crate) status: SpanStatus,
}

#[derive(Clone, Debug)]
pub(crate) struct Report {
    pub(crate) records: Vec<Sample>,
    pub(crate) capacity: usize,
    pub(crate) started_spans: u64,
    pub(crate) completed_spans: u64,
    pub(crate) open_spans: u64,
    pub(crate) failed_spans: u64,
    pub(crate) lost: u64,
    pub(crate) after_close: u64,
    pub(crate) clock_failures: u64,
    pub(crate) closed_at_ns: Option<u64>,
    pub(crate) closed: bool,
    pub(crate) clock_resolution_ns: u64,
    pub(crate) record_bytes: usize,
    pub(crate) clock_read_samples_ns: [u64; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub(crate) capacity: usize,
    pub(crate) started_spans: u64,
    pub(crate) completed_spans: u64,
    pub(crate) open_spans: u64,
    pub(crate) failed_spans: u64,
    pub(crate) lost: u64,
    pub(crate) after_close: u64,
    pub(crate) clock_failures: u64,
    pub(crate) closed_at_ns: Option<u64>,
    pub(crate) clock_resolution_ns: u64,
    pub(crate) record_bytes: usize,
    pub(crate) clock_read_samples_ns: [u64; 32],
}

impl From<&Report> for Summary {
    fn from(report: &Report) -> Self {
        Self {
            capacity: report.capacity,
            started_spans: report.started_spans,
            completed_spans: report.completed_spans,
            open_spans: report.open_spans,
            failed_spans: report.failed_spans,
            lost: report.lost,
            after_close: report.after_close,
            clock_failures: report.clock_failures,
            closed_at_ns: report.closed_at_ns,
            clock_resolution_ns: report.clock_resolution_ns,
            record_bytes: report.record_bytes,
            clock_read_samples_ns: report.clock_read_samples_ns,
        }
    }
}

struct State {
    report: Report,
    export: Option<crate::diagnostic_observer::Capture>,
}

#[derive(Clone)]
pub(crate) struct TimingCapture(Arc<Mutex<State>>);

pub(crate) struct Span {
    capture: TimingCapture,
    sequence: u64,
    start_ns: u64,
    kind: SpanKind,
    finished: bool,
}

impl TimingCapture {
    pub(crate) fn new(capacity: usize) -> Result<Self> {
        Self::new_storage(capacity, None)
    }

    /// Shared raw source capacity/loss is authoritative in export mode. No
    /// second sample vector is allocated or flushed at source shutdown.
    pub(crate) fn exporting(
        capacity: usize,
        capture: crate::diagnostic_observer::Capture,
    ) -> Result<Self> {
        Self::new_storage(capacity, Some(capture))
    }

    fn new_storage(
        capacity: usize,
        export: Option<crate::diagnostic_observer::Capture>,
    ) -> Result<Self> {
        ensure!(
            (1..=65_536).contains(&capacity),
            "timing capacity outside 1..=65536"
        );
        let mut records = Vec::new();
        if export.is_none() {
            records.try_reserve_exact(capacity)?;
        }
        let mut clock_read_samples_ns = [0; 32];
        for sample in &mut clock_read_samples_ns {
            let start = clock_ns(false)?;
            let end = clock_ns(false)?;
            ensure!(end >= start, "monotonic clock regressed");
            *sample = end - start;
        }
        Ok(Self(Arc::new(Mutex::new(State {
            export,
            report: Report {
                records,
                capacity,
                started_spans: 0,
                completed_spans: 0,
                open_spans: 0,
                failed_spans: 0,
                lost: 0,
                after_close: 0,
                clock_failures: 0,
                closed_at_ns: None,
                closed: false,
                clock_resolution_ns: clock_ns(true)?,
                record_bytes: std::mem::size_of::<Sample>(),
                clock_read_samples_ns,
            },
        }))))
    }

    pub(crate) fn begin(&self, kind: SpanKind) -> Span {
        let start = clock_ns(false);
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let report = &mut state.report;
        let start_ns = start.unwrap_or_else(|_| {
            report.clock_failures = report.clock_failures.saturating_add(1);
            0
        });
        report.started_spans = report.started_spans.saturating_add(1);
        report.open_spans = report.open_spans.saturating_add(1);
        if report.closed {
            report.after_close = report.after_close.saturating_add(1);
        }
        Span {
            capture: self.clone(),
            sequence: report.started_spans,
            start_ns,
            kind,
            finished: false,
        }
    }

    /// Stop/join every source first. Open spans and post-closure activity remain
    /// visible, never converted into successful work by a storage snapshot.
    pub(crate) fn close(&self) -> Report {
        let now = clock_ns(false);
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let State { report, export } = &mut *state;
        if !report.closed {
            report.closed = true;
            report.closed_at_ns = now.ok();
            if report.closed_at_ns.is_none() {
                report.clock_failures = report.clock_failures.saturating_add(1);
            }
        }
        if let Some(export) = export {
            export.record(crate::diagnostic_observer::EventKind::MinimalSummary(
                Summary::from(&*report),
            ));
        }
        report.clone()
    }
}

impl Span {
    pub(crate) fn finish(mut self, frame_bearing: bool, success: bool) {
        self.complete(
            frame_bearing,
            if success {
                SpanStatus::Succeeded
            } else {
                SpanStatus::Failed
            },
        );
    }

    fn complete(&mut self, frame_bearing: bool, status: SpanStatus) {
        let now = clock_ns(false);
        self.finished = true;
        let mut state = self
            .capture
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let State { report, export } = &mut *state;
        let end_ns = now.unwrap_or_else(|_| {
            report.clock_failures = report.clock_failures.saturating_add(1);
            0
        });
        if end_ns < self.start_ns {
            report.clock_failures = report.clock_failures.saturating_add(1);
        }
        report.open_spans -= 1;
        report.completed_spans = report.completed_spans.saturating_add(1);
        if status != SpanStatus::Succeeded {
            report.failed_spans = report.failed_spans.saturating_add(1);
        }
        if report.closed {
            report.after_close = report.after_close.saturating_add(1);
        }
        if report.closed || (export.is_none() && report.records.len() == report.capacity) {
            report.lost = report.lost.saturating_add(1);
            if let Some(export) = export {
                export.record(crate::diagnostic_observer::EventKind::MinimalSummary(
                    Summary::from(&*report),
                ));
            }
            return;
        }
        let sequence = report.completed_spans;
        let sample = Sample {
            sequence,
            span_id: self.sequence,
            kind: self.kind,
            start_ns: self.start_ns,
            end_ns,
            frame_bearing,
            status,
        };
        if let Some(export) = export {
            export.record(crate::diagnostic_observer::EventKind::MinimalSpan(sample));
        } else {
            report.records.push(sample);
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if !self.finished {
            self.complete(false, SpanStatus::Abandoned);
        }
    }
}

/// Measures exactly the supplied adapter's complete call. At the supervisor a
/// BrokerClient includes IPC; only an authenticated broker/M2 source can supply
/// native hardware-call samples. Counts here do not establish unique frames.
pub(crate) fn present<H: TouchBarHardware>(
    timing: Option<&TimingCapture>,
    hardware: &mut H,
    frame: &LogicalFrame,
) -> Result<()> {
    let span = timing.map(|timing| timing.begin(SpanKind::Present));
    let result = hardware.present(frame);
    if let Some(span) = span {
        span.finish(true, result.is_ok());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_minimal_timing_exports_actual_samples_without_a_second_record_buffer() -> Result<()>
    {
        use crate::diagnostic_capture_transport::{Collector, MappedWriter};
        use crate::diagnostic_observer::{Capture, EventKind};
        use crate::hardware::FakeTouchBar;
        let mut collector = Collector::new(8)?;
        let capture =
            Capture::from_mapped(MappedWriter::receive(collector.take_worker_storage()?)?)?;
        let timing = TimingCapture::exporting(8, capture.clone())?;
        let mut hardware = FakeTouchBar::new();
        hardware.claim()?;
        let (frame, _) = LogicalFrame::from_completed(crate::frame_slots::CompletedFrame {
            width: 2,
            height: 1,
            stride: 8,
            pixels: vec![255; 8],
            timing: crate::frame_slots::FrameTiming::new(0.0, 0.0)?,
        });
        present(Some(&timing), &mut hardware, &frame)?;
        let local = timing.close();
        assert!(local.records.is_empty());
        assert_eq!(local.completed_spans, 1);
        capture.finish();
        let raw = collector.snapshot()?;
        assert!(raw.report.closed && raw.metadata_consistent);
        assert_eq!(raw.report.lost, 0);
        assert_eq!(raw.report.records.len(), 2);
        let EventKind::MinimalSpan(sample) = &raw.report.records[0].kind else {
            panic!("actual present sample was not exported");
        };
        assert_eq!(sample.kind, SpanKind::Present);
        assert_eq!(sample.status, SpanStatus::Succeeded);
        assert!(sample.start_ns <= sample.end_ns);
        let EventKind::MinimalSummary(summary) = &raw.report.records[1].kind else {
            panic!("closure metadata was not exported");
        };
        assert_eq!(summary.completed_spans, 1);
        assert_eq!(summary.open_spans, 0);
        assert!(summary
            .closed_at_ns
            .is_some_and(|closed| closed >= sample.end_ns));
        Ok(())
    }

    #[test]
    fn private_minimal_timing_early_close_cannot_hide_open_or_abandoned_work() -> Result<()> {
        let timing = TimingCapture::new(4)?;
        let span = timing.begin(SpanKind::SupervisorDrive);
        let early = timing.close();
        assert_eq!(early.open_spans, 1);
        assert_eq!(early.completed_spans, 0);
        drop(span);
        let after = timing.close();
        assert_eq!(after.open_spans, 0);
        assert_eq!(after.completed_spans, 1);
        assert_eq!(after.failed_spans, 1);
        assert_eq!(after.after_close, 1);
        assert_eq!(after.lost, 1);
        assert!(after.records.is_empty());
        assert_eq!(after.closed_at_ns, early.closed_at_ns);
        Ok(())
    }

    #[test]
    fn private_minimal_timing_abandoned_spans_are_not_success_samples() -> Result<()> {
        let timing = TimingCapture::new(4)?;
        drop(timing.begin(SpanKind::SupervisorDrive));
        let report = timing.close();
        assert_eq!(report.failed_spans, 1);
        assert_eq!(report.open_spans, 0);
        assert_eq!(report.lost, 0);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].status, SpanStatus::Abandoned);
        assert!(report.records[0].start_ns <= report.records[0].end_ns);
        Ok(())
    }
}
