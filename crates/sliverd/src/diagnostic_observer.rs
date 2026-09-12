//! Private experimental userspace observation, not a P1 collector or evaluator.
//!
//! Authority is an explicitly supplied Capture, never an environment variable,
//! Lua API, or broker payload. No service currently provisions that authority.
//! Records are fixed-size, preallocated, and retained in memory until explicit
//! closure. This module performs no filesystem IO. Marker CRC is not provenance.
//!
//! Scope: the in-process Lua/canvas seam (including the shared-map algorithm)
//! and explicitly wrapped hardware calls. No cross-process recorder transport,
//! supervisor drive duration, timer registration, native fixture scheduler,
//! lifecycle/provenance verification, or release artifact exporter is provided.
//! Existing production constructors leave observation off. Publication is sampled
//! immediately before releasing READY, selection after acquiring READING, render
//! completion after snapshot finishing/decoding, and present return immediately
//! after the complete adapter call. These are not optical timestamps or the
//! overhead protocol's minimal callback/drive durations.
//!
//! A recorder retains at most capacity * record_bytes (currently <=32 MiB), plus
//! fixed metadata. Marker snapshots are transient complete frame copies at slot
//! publication; no pixels are retained in records. close() clones the bounded
//! record buffer for the caller, who owns that extra storage. Mutex contention,
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

#[derive(Clone, Debug)]
pub(crate) enum EventKind {
    RenderAllocated {
        attempt: u64,
    },
    RenderFinished {
        attempt: u64,
        marker: Option<std::result::Result<Marker, DecodeError>>,
    },
    Published {
        sequence: u64,
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
}

#[derive(Clone)]
pub(crate) struct Capture(Arc<Mutex<State>>);

fn clock_ns(resolution: bool) -> Result<u64> {
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
        ensure!(
            (1..=MAX_RECORDS).contains(&capacity),
            "diagnostic capacity outside 1..=65536"
        );
        let mut records = Vec::new();
        records.try_reserve_exact(capacity)?;
        let mut clock_read_samples_ns = [0; 32];
        for sample in &mut clock_read_samples_ns {
            let start = clock_ns(false)?;
            *sample = clock_ns(false)?
                .checked_sub(start)
                .context("monotonic clock regressed")?;
        }
        Ok(Self(Arc::new(Mutex::new(State {
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
                record_bytes: std::mem::size_of::<Record>(),
                clock_read_samples_ns,
            },
            renders: 0,
            calls: 0,
        }))))
    }

    pub(crate) fn monotonic_ns(&self) -> Result<u64> {
        clock_ns(false)
    }

    pub(crate) fn record(&self, kind: EventKind) {
        // Sample at the seam, not after waiting for the recorder mutex.
        self.record_at(clock_ns(false), kind);
    }

    fn record_at(&self, time: Result<u64>, kind: EventKind) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let r = &mut state.report;
        r.attempted_records = r.attempted_records.saturating_add(1);
        match &kind {
            EventKind::RenderFinished { marker: None, .. }
            | EventKind::PresentReturned { success: false, .. }
            | EventKind::TouchCallbackReturned { success: false, .. }
            | EventKind::PollFailed => r.failed_operations = r.failed_operations.saturating_add(1),
            EventKind::RenderFinished {
                marker: Some(Err(_)),
                ..
            }
            | EventKind::Published { marker: Err(_), .. }
            | EventKind::PresentEntered { marker: Err(_), .. }
            | EventKind::PendingDiscarded { marker: Err(_) } => {
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
        if r.closed || r.records.len() == r.capacity {
            r.lost = r.lost.saturating_add(1);
            return;
        }
        r.records.push(Record {
            sequence: r.attempted_records,
            at_ns,
            kind,
        });
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
        let result = hardware.poll(timeout);
        let received = clock_ns(false);
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
        state.report.clone()
    }
}
