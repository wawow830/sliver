//! Private raw worker recording transport, NOT native provenance or P1 evidence.
//! One source/ticket per Collector, <=65536 512-byte records + a 512-byte header.
//! No paths, environment, public arming or broker fields. Size seals prevent
//! truncation; they do not authenticate a caller. Only explicit private staging
//! delivers this capability. Records are append-only; death preserves a prefix,
//! never semantic closure. All mapped accesses use aligned atomic words so a
//! concurrent snapshot or interrupted metadata update has no Rust data race.
#![allow(dead_code)] // Production arming remains deliberately absent.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use memmap2::MmapMut;

use crate::diagnostic_fixture::{ObservationKind, Phase, Resolution};
use crate::diagnostic_observer::{
    DecodeError, DiscardReason, EventKind, Marker, Record, Report, TimerDisposition,
};
use crate::diagnostic_timing::{Sample, SpanKind, SpanStatus, Summary};
use crate::hardware::{
    HardwareCapability, HardwareEvent, Modifier, ModifierState, TouchEvent, TouchPhase,
};

const HEADER_BYTES: usize = 512;
pub(crate) const RECORD_BYTES: usize = 512;
const MAX_RECORDS: usize = 65_536;
const MAGIC: u64 = u64::from_le_bytes(*b"SLVRAW01");
const VERSION: u64 = 2;
const WORKER_ROLE: u64 = 1;
const SIZE_SEALS: i32 = libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
const CLAIM: usize = 6;
const REVISION: usize = 7;
const COMMITTED: usize = 8;
const INITIALIZED: usize = 51;

/// Bounded private bootstrap plan, not launcher/source authentication. The
/// child revalidates it and always constructs its own HostMonotonic clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FixtureBootstrap {
    pub(crate) run: String,
    pub(crate) generation: String,
    pub(crate) start_ns: u64,
    pub(crate) rate: u32,
    pub(crate) mode: crate::diagnostic_fixture::Mode,
    pub(crate) causal: bool,
}
impl FixtureBootstrap {
    pub(crate) fn new(
        run: &str,
        generation: &str,
        start_ns: u64,
        rate: u32,
        mode: crate::diagnostic_fixture::Mode,
    ) -> Result<Self> {
        crate::diagnostic_fixture::Plan::new(run, generation, start_ns, rate, mode)?;
        Ok(Self {
            run: run.into(),
            generation: generation.into(),
            start_ns,
            rate,
            mode,
            causal: false,
        })
    }

    pub(crate) fn plan(&self) -> Result<crate::diagnostic_fixture::Plan> {
        let plan = crate::diagnostic_fixture::Plan::new(
            &self.run,
            &self.generation,
            self.start_ns,
            self.rate,
            self.mode,
        )?;
        if self.causal {
            plan.causal_response()
        } else {
            Ok(plan)
        }
    }

    pub(crate) fn capture_level(&self) -> CaptureLevel {
        match self.mode {
            crate::diagnostic_fixture::Mode::C => CaptureLevel::Detailed,
            crate::diagnostic_fixture::Mode::A | crate::diagnostic_fixture::Mode::B => {
                CaptureLevel::Minimal
            }
        }
    }
}

/// Explicit coordinator-supplied observations; no timestamp/source authority
/// is inferred from transport delivery or from successful command completion.
#[derive(Clone, Copy)]
pub(crate) enum FixtureControl {
    Receive(crate::diagnostic_fixture::Receipt),
    ConfirmWarmupClosed,
    Finish,
    Resolve {
        frame_id: u64,
        resolution: Resolution,
        resolved_ns: u64,
    },
}

struct Mapping(MmapMut);
impl Mapping {
    fn word(&self, index: usize) -> &AtomicU64 {
        // mmap is page aligned; every index is bounds checked and word aligned.
        assert!(index < self.0.len() / 8);
        unsafe { &*self.0.as_ptr().add(index * 8).cast::<AtomicU64>() }
    }
    // SeqCst makes the bounded revision-checked snapshot ordering explicit,
    // including on AArch64. Values have a fixed little-endian mapped encoding.
    fn get(&self, index: usize) -> u64 {
        u64::from_le(self.word(index).load(Ordering::SeqCst))
    }
    fn set(&self, index: usize, value: u64) {
        self.word(index).store(value.to_le(), Ordering::SeqCst);
    }
}

pub(crate) struct Collector {
    fd: OwnedFd,
    map: Mapping,
    capacity: usize,
    ticket_taken: bool,
}

pub(crate) struct RawReport {
    pub(crate) report: Report,
    pub(crate) initialized: bool,
    /// False for an interrupted/in-flight metadata update. Even true only means
    /// a consistent storage snapshot, not source/work/transaction closure.
    pub(crate) metadata_consistent: bool,
    pub(crate) storage_bytes: usize,
}

impl Collector {
    pub(crate) fn new(capacity: usize) -> Result<Self> {
        ensure!(
            (1..=MAX_RECORDS).contains(&capacity),
            "raw capture capacity outside 1..=65536"
        );
        let size = HEADER_BYTES + capacity * RECORD_BYTES;
        let raw = unsafe {
            libc::memfd_create(
                c"sliver-worker-raw".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        ensure!(
            raw >= 0,
            "creating raw capture memfd: {}",
            io::Error::last_os_error()
        );
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        ensure!(
            unsafe { libc::ftruncate(raw, size as libc::off_t) } == 0,
            "sizing raw capture memfd: {}",
            io::Error::last_os_error()
        );
        let mut bytes = unsafe { MmapMut::map_mut(&fd)? };
        bytes.fill(0); // Fault in the bounded storage before workload recording.
        let map = Mapping(bytes);
        for (index, value) in [
            (0, MAGIC),
            (1, VERSION),
            (2, WORKER_ROLE),
            (3, 1),
            (4, capacity as u64),
            (5, RECORD_BYTES as u64),
        ] {
            map.set(index, value);
        }
        ensure!(
            unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, SIZE_SEALS) } == 0,
            "sealing raw capture size: {}",
            io::Error::last_os_error()
        );
        Ok(Self {
            fd,
            map,
            capacity,
            ticket_taken: false,
        })
    }

    /// There is exactly one source and one writer ticket; no source fan-out.
    pub(crate) fn take_worker_storage(&mut self) -> Result<OwnedFd> {
        ensure!(!self.ticket_taken, "worker capture ticket already taken");
        let fd = self.fd.try_clone()?;
        self.ticket_taken = true;
        Ok(fd)
    }

    pub(crate) fn snapshot(&self) -> Result<RawReport> {
        snapshot_mapping(&self.map, self.capacity)
    }
}

fn snapshot_mapping(map: &Mapping, capacity: usize) -> Result<RawReport> {
    let mut last = None;
    for _ in 0..3 {
        let before = map.get(REVISION);
        let count = usize::try_from(map.get(COMMITTED))?;
        ensure!(count <= capacity, "raw committed count exceeds capacity");
        let mut records = Vec::with_capacity(count);
        for index in 0..count {
            let mut bytes = [0; RECORD_BYTES];
            for (word, chunk) in bytes.chunks_exact_mut(8).enumerate() {
                chunk.copy_from_slice(
                    &map.get(HEADER_BYTES / 8 + index * RECORD_BYTES / 8 + word)
                        .to_le_bytes(),
                );
            }
            let record = decode_record(&bytes)?;
            ensure!(
                record.sequence == index as u64 + 1,
                "raw record sequence is discontinuous"
            );
            records.push(record);
        }
        let initialized = map.get(INITIALIZED) == 1;
        let mut samples = [0; 32];
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = map.get(19 + index);
        }
        let report = Report {
            records,
            capacity,
            attempted_records: map.get(9),
            lost: map.get(10),
            clock_failures: map.get(11),
            failed_operations: map.get(12),
            decode_failures: map.get(13),
            after_close: map.get(14),
            closed: map.get(15) == 1,
            closed_at_ns: match map.get(16) {
                u64::MAX => None,
                value => Some(value),
            },
            clock_resolution_ns: map.get(17),
            record_bytes: RECORD_BYTES,
            clock_read_samples_ns: samples,
        };
        let after = map.get(REVISION);
        let consistent = before == after && after.is_multiple_of(2);
        let mut raw = RawReport {
            report,
            initialized,
            metadata_consistent: consistent,
            storage_bytes: map.0.len(),
        };
        if !initialized || !consistent {
            raw.report.closed = false;
            raw.report.closed_at_ns = None;
        }
        if consistent {
            return Ok(raw);
        }
        last = Some(raw);
    }
    Ok(last.expect("bounded snapshot attempted"))
}

pub(crate) struct MappedWriter {
    map: Mapping,
    capacity: usize,
    committed: usize,
    revision: u64,
}

impl MappedWriter {
    pub(crate) fn receive(fd: OwnedFd) -> Result<Self> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        ensure!(
            unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } == 0,
            "stat raw storage failed"
        );
        let stat = unsafe { stat.assume_init() };
        ensure!(
            stat.st_mode & libc::S_IFMT == libc::S_IFREG,
            "raw storage is not a regular memfd"
        );
        let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
        ensure!(
            seals >= 0 && seals & SIZE_SEALS == SIZE_SEALS,
            "raw storage lacks immutable size seals"
        );
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0 && flags & libc::O_ACCMODE == libc::O_RDWR,
            "raw storage is not writable"
        );
        ensure!(
            (HEADER_BYTES as i64..=(HEADER_BYTES + MAX_RECORDS * RECORD_BYTES) as i64)
                .contains(&stat.st_size),
            "raw storage size outside bound"
        );
        let map = Mapping(unsafe { MmapMut::map_mut(&fd)? });
        ensure!(
            map.get(0) == MAGIC && map.get(1) == VERSION,
            "unsupported raw storage version"
        );
        ensure!(
            map.get(2) == WORKER_ROLE && map.get(3) == 1,
            "raw storage is not exactly one worker source"
        );
        let capacity = usize::try_from(map.get(4))?;
        ensure!(
            (1..=MAX_RECORDS).contains(&capacity) && map.get(5) == RECORD_BYTES as u64,
            "invalid raw storage layout"
        );
        ensure!(
            map.0.len() == HEADER_BYTES + capacity * RECORD_BYTES,
            "truncated raw storage layout"
        );
        ensure!(
            map.get(REVISION) == 0 && map.get(COMMITTED) == 0 && map.get(INITIALIZED) == 0,
            "raw storage already used"
        );
        ensure!(
            map.word(CLAIM)
                .compare_exchange(0, 1_u64.to_le(), Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "raw storage already claimed"
        );
        // Collector populated backing pages, not this process's PTEs. A real
        // atomic write/no-op RMW prefaults each writable receiver page after
        // exclusive claim, without changing identity, metadata, or records and
        // without racing a snapshot through plain shared-memory writes.
        let page_bytes = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .context("reading raw capture page size")?;
        ensure!(
            page_bytes >= 8 && page_bytes.is_multiple_of(8),
            "invalid raw capture page size"
        );
        for offset in (0..map.0.len()).step_by(page_bytes) {
            map.word(offset / 8).fetch_or(0, Ordering::SeqCst);
        }
        Ok(Self {
            map,
            capacity,
            committed: 0,
            revision: 0,
        })
    }
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
    pub(crate) fn snapshot(&self) -> Result<RawReport> {
        snapshot_mapping(&self.map, self.capacity)
    }
    fn begin(&mut self) {
        if self.revision.is_multiple_of(2) {
            self.revision += 1;
            self.map.set(REVISION, self.revision);
        }
    }
    pub(crate) fn append(&mut self, record: &Record) -> Result<()> {
        ensure!(self.committed < self.capacity, "raw recording storage full");
        let bytes = encode_record(record)?;
        self.begin();
        for (index, chunk) in bytes.chunks_exact(8).enumerate() {
            self.map.set(
                HEADER_BYTES / 8 + self.committed * RECORD_BYTES / 8 + index,
                u64::from_le_bytes(chunk.try_into()?),
            );
        }
        self.committed += 1;
        self.map.set(COMMITTED, self.committed as u64);
        Ok(())
    }
    pub(crate) fn update_metadata(&mut self, report: &Report) {
        self.begin();
        for (index, value) in [
            (9, report.attempted_records),
            (10, report.lost),
            (11, report.clock_failures),
            (12, report.failed_operations),
            (13, report.decode_failures),
            (14, report.after_close),
            (15, u64::from(report.closed)),
            (16, report.closed_at_ns.unwrap_or(u64::MAX)),
            (17, report.clock_resolution_ns),
        ] {
            self.map.set(index, value);
        }
        for (index, value) in report.clock_read_samples_ns.iter().enumerate() {
            self.map.set(19 + index, *value);
        }
        self.map.set(INITIALIZED, 1);
        self.revision += 1;
        self.map.set(REVISION, self.revision);
    }
}

// A single ancillary-bearing byte precedes the existing bounded BOOTSTRAP
// packet. Production sends Off. No ordinary Read may consume this byte first.
const BOOTSTRAP_OFF: u8 = 0xa0;
const BOOTSTRAP_WORKER: u8 = 0xa1;
const BOOTSTRAP_WORKER_MINIMAL: u8 = 0xa2;

/// Recording level, NOT marker/scene mode or authenticated P1 provenance.
/// Both levels export minimal spans and generic worker failure observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureLevel {
    Detailed,
    Minimal,
}

pub(crate) fn send_bootstrap_storage(
    stream: &UnixStream,
    storage: Option<(&OwnedFd, CaptureLevel)>,
    deadline: Instant,
) -> Result<()> {
    let mut byte = match storage {
        Some((_, CaptureLevel::Detailed)) => BOOTSTRAP_WORKER,
        Some((_, CaptureLevel::Minimal)) => BOOTSTRAP_WORKER_MINIMAL,
        None => BOOTSTRAP_OFF,
    };
    let mut iov = libc::iovec {
        iov_base: (&mut byte as *mut u8).cast(),
        iov_len: 1,
    };
    let mut control = [0_usize; 8]; // aligned and bounded; only one FD is emitted
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if let Some((fd, _)) = storage {
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize;
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as usize;
            libc::CMSG_DATA(cmsg)
                .cast::<i32>()
                .write_unaligned(fd.as_raw_fd());
        }
    }
    loop {
        ensure!(
            Instant::now() < deadline,
            "sending worker bootstrap descriptors timed out"
        );
        let sent = unsafe {
            libc::sendmsg(
                stream.as_raw_fd(),
                &msg,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent == 1 {
            return Ok(());
        }
        if sent == 0 {
            bail!("worker bootstrap descriptor send closed");
        }
        retry_io(Some(deadline), "sending worker bootstrap descriptors")?;
    }
}

pub(crate) fn receive_bootstrap_storage(
    stream: &UnixStream,
    deadline: Instant,
) -> Result<Option<(MappedWriter, CaptureLevel)>> {
    receive_storage(stream, Some(deadline))
}

/// The actual child is supervised by the parent's existing bootstrap watchdog
/// and EOF. Launcher/cgroup discovery can precede that budget, so the child must
/// not start a competing timer when it sends HELLO.
pub(crate) fn receive_parent_bootstrap_storage(
    stream: &UnixStream,
) -> Result<Option<(MappedWriter, CaptureLevel)>> {
    receive_storage(stream, None)
}

fn receive_storage(
    stream: &UnixStream,
    deadline: Option<Instant>,
) -> Result<Option<(MappedWriter, CaptureLevel)>> {
    loop {
        if let Some(deadline) = deadline {
            ensure!(
                Instant::now() < deadline,
                "receiving worker bootstrap descriptors timed out"
            );
        }
        let mut byte = 0;
        let mut iov = libc::iovec {
            iov_base: (&mut byte as *mut u8).cast(),
            iov_len: 1,
        };
        // CMSG alignment can allow a second FD in the padding: parse and close
        // everything received before rejecting counts or truncation.
        let mut control = [0_usize; 32 / std::mem::size_of::<usize>()];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&control);
        let received = unsafe {
            libc::recvmsg(
                stream.as_raw_fd(),
                &mut msg,
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if received < 0 {
            retry_io(deadline, "receiving worker bootstrap descriptors")?;
            continue;
        }
        // Even a zero-sized ancillary header could not fit more than eight
        // i32 descriptors in this 32-byte control buffer. Never grow this Vec.
        let mut fds = Vec::with_capacity(8);
        let mut invalid = false;
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
                    invalid = true;
                } else {
                    let len = (*cmsg).cmsg_len.saturating_sub(libc::CMSG_LEN(0) as usize);
                    if !len.is_multiple_of(std::mem::size_of::<i32>()) {
                        invalid = true;
                    }
                    for index in 0..len / std::mem::size_of::<i32>() {
                        fds.push(OwnedFd::from_raw_fd(
                            libc::CMSG_DATA(cmsg)
                                .cast::<i32>()
                                .add(index)
                                .read_unaligned(),
                        ));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        ensure!(
            received == 1,
            "worker disconnected before descriptor bootstrap"
        );
        ensure!(
            !invalid && msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) == 0,
            "truncated or unsupported bootstrap ancillary data"
        );
        match byte {
            BOOTSTRAP_OFF => {
                ensure!(fds.is_empty(), "off bootstrap carries descriptors");
                return Ok(None);
            }
            BOOTSTRAP_WORKER | BOOTSTRAP_WORKER_MINIMAL => {
                ensure!(
                    fds.len() == 1,
                    "worker bootstrap requires exactly one storage descriptor"
                );
                let level = if byte == BOOTSTRAP_WORKER {
                    CaptureLevel::Detailed
                } else {
                    CaptureLevel::Minimal
                };
                return MappedWriter::receive(fds.pop().expect("one descriptor"))
                    .map(|writer| Some((writer, level)));
            }
            _ => bail!("unknown worker descriptor bootstrap version"),
        }
    }
}

fn retry_io(deadline: Option<Instant>, context: &str) -> Result<()> {
    let error = io::Error::last_os_error();
    if !matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    ) {
        return Err(error).context(context.to_owned());
    }
    if let Some(deadline) = deadline {
        ensure!(Instant::now() < deadline, "{context} timed out");
    }
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}

// Version 1 explicit tags; f64 values preserve raw bits, not policy integers.
struct Encoder {
    bytes: [u8; RECORD_BYTES],
    at: usize,
}
impl Encoder {
    fn new() -> Self {
        Self {
            bytes: [0; RECORD_BYTES],
            at: 2,
        }
    }
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self
            .at
            .checked_add(bytes.len())
            .context("raw encoding length overflow")?;
        ensure!(
            end <= RECORD_BYTES,
            "raw record exceeds fixed encoding size"
        );
        self.bytes[self.at..end].copy_from_slice(bytes);
        self.at = end;
        Ok(())
    }
    fn u8(&mut self, value: u8) -> Result<()> {
        self.bytes(&[value])
    }
    fn u64(&mut self, value: u64) -> Result<()> {
        self.bytes(&value.to_le_bytes())
    }
    fn f64(&mut self, value: f64) -> Result<()> {
        self.u64(value.to_bits())
    }
    fn boolean(&mut self, value: bool) -> Result<()> {
        self.u8(u8::from(value))
    }
    fn optional(&mut self, value: Option<f64>) -> Result<()> {
        self.boolean(value.is_some())?;
        if let Some(value) = value {
            self.f64(value)?;
        }
        Ok(())
    }
    fn allocation(
        &mut self,
        allocation: Option<crate::frame_slots::FrameAllocation>,
    ) -> Result<()> {
        self.boolean(allocation.is_some())?;
        if let Some(allocation) = allocation {
            validate_allocation(allocation)?;
            self.u64(allocation.id)?;
            self.u64(allocation.token)?;
            self.u64(allocation.allocated_ns)?;
        }
        Ok(())
    }
    fn marker(&mut self, marker: &std::result::Result<Marker, DecodeError>) -> Result<()> {
        match marker {
            Ok(marker) => {
                self.u8(0)?;
                self.bytes(marker.bytes())
            }
            Err(DecodeError::Geometry) => self.u8(1),
            Err(DecodeError::Cell) => self.u8(2),
            Err(DecodeError::Packet) => self.u8(3),
        }
    }
    fn fixture(&mut self, kind: &ObservationKind) -> Result<()> {
        match kind {
            ObservationKind::Phase(phase) => {
                self.u8(0)?;
                self.u8(match phase {
                    Phase::Warmup => 0,
                    Phase::Quiescing => 1,
                    Phase::Armed => 2,
                    Phase::Measured => 3,
                    Phase::Drain => 4,
                    Phase::Ended => 5,
                })?;
            }
            ObservationKind::Allocated { frame_id, token } => {
                self.u8(1)?;
                self.u64(*frame_id)?;
                self.u64(*token)?;
            }
            ObservationKind::Suppressed => self.u8(2)?,
            ObservationKind::Resolved {
                frame_id,
                resolution,
                resolved_ns,
            } => {
                self.u8(3)?;
                self.u64(*frame_id)?;
                self.u8(match resolution {
                    Resolution::Presented => 0,
                    Resolution::Discarded => 1,
                    Resolution::Failed => 2,
                })?;
                self.u64(*resolved_ns)?;
            }
            ObservationKind::WarmupClosed => self.u8(4)?,
            ObservationKind::PeriodicRetired { timer_id } => {
                self.u8(5)?;
                self.u64(*timer_id)?;
            }
            ObservationKind::ReceiptSupplied {
                receipt_sequence,
                received_ns,
                contact,
                phase,
            } => {
                self.u8(6)?;
                self.u64(*receipt_sequence)?;
                self.u64(*received_ns)?;
                self.bytes(&contact.to_le_bytes())?;
                self.u8(match phase {
                    TouchPhase::Down => 0,
                    TouchPhase::Move => 1,
                    TouchPhase::Up => 2,
                    TouchPhase::Cancel => 3,
                })?;
            }
            ObservationKind::Delivered { receipt_sequence } => {
                self.u8(7)?;
                self.u64(*receipt_sequence)?;
            }
            ObservationKind::TokenMutated {
                receipt_sequence,
                previous,
                token,
            } => {
                self.u8(8)?;
                self.u64(*receipt_sequence)?;
                self.u64(*previous)?;
                self.u64(*token)?;
            }
            ObservationKind::ResponseConfirmed {
                receipt_sequence,
                frame_id,
                token,
                responded_ns,
            } => {
                self.u8(9)?;
                self.u64(*receipt_sequence)?;
                self.u64(*frame_id)?;
                self.u64(*token)?;
                self.u64(*responded_ns)?;
            }
            ObservationKind::Failed => self.u8(10)?,
            ObservationKind::Closed => self.u8(11)?,
        }
        Ok(())
    }
    fn touch(&mut self, t: &TouchEvent) -> Result<()> {
        self.u8(match t.phase {
            TouchPhase::Down => 0,
            TouchPhase::Move => 1,
            TouchPhase::Up => 2,
            TouchPhase::Cancel => 3,
        })?;
        self.bytes(&t.id.to_le_bytes())?;
        self.f64(t.time)?;
        self.f64(t.x)?;
        self.f64(t.y)?;
        for modifier in Modifier::ALL {
            self.boolean(t.modifiers.is_active(modifier))?;
        }
        self.optional(t.pressure)?;
        self.optional(t.width)?;
        self.optional(t.height)
    }
}

fn validate_allocation(allocation: crate::frame_slots::FrameAllocation) -> Result<()> {
    ensure!(
        allocation.id > 0 && allocation.id <= i64::MAX as u64,
        "invalid raw fixture allocation id"
    );
    ensure!(
        allocation.token <= i64::MAX as u64
            && allocation.allocated_ns > 0
            && allocation.allocated_ns <= i64::MAX as u64,
        "invalid raw fixture allocation token/time"
    );
    Ok(())
}

fn encode_record(record: &Record) -> Result<[u8; RECORD_BYTES]> {
    use EventKind::*;
    let mut e = Encoder::new();
    e.u64(record.sequence)?;
    e.u64(record.at_ns)?;
    match &record.kind {
        RenderAllocated { attempt } => {
            e.u8(1)?;
            e.u64(*attempt)?;
        }
        RenderFinished { attempt, marker } => {
            e.u8(2)?;
            e.u64(*attempt)?;
            e.boolean(marker.is_some())?;
            if let Some(marker) = marker {
                e.marker(marker)?;
            }
        }
        Published {
            sequence,
            allocation,
            marker,
        } => {
            e.u8(3)?;
            e.u64(*sequence)?;
            e.allocation(*allocation)?;
            e.marker(marker)?;
        }
        Selected { sequence } => {
            e.u8(4)?;
            e.u64(*sequence)?;
        }
        Discarded { sequence, reason } => {
            e.u8(5)?;
            e.u64(*sequence)?;
            e.u8(match reason {
                DiscardReason::ProducerReclaim => 0,
                DiscardReason::ConsumerSuperseded => 1,
                DiscardReason::MappingClosed => 2,
            })?;
        }
        PendingDiscarded { allocation, marker } => {
            e.u8(6)?;
            e.allocation(*allocation)?;
            e.marker(marker)?;
        }
        PresentEntered { call, marker } => {
            e.u8(7)?;
            e.u64(*call)?;
            e.marker(marker)?;
        }
        PresentReturned { call, success } => {
            e.u8(8)?;
            e.u64(*call)?;
            e.boolean(*success)?;
        }
        InputReceived(event) => {
            e.u8(9)?;
            match event {
                HardwareEvent::Touch(t) => {
                    e.u8(0)?;
                    e.touch(t)?;
                }
                HardwareEvent::Fn { active } => {
                    e.u8(1)?;
                    e.boolean(*active)?;
                }
                HardwareEvent::Modifier { modifier, active } => {
                    e.u8(2)?;
                    e.u8(modifier.index() as u8)?;
                    e.boolean(*active)?;
                }
                HardwareEvent::Device { present } => {
                    e.u8(3)?;
                    e.boolean(*present)?;
                }
                HardwareEvent::Capability {
                    capability,
                    present,
                } => {
                    e.u8(4)?;
                    e.u8(capability.to_wire())?;
                    e.boolean(*present)?;
                }
                HardwareEvent::Visibility { visible } => {
                    e.u8(5)?;
                    e.boolean(*visible)?;
                }
            }
        }
        PollFailed => e.u8(10)?,
        TouchCallbackEntered(t) => {
            e.u8(11)?;
            e.touch(t)?;
        }
        TouchCallbackReturned { contact, success } => {
            e.u8(12)?;
            e.bytes(&contact.to_le_bytes())?;
            e.boolean(*success)?;
        }
        TimerRegistered {
            timer_id,
            delay_seconds,
            interval_seconds,
            scheduler_now_seconds,
        } => {
            e.u8(13)?;
            e.u64(*timer_id)?;
            e.f64(*delay_seconds)?;
            e.optional(*interval_seconds)?;
            e.optional(*scheduler_now_seconds)?;
        }
        TimerActivated {
            timer_id,
            scheduler_now_seconds,
            deadline_seconds,
        } => {
            e.u8(14)?;
            e.u64(*timer_id)?;
            e.f64(*scheduler_now_seconds)?;
            e.f64(*deadline_seconds)?;
        }
        TimerCancelled { timer_id, removed } => {
            e.u8(15)?;
            e.u64(*timer_id)?;
            e.boolean(*removed)?;
        }
        TimerDeadlineShifted {
            timer_id,
            previous_deadline_seconds,
            shift_seconds,
            deadline_seconds,
        } => {
            e.u8(16)?;
            e.u64(*timer_id)?;
            e.f64(*previous_deadline_seconds)?;
            e.f64(*shift_seconds)?;
            e.f64(*deadline_seconds)?;
        }
        TimerDispatched {
            timer_id,
            scheduled_deadline_seconds,
            scheduler_now_seconds,
        } => {
            e.u8(17)?;
            e.u64(*timer_id)?;
            e.f64(*scheduled_deadline_seconds)?;
            e.f64(*scheduler_now_seconds)?;
        }
        TimerFinished {
            timer_id,
            success,
            scheduler_now_seconds,
            disposition,
        } => {
            e.u8(18)?;
            e.u64(*timer_id)?;
            e.boolean(*success)?;
            e.f64(*scheduler_now_seconds)?;
            match disposition {
                TimerDisposition::Completed => e.u8(0)?,
                TimerDisposition::Cancelled => e.u8(1)?,
                TimerDisposition::Rescheduled {
                    next_deadline_seconds,
                    intervals_advanced,
                    skipped_intervals,
                    fallback,
                } => {
                    e.u8(2)?;
                    e.f64(*next_deadline_seconds)?;
                    e.f64(*intervals_advanced)?;
                    e.f64(*skipped_intervals)?;
                    e.boolean(*fallback)?;
                }
            }
        }
        RedrawRequested { coalesced } => {
            e.u8(19)?;
            e.boolean(*coalesced)?;
        }
        WorkerLoadFailed => e.u8(20)?,
        WorkerLoopFailed => e.u8(21)?,
        MinimalSpan(sample) => {
            e.u8(22)?;
            e.u64(sample.sequence)?;
            e.u64(sample.span_id)?;
            e.u8(match sample.kind {
                SpanKind::RenderCallback => 0,
                SpanKind::Present => 1,
                SpanKind::SupervisorDrive => 2,
                SpanKind::Calibration => 3,
            })?;
            e.u64(sample.start_ns)?;
            e.u64(sample.end_ns)?;
            e.boolean(sample.frame_bearing)?;
            e.u8(match sample.status {
                SpanStatus::Succeeded => 0,
                SpanStatus::Failed => 1,
                SpanStatus::Abandoned => 2,
            })?;
        }
        MinimalSummary(summary) => {
            e.u8(23)?;
            for value in [
                summary.capacity as u64,
                summary.started_spans,
                summary.completed_spans,
                summary.open_spans,
                summary.failed_spans,
                summary.lost,
                summary.after_close,
                summary.clock_failures,
            ] {
                e.u64(value)?;
            }
            e.boolean(summary.closed_at_ns.is_some())?;
            if let Some(at) = summary.closed_at_ns {
                e.u64(at)?;
            }
            e.u64(summary.clock_resolution_ns)?;
            e.u64(summary.record_bytes as u64)?;
            for value in summary.clock_read_samples_ns {
                e.u64(value)?;
            }
        }
        WorkerCommandFailed => e.u8(24)?,
        WorkerRunFailed => e.u8(25)?,
        WorkerCaptureConfigured { detailed } => {
            e.u8(26)?;
            e.boolean(*detailed)?;
        }
        MinimalCalibration { cost_ns } => {
            e.u8(27)?;
            for cost in cost_ns {
                e.u64(*cost)?;
            }
        }
        FixtureObserved {
            synthetic,
            decision_ns,
            kind,
        } => {
            e.u8(32)?;
            e.boolean(*synthetic)?;
            e.u64(*decision_ns)?;
            e.fixture(kind)?;
        }
    }
    e.bytes[..2].copy_from_slice(&(e.at as u16).to_le_bytes());
    Ok(e.bytes)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Decoder<'a> {
    fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(len).context("raw decode overflow")?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .context("truncated raw record")?;
        self.at = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn boolean(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => bail!("invalid raw boolean"),
        }
    }
    fn optional(&mut self) -> Result<Option<f64>> {
        if self.boolean()? {
            self.f64().map(Some)
        } else {
            Ok(None)
        }
    }
    fn allocation(&mut self) -> Result<Option<crate::frame_slots::FrameAllocation>> {
        if !self.boolean()? {
            return Ok(None);
        }
        let allocation = crate::frame_slots::FrameAllocation {
            id: self.u64()?,
            token: self.u64()?,
            allocated_ns: self.u64()?,
        };
        validate_allocation(allocation)?;
        Ok(Some(allocation))
    }
    fn marker(&mut self) -> Result<std::result::Result<Marker, DecodeError>> {
        Ok(match self.u8()? {
            0 => Ok(
                crate::diagnostic_observer::decode_packet(self.bytes(408)?.try_into()?)
                    .map_err(|_| anyhow::anyhow!("invalid exported marker packet"))?,
            ),
            1 => Err(DecodeError::Geometry),
            2 => Err(DecodeError::Cell),
            3 => Err(DecodeError::Packet),
            _ => bail!("invalid raw marker status"),
        })
    }
    fn fixture(&mut self) -> Result<ObservationKind> {
        Ok(match self.u8()? {
            0 => ObservationKind::Phase(match self.u8()? {
                0 => Phase::Warmup,
                1 => Phase::Quiescing,
                2 => Phase::Armed,
                3 => Phase::Measured,
                4 => Phase::Drain,
                5 => Phase::Ended,
                _ => bail!("invalid raw fixture phase"),
            }),
            1 => ObservationKind::Allocated {
                frame_id: self.u64()?,
                token: self.u64()?,
            },
            2 => ObservationKind::Suppressed,
            3 => ObservationKind::Resolved {
                frame_id: self.u64()?,
                resolution: match self.u8()? {
                    0 => Resolution::Presented,
                    1 => Resolution::Discarded,
                    2 => Resolution::Failed,
                    _ => bail!("invalid raw fixture resolution"),
                },
                resolved_ns: self.u64()?,
            },
            4 => ObservationKind::WarmupClosed,
            5 => ObservationKind::PeriodicRetired {
                timer_id: self.u64()?,
            },
            6 => ObservationKind::ReceiptSupplied {
                receipt_sequence: self.u64()?,
                received_ns: self.u64()?,
                contact: self.u32()?,
                phase: match self.u8()? {
                    0 => TouchPhase::Down,
                    1 => TouchPhase::Move,
                    2 => TouchPhase::Up,
                    3 => TouchPhase::Cancel,
                    _ => bail!("invalid raw fixture touch phase"),
                },
            },
            7 => ObservationKind::Delivered {
                receipt_sequence: self.u64()?,
            },
            8 => ObservationKind::TokenMutated {
                receipt_sequence: self.u64()?,
                previous: self.u64()?,
                token: self.u64()?,
            },
            9 => ObservationKind::ResponseConfirmed {
                receipt_sequence: self.u64()?,
                frame_id: self.u64()?,
                token: self.u64()?,
                responded_ns: self.u64()?,
            },
            10 => ObservationKind::Failed,
            11 => ObservationKind::Closed,
            _ => bail!("invalid raw fixture observation tag"),
        })
    }
    fn touch(&mut self) -> Result<TouchEvent> {
        let phase = match self.u8()? {
            0 => TouchPhase::Down,
            1 => TouchPhase::Move,
            2 => TouchPhase::Up,
            3 => TouchPhase::Cancel,
            _ => bail!("invalid raw touch phase"),
        };
        let id = self.u32()?;
        let time = self.f64()?;
        let x = self.f64()?;
        let y = self.f64()?;
        let mut modifiers = ModifierState::default();
        for modifier in Modifier::ALL {
            modifiers.set(modifier, self.boolean()?);
        }
        Ok(TouchEvent {
            phase,
            id,
            time,
            x,
            y,
            modifiers,
            pressure: self.optional()?,
            width: self.optional()?,
            height: self.optional()?,
        })
    }
}
fn decode_record(bytes: &[u8; RECORD_BYTES]) -> Result<Record> {
    use EventKind::*;
    let len = u16::from_le_bytes(bytes[..2].try_into()?) as usize;
    ensure!(
        (19..=RECORD_BYTES).contains(&len),
        "invalid raw record length"
    );
    ensure!(
        bytes[len..].iter().all(|byte| *byte == 0),
        "nonzero raw record padding"
    );
    let mut d = Decoder {
        bytes: &bytes[..len],
        at: 2,
    };
    let sequence = d.u64()?;
    let at_ns = d.u64()?;
    let kind = match d.u8()? {
        1 => RenderAllocated { attempt: d.u64()? },
        2 => RenderFinished {
            attempt: d.u64()?,
            marker: if d.boolean()? {
                Some(d.marker()?)
            } else {
                None
            },
        },
        3 => Published {
            sequence: d.u64()?,
            allocation: d.allocation()?,
            marker: d.marker()?,
        },
        4 => Selected { sequence: d.u64()? },
        5 => Discarded {
            sequence: d.u64()?,
            reason: match d.u8()? {
                0 => DiscardReason::ProducerReclaim,
                1 => DiscardReason::ConsumerSuperseded,
                2 => DiscardReason::MappingClosed,
                _ => bail!("invalid raw discard reason"),
            },
        },
        6 => PendingDiscarded {
            allocation: d.allocation()?,
            marker: d.marker()?,
        },
        7 => PresentEntered {
            call: d.u64()?,
            marker: d.marker()?,
        },
        8 => PresentReturned {
            call: d.u64()?,
            success: d.boolean()?,
        },
        9 => InputReceived(match d.u8()? {
            0 => HardwareEvent::Touch(d.touch()?),
            1 => HardwareEvent::Fn {
                active: d.boolean()?,
            },
            2 => HardwareEvent::Modifier {
                modifier: *Modifier::ALL
                    .get(d.u8()? as usize)
                    .context("invalid raw modifier")?,
                active: d.boolean()?,
            },
            3 => HardwareEvent::Device {
                present: d.boolean()?,
            },
            4 => HardwareEvent::Capability {
                capability: HardwareCapability::from_wire(d.u8()?)?,
                present: d.boolean()?,
            },
            5 => HardwareEvent::Visibility {
                visible: d.boolean()?,
            },
            _ => bail!("invalid raw hardware event"),
        }),
        10 => PollFailed,
        11 => TouchCallbackEntered(d.touch()?),
        12 => TouchCallbackReturned {
            contact: d.u32()?,
            success: d.boolean()?,
        },
        13 => TimerRegistered {
            timer_id: d.u64()?,
            delay_seconds: d.f64()?,
            interval_seconds: d.optional()?,
            scheduler_now_seconds: d.optional()?,
        },
        14 => TimerActivated {
            timer_id: d.u64()?,
            scheduler_now_seconds: d.f64()?,
            deadline_seconds: d.f64()?,
        },
        15 => TimerCancelled {
            timer_id: d.u64()?,
            removed: d.boolean()?,
        },
        16 => TimerDeadlineShifted {
            timer_id: d.u64()?,
            previous_deadline_seconds: d.f64()?,
            shift_seconds: d.f64()?,
            deadline_seconds: d.f64()?,
        },
        17 => TimerDispatched {
            timer_id: d.u64()?,
            scheduled_deadline_seconds: d.f64()?,
            scheduler_now_seconds: d.f64()?,
        },
        18 => TimerFinished {
            timer_id: d.u64()?,
            success: d.boolean()?,
            scheduler_now_seconds: d.f64()?,
            disposition: match d.u8()? {
                0 => TimerDisposition::Completed,
                1 => TimerDisposition::Cancelled,
                2 => TimerDisposition::Rescheduled {
                    next_deadline_seconds: d.f64()?,
                    intervals_advanced: d.f64()?,
                    skipped_intervals: d.f64()?,
                    fallback: d.boolean()?,
                },
                _ => bail!("invalid raw timer disposition"),
            },
        },
        19 => RedrawRequested {
            coalesced: d.boolean()?,
        },
        20 => WorkerLoadFailed,
        21 => WorkerLoopFailed,
        22 => MinimalSpan(Sample {
            sequence: d.u64()?,
            span_id: d.u64()?,
            kind: match d.u8()? {
                0 => SpanKind::RenderCallback,
                1 => SpanKind::Present,
                2 => SpanKind::SupervisorDrive,
                3 => SpanKind::Calibration,
                _ => bail!("invalid raw minimal span kind"),
            },
            start_ns: d.u64()?,
            end_ns: d.u64()?,
            frame_bearing: d.boolean()?,
            status: match d.u8()? {
                0 => SpanStatus::Succeeded,
                1 => SpanStatus::Failed,
                2 => SpanStatus::Abandoned,
                _ => bail!("invalid raw minimal span status"),
            },
        }),
        23 => MinimalSummary(Summary {
            capacity: usize::try_from(d.u64()?)?,
            started_spans: d.u64()?,
            completed_spans: d.u64()?,
            open_spans: d.u64()?,
            failed_spans: d.u64()?,
            lost: d.u64()?,
            after_close: d.u64()?,
            clock_failures: d.u64()?,
            closed_at_ns: if d.boolean()? { Some(d.u64()?) } else { None },
            clock_resolution_ns: d.u64()?,
            record_bytes: usize::try_from(d.u64()?)?,
            clock_read_samples_ns: {
                let mut samples = [0; 32];
                for sample in &mut samples {
                    *sample = d.u64()?;
                }
                samples
            },
        }),
        24 => WorkerCommandFailed,
        25 => WorkerRunFailed,
        26 => WorkerCaptureConfigured {
            detailed: d.boolean()?,
        },
        27 => MinimalCalibration {
            cost_ns: {
                let mut costs = [0; 32];
                for cost in &mut costs {
                    *cost = d.u64()?;
                }
                costs
            },
        },
        32 => FixtureObserved {
            synthetic: d.boolean()?,
            decision_ns: d.u64()?,
            kind: d.fixture()?,
        },
        _ => bail!("unknown raw event tag"),
    };
    ensure!(d.at == len, "trailing raw record bytes");
    Ok(Record {
        sequence,
        at_ns,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Malformed peers are manufactured at the descriptor interface, not by
    // weakening validation or adding runtime provisioning switches.
    fn send_rights(stream: &UnixStream, byte: u8, fds: &[i32]) -> Result<()> {
        ensure!(fds.len() <= 8, "test descriptor bound");
        let mut byte = byte;
        let mut iov = libc::iovec {
            iov_base: (&mut byte as *mut u8).cast(),
            iov_len: 1,
        };
        let mut control = [0_usize; 16];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if !fds.is_empty() {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen =
                unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&msg);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as usize;
                std::ptr::copy_nonoverlapping(
                    fds.as_ptr().cast::<u8>(),
                    libc::CMSG_DATA(cmsg),
                    std::mem::size_of_val(fds),
                );
            }
        }
        ensure!(
            unsafe { libc::sendmsg(stream.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) } == 1,
            "test ancillary send failed"
        );
        Ok(())
    }

    #[test]
    fn private_capture_bootstrap_rejects_missing_extra_and_truncated_rights() -> Result<()> {
        let mut collector = Collector::new(1)?;
        let fd = collector.take_worker_storage()?;
        for (byte, count, message) in [
            (BOOTSTRAP_WORKER, 0, "exactly one"),
            (BOOTSTRAP_WORKER, 2, "exactly one"),
            (BOOTSTRAP_WORKER, 8, "truncated"),
            (BOOTSTRAP_WORKER_MINIMAL, 0, "exactly one"),
            (BOOTSTRAP_WORKER_MINIMAL, 2, "exactly one"),
            (BOOTSTRAP_WORKER_MINIMAL, 8, "truncated"),
            (BOOTSTRAP_OFF, 1, "off bootstrap"),
            (0xff, 1, "version"),
        ] {
            let (sender, receiver) = UnixStream::pair()?;
            send_rights(&sender, byte, &vec![fd.as_raw_fd(); count])?;
            let error =
                receive_bootstrap_storage(&receiver, Instant::now() + Duration::from_secs(1))
                    .err()
                    .context("malformed rights accepted")?;
            assert!(format!("{error:#}").contains(message), "{error:#}");
        }
        assert!(!collector.snapshot()?.initialized);
        // The rejected transfers did not consume/claim the real source ticket.
        let (sender, receiver) = UnixStream::pair()?;
        send_bootstrap_storage(
            &sender,
            Some((&fd, CaptureLevel::Detailed)),
            Instant::now() + Duration::from_secs(1),
        )?;
        assert!(
            receive_bootstrap_storage(&receiver, Instant::now() + Duration::from_secs(1))?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn private_capture_storage_rejects_unsealed_readonly_wrong_role_and_layout() -> Result<()> {
        use std::os::unix::fs::FileExt;
        let regular = tempfile::tempfile()?;
        regular.set_len(HEADER_BYTES as u64)?;
        assert!(MappedWriter::receive(regular.into()).is_err());
        let (socket, _) = UnixStream::pair()?;
        assert!(MappedWriter::receive(socket.into()).is_err());

        let mut collector = Collector::new(2)?;
        let fd = collector.take_worker_storage()?;
        let read_only = std::fs::File::open(format!("/proc/self/fd/{}", fd.as_raw_fd()))?;
        assert!(MappedWriter::receive(read_only.into()).is_err());
        assert_eq!(
            unsafe { libc::ftruncate(fd.as_raw_fd(), HEADER_BYTES as libc::off_t) },
            -1
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));

        // The memfd size cannot change; an inconsistent declared layout must
        // also be rejected rather than mapping beyond its actual allocation.
        let file = std::fs::File::from(fd.try_clone()?);
        for (index, invalid, original) in [
            (2, 2, WORKER_ROLE),
            (3, 2, 1),
            (4, 3, 2),
            (5, 1024, RECORD_BYTES as u64),
        ] {
            file.write_all_at(&u64::to_le_bytes(invalid), index * 8)?;
            assert!(MappedWriter::receive(fd.try_clone()?).is_err());
            file.write_all_at(&u64::to_le_bytes(original), index * 8)?;
        }
        assert!(MappedWriter::receive(fd.try_clone()?).is_ok());
        assert!(
            MappedWriter::receive(fd).is_err(),
            "a second writer must not claim the source"
        );
        assert!(collector.take_worker_storage().is_err());
        assert!(Collector::new(0).is_err());
        assert!(Collector::new(MAX_RECORDS + 1).is_err());
        Ok(())
    }

    #[test]
    fn private_capture_wire_is_explicit_and_late_writes_remain_loss() -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use std::os::unix::fs::FileExt;
        let mut collector = Collector::new(2)?;
        let fd = collector.take_worker_storage()?;
        let raw_file = std::fs::File::from(fd.try_clone()?);
        let capture = Capture::from_mapped(MappedWriter::receive(fd)?)?;
        capture.record(EventKind::RenderAllocated { attempt: 7 });
        capture.finish();
        capture.record(EventKind::PollFailed);
        let raw = collector.snapshot()?;
        assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
        assert_eq!(raw.report.records.len(), 1);
        assert_eq!(raw.report.attempted_records, 2);
        assert_eq!(raw.report.lost, 1);
        assert_eq!(raw.report.after_close, 1);
        assert_eq!(raw.report.failed_operations, 1);
        assert_eq!(raw.storage_bytes, 1536);
        let mut record = [0; RECORD_BYTES];
        raw_file.read_exact_at(&mut record, HEADER_BYTES as u64)?;
        // Independently specified v1: u16 length, u64 sequence/time, u8 tag,
        // u64 allocation identity, zero padding. No Rust enum/pointer layout.
        assert_eq!(&record[..10], &[27, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&record[10..18], &raw.report.records[0].at_ns.to_le_bytes());
        assert_eq!(&record[18..27], &[1, 7, 0, 0, 0, 0, 0, 0, 0]);
        assert!(record[27..].iter().all(|byte| *byte == 0));
        assert_eq!(capture.close().records.len(), 1);
        Ok(())
    }

    #[test]
    fn private_capture_rejects_truncated_and_unknown_record_encoding() -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use std::os::unix::fs::FileExt;
        let mut collector = Collector::new(1)?;
        let fd = collector.take_worker_storage()?;
        let bytes = std::fs::File::from(fd.try_clone()?);
        let capture = Capture::from_mapped(MappedWriter::receive(fd)?)?;
        capture.record(EventKind::RenderAllocated { attempt: 1 });
        capture.finish();
        bytes.write_all_at(&25_u16.to_le_bytes(), HEADER_BYTES as u64)?;
        assert!(
            collector.snapshot().is_err(),
            "truncated typed payload was accepted"
        );
        bytes.write_all_at(&27_u16.to_le_bytes(), HEADER_BYTES as u64)?;
        bytes.write_all_at(&[255], HEADER_BYTES as u64 + 18)?;
        assert!(
            collector.snapshot().is_err(),
            "unknown event type was accepted"
        );
        bytes.write_all_at(&[1], HEADER_BYTES as u64 + 18)?;
        bytes.write_all_at(&[1], (HEADER_BYTES + RECORD_BYTES - 1) as u64)?;
        assert!(
            collector.snapshot().is_err(),
            "noncanonical padding was accepted"
        );
        Ok(())
    }

    #[test]
    fn private_capture_receiver_prefaults_its_own_mapping() -> Result<()> {
        use std::os::unix::fs::FileExt;
        let mut collector = Collector::new(128)?;
        let writer = MappedWriter::receive(collector.take_worker_storage()?)?;
        let page_bytes = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })?;
        let first_page = writer.map.0.as_ptr() as usize / page_bytes;
        let pages = writer.map.0.len().div_ceil(page_bytes);
        // Present bits are observable for our own mappings without PFN access
        // or privileges. mincore would only show backing-page residency, which
        // is insufficient: Collector already populated those backing pages.
        let pagemap = std::fs::File::open("/proc/self/pagemap")?;
        for page in 0..pages {
            let mut entry = [0; 8];
            pagemap.read_exact_at(&mut entry, ((first_page + page) as u64) * 8)?;
            assert_ne!(
                u64::from_ne_bytes(entry) & (1 << 63),
                0,
                "receiver page {page} lacks its own populated PTE"
            );
        }
        let raw = collector.snapshot()?;
        assert!(!raw.initialized && !raw.report.closed);
        assert!(raw.report.records.is_empty());
        assert_eq!(writer.map.get(0), MAGIC);
        assert_eq!(writer.map.get(2), WORKER_ROLE);
        assert_eq!(writer.map.get(4), 128);
        assert_eq!(writer.map.get(REVISION), 0);
        Ok(())
    }

    #[test]
    fn private_capture_counts_failed_minimal_spans_without_recounting_summaries() -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use crate::diagnostic_timing::TimingCapture;
        let mut collector = Collector::new(8)?;
        let capture =
            Capture::from_mapped(MappedWriter::receive(collector.take_worker_storage()?)?)?;
        let timing = TimingCapture::exporting(4, capture.clone())?;
        timing.begin(SpanKind::Present).finish(true, false);
        drop(timing.begin(SpanKind::RenderCallback));
        timing.close();
        timing.close();
        capture.finish();
        let raw = collector.snapshot()?;
        assert_eq!(
            raw.report.failed_operations, 2,
            "failed and abandoned samples count once each, repeated summaries add no failures"
        );
        assert_eq!(
            raw.report
                .records
                .iter()
                .filter(|r| matches!(r.kind, EventKind::MinimalSummary(_)))
                .count(),
            2
        );
        assert_eq!(raw.report.clock_failures, 0);
        Ok(())
    }

    #[test]
    fn private_capture_counts_fixture_failure_observations_not_phase_or_summaries() -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use crate::diagnostic_timing::TimingCapture;
        for synthetic in [false, true] {
            let mut collector = Collector::new(8)?;
            let capture =
                Capture::from_mapped(MappedWriter::receive(collector.take_worker_storage()?)?)?;
            for kind in [
                ObservationKind::Failed,
                ObservationKind::Resolved {
                    frame_id: 7,
                    resolution: Resolution::Failed,
                    resolved_ns: 80,
                },
                ObservationKind::Resolved {
                    frame_id: 8,
                    resolution: Resolution::Discarded,
                    resolved_ns: 81,
                },
                ObservationKind::Resolved {
                    frame_id: 9,
                    resolution: Resolution::Presented,
                    resolved_ns: 82,
                },
                ObservationKind::Phase(Phase::Ended),
                ObservationKind::Closed,
            ] {
                capture.record_at(
                    Ok(100),
                    EventKind::FixtureObserved {
                        synthetic,
                        decision_ns: 90,
                        kind,
                    },
                );
            }
            let timing = TimingCapture::exporting(1, capture.clone())?;
            timing.close();
            timing.close();
            capture.finish();
            let raw = collector.snapshot()?;
            assert_eq!(
                raw.report.failed_operations, 2,
                "Failed and Resolved(Failed) are separate failure observations, synthetic={synthetic}"
            );
            assert_eq!(raw.report.clock_failures, 0);
            assert_eq!(raw.report.records.len(), 8);
            assert_eq!(raw.report.lost, 0);
        }
        Ok(())
    }

    #[test]
    fn private_capture_bootstrap_preserves_typed_recording_level() -> Result<()> {
        for level in [CaptureLevel::Detailed, CaptureLevel::Minimal] {
            let mut collector = Collector::new(8)?;
            let fd = collector.take_worker_storage()?;
            let (sender, receiver) = UnixStream::pair()?;
            let deadline = Instant::now() + Duration::from_secs(1);
            send_bootstrap_storage(&sender, Some((&fd, level)), deadline)?;
            let (writer, received_level) = receive_bootstrap_storage(&receiver, deadline)?
                .context("requested recording level was dropped")?;
            assert_eq!(received_level, level);
            assert_eq!(writer.capacity(), 8);
            assert!(!collector.snapshot()?.initialized);
        }
        Ok(())
    }

    #[test]
    fn private_capture_encodes_calibration_separately_from_work() -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use crate::diagnostic_timing::TimingCapture;
        use std::os::unix::fs::FileExt;
        let mut collector = Collector::new(4)?;
        let fd = collector.take_worker_storage()?;
        let bytes = std::fs::File::from(fd.try_clone()?);
        let capture = Capture::from_mapped(MappedWriter::receive(fd)?)?;
        let timing = TimingCapture::exporting(1, capture.clone())?;
        timing.begin(SpanKind::Calibration).finish(false, true);
        let mut costs = [7; 32];
        costs[0] = 0;
        costs[31] = u64::MAX;
        capture.record(EventKind::MinimalCalibration { cost_ns: costs });
        timing.close();
        capture.finish();
        let raw = collector.snapshot()?;
        assert!(
            matches!(&raw.report.records[0].kind, EventKind::MinimalSpan(s)
            if s.kind == SpanKind::Calibration && !s.frame_bearing && s.status == SpanStatus::Succeeded)
        );
        assert!(
            matches!(&raw.report.records[1].kind, EventKind::MinimalCalibration { cost_ns } if *cost_ns == costs)
        );
        assert_eq!(raw.report.failed_operations, 0);
        let mut kind = [0; 1];
        bytes.read_exact_at(&mut kind, (HEADER_BYTES + 35) as u64)?;
        assert_eq!(kind, [3], "Calibration has its own span wire kind");
        let mut encoded = [0; RECORD_BYTES];
        bytes.read_exact_at(&mut encoded, (HEADER_BYTES + RECORD_BYTES) as u64)?;
        assert_eq!(&encoded[..2], &275_u16.to_le_bytes());
        assert_eq!(encoded[18], 27);
        assert_eq!(&encoded[19..27], &[0; 8]);
        assert_eq!(&encoded[267..275], &[255; 8]);
        assert!(encoded[275..].iter().all(|b| *b == 0));
        Ok(())
    }

    #[test]
    fn private_capture_preserves_fixture_endpoint_and_clock_domains() -> Result<()> {
        use crate::diagnostic_fixture::{ObservationKind, Resolution};
        use crate::diagnostic_observer::Capture;
        use std::os::unix::fs::FileExt;
        let mut collector = Collector::new(4)?;
        let fd = collector.take_worker_storage()?;
        let bytes = std::fs::File::from(fd.try_clone()?);
        let capture = Capture::from_mapped(MappedWriter::receive(fd)?)?;
        // Codec fixtures, not native observations: neither a false synthetic
        // flag nor these labels authenticate supplied timestamp/source values.
        capture.record_at(
            Ok(100),
            EventKind::FixtureObserved {
                synthetic: true,
                decision_ns: 90,
                kind: ObservationKind::Resolved {
                    frame_id: 7,
                    resolution: Resolution::Presented,
                    resolved_ns: 80,
                },
            },
        );
        capture.record_at(
            Ok(200),
            EventKind::FixtureObserved {
                synthetic: false,
                decision_ns: 190,
                kind: ObservationKind::ResponseConfirmed {
                    receipt_sequence: 3,
                    frame_id: 7,
                    token: 1,
                    responded_ns: 180,
                },
            },
        );
        capture.finish();
        let raw = collector.snapshot()?;
        assert_eq!(raw.report.records[0].at_ns, 100);
        assert_eq!(raw.report.records[1].at_ns, 200);
        assert!(matches!(
            raw.report.records[0].kind,
            EventKind::FixtureObserved {
                synthetic: true,
                decision_ns: 90,
                kind: ObservationKind::Resolved {
                    frame_id: 7,
                    resolution: Resolution::Presented,
                    resolved_ns: 80
                },
            }
        ));
        assert!(matches!(
            raw.report.records[1].kind,
            EventKind::FixtureObserved {
                synthetic: false,
                decision_ns: 190,
                kind: ObservationKind::ResponseConfirmed {
                    receipt_sequence: 3,
                    frame_id: 7,
                    token: 1,
                    responded_ns: 180
                },
            }
        ));
        let mut record = [0; RECORD_BYTES];
        bytes.read_exact_at(&mut record, HEADER_BYTES as u64)?;
        assert_eq!(record[18], 32);
        assert_eq!(record[19], 1);
        assert_eq!(&record[20..28], &90_u64.to_le_bytes());
        assert_eq!(record[28], 3, "Resolved has a fixed fixture subtag");
        assert_eq!(&record[38..46], &80_u64.to_le_bytes());
        Ok(())
    }

    #[test]
    fn private_capture_fixture_variants_roundtrip_in_fixed_records() -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use std::os::unix::fs::FileExt;
        // Independent v1 subtag/length expectations, including every nested enum.
        let mut cases = vec![
            (
                ObservationKind::Allocated {
                    frame_id: 0,
                    token: u64::MAX,
                },
                1,
                45,
            ),
            (ObservationKind::Suppressed, 2, 29),
            (ObservationKind::WarmupClosed, 4, 29),
            (
                ObservationKind::PeriodicRetired { timer_id: u64::MAX },
                5,
                37,
            ),
            (
                ObservationKind::Delivered {
                    receipt_sequence: u64::MAX,
                },
                7,
                37,
            ),
            (
                ObservationKind::TokenMutated {
                    receipt_sequence: 0,
                    previous: u64::MAX,
                    token: 1,
                },
                8,
                53,
            ),
            (
                ObservationKind::ResponseConfirmed {
                    receipt_sequence: 0,
                    frame_id: 1,
                    token: 2,
                    responded_ns: u64::MAX,
                },
                9,
                61,
            ),
            (ObservationKind::Failed, 10, 29),
            (ObservationKind::Closed, 11, 29),
        ];
        for phase in [
            Phase::Warmup,
            Phase::Quiescing,
            Phase::Armed,
            Phase::Measured,
            Phase::Drain,
            Phase::Ended,
        ] {
            cases.push((ObservationKind::Phase(phase), 0, 30));
        }
        for resolution in [
            Resolution::Presented,
            Resolution::Discarded,
            Resolution::Failed,
        ] {
            cases.push((
                ObservationKind::Resolved {
                    frame_id: u64::MAX,
                    resolution,
                    resolved_ns: 0,
                },
                3,
                46,
            ));
        }
        for phase in [
            TouchPhase::Down,
            TouchPhase::Move,
            TouchPhase::Up,
            TouchPhase::Cancel,
        ] {
            cases.push((
                ObservationKind::ReceiptSupplied {
                    receipt_sequence: u64::MAX,
                    received_ns: 0,
                    contact: u32::MAX,
                    phase,
                },
                6,
                50,
            ));
        }
        let mut collector = Collector::new(cases.len())?;
        let fd = collector.take_worker_storage()?;
        let bytes = std::fs::File::from(fd.try_clone()?);
        let capture = Capture::from_mapped(MappedWriter::receive(fd)?)?;
        for (index, (kind, _, _)) in cases.iter().enumerate() {
            capture.record_at(
                Ok(u64::MAX),
                EventKind::FixtureObserved {
                    synthetic: index % 2 == 0,
                    decision_ns: 0,
                    kind: *kind,
                },
            );
        }
        capture.finish();
        let raw = collector.snapshot()?;
        assert!(raw.metadata_consistent && raw.report.closed);
        assert_eq!(raw.report.records.len(), cases.len());
        assert_eq!(raw.report.lost, 0);
        assert_eq!(raw.storage_bytes, HEADER_BYTES + cases.len() * RECORD_BYTES);
        for (index, (expected, subtag, len)) in cases.iter().enumerate() {
            let record = &raw.report.records[index];
            assert_eq!(record.sequence, index as u64 + 1);
            assert_eq!(record.at_ns, u64::MAX);
            let EventKind::FixtureObserved {
                synthetic,
                decision_ns,
                kind,
            } = record.kind
            else {
                panic!("fixture record decoded as another event");
            };
            assert_eq!(synthetic, index % 2 == 0);
            assert_eq!(decision_ns, 0);
            assert_eq!(kind, *expected);
            let mut encoded = [0; RECORD_BYTES];
            bytes.read_exact_at(&mut encoded, (HEADER_BYTES + index * RECORD_BYTES) as u64)?;
            assert_eq!(u16::from_le_bytes(encoded[..2].try_into()?), *len as u16);
            assert_eq!(encoded[18], 32);
            assert_eq!(encoded[28], *subtag);
            assert!(encoded[*len..].iter().all(|byte| *byte == 0));
            match index {
                9..=14 => assert_eq!(encoded[29], (index - 9) as u8),
                15..=17 => assert_eq!(encoded[37], (index - 15) as u8),
                18..=21 => assert_eq!(encoded[49], (index - 18) as u8),
                _ => {}
            }
        }
        Ok(())
    }

    #[test]
    fn private_capture_rejects_malformed_calibration_and_fixture_records() -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use std::os::unix::fs::FileExt;
        let mut collector = Collector::new(1)?;
        let fd = collector.take_worker_storage()?;
        let bytes = std::fs::File::from(fd.try_clone()?);
        let capture = Capture::from_mapped(MappedWriter::receive(fd)?)?;
        capture.record_at(Ok(100), EventKind::MinimalCalibration { cost_ns: [0; 32] });
        capture.finish();
        let mut calibration = [0; RECORD_BYTES];
        bytes.read_exact_at(&mut calibration, HEADER_BYTES as u64)?;
        // Literal fixture packets exercise the decoder independently of its encoder.
        // All payload integers are zero; only v1 discriminants and lengths differ.
        let mut packets = vec![calibration];
        for (subtag, len) in [(0, 30_u16), (3, 46), (6, 50), (9, 61)] {
            let mut packet = [0; RECORD_BYTES];
            packet[..2].copy_from_slice(&len.to_le_bytes());
            packet[2] = 1; // sequence
            packet[18] = 32;
            packet[28] = subtag;
            packets.push(packet);
        }
        for original in packets {
            bytes.write_all_at(&original, HEADER_BYTES as u64)?;
            assert!(
                collector.snapshot().is_ok(),
                "valid packet must decode first"
            );
            let len = u16::from_le_bytes(original[..2].try_into()?) as usize;
            // Every truncated prefix, including inside any u64/u32 field, must
            // fail with canonical zero padding rather than reading beyond length.
            for short in 0..len {
                let mut malformed = original;
                malformed[short.max(2)..].fill(0);
                malformed[..2].copy_from_slice(&(short as u16).to_le_bytes());
                bytes.write_all_at(&malformed, HEADER_BYTES as u64)?;
                assert!(
                    collector.snapshot().is_err(),
                    "accepted tag {} length {short}",
                    original[18]
                );
            }
            for invalid_len in [len + 1, RECORD_BYTES, RECORD_BYTES + 1, u16::MAX as usize] {
                let mut malformed = original;
                malformed[..2].copy_from_slice(&(invalid_len as u16).to_le_bytes());
                bytes.write_all_at(&malformed, HEADER_BYTES as u64)?;
                assert!(
                    collector.snapshot().is_err(),
                    "accepted tag {} length {invalid_len}",
                    original[18]
                );
            }
            let mut invalid_bytes = vec![(RECORD_BYTES - 1, 1)];
            if original[18] == 32 {
                invalid_bytes.extend([(19, 2), (28, 12), (28, 255)]);
                match original[28] {
                    0 => invalid_bytes.push((29, 6)),
                    3 => invalid_bytes.push((37, 3)),
                    6 => invalid_bytes.push((49, 4)),
                    _ => {}
                }
            }
            for (offset, value) in invalid_bytes {
                let mut malformed = original;
                malformed[offset] = value;
                bytes.write_all_at(&malformed, HEADER_BYTES as u64)?;
                assert!(
                    collector.snapshot().is_err(),
                    "accepted tag {} byte {offset}={value}",
                    original[18]
                );
            }
            bytes.write_all_at(&original, HEADER_BYTES as u64)?;
            assert!(
                collector.snapshot().is_ok(),
                "malformed input must not consume the valid prefix"
            );
        }
        Ok(())
    }

    #[test]
    fn private_capture_correlated_publications_preserve_exact_allocation_and_reject_malformed_fields(
    ) -> Result<()> {
        use crate::diagnostic_observer::Capture;
        use crate::frame_slots::FrameAllocation;
        use std::os::unix::fs::FileExt;
        let allocation = FrameAllocation {
            id: 7,
            token: 3,
            allocated_ns: 123_456_789,
        };
        let mut collector = Collector::new(4)?;
        let fd = collector.take_worker_storage()?;
        let file = std::fs::File::from(fd.try_clone()?);
        let capture = Capture::from_mapped(MappedWriter::receive(fd)?)?;
        capture.record_at(
            Ok(200_000_000),
            EventKind::Published {
                sequence: 2,
                allocation: Some(allocation),
                marker: Err(DecodeError::Geometry),
            },
        );
        capture.record_at(
            Ok(200_000_001),
            EventKind::PendingDiscarded {
                allocation: Some(allocation),
                marker: Err(DecodeError::Geometry),
            },
        );
        capture.record_at(
            Ok(200_000_002),
            EventKind::Published {
                sequence: 3,
                allocation: None,
                marker: Err(DecodeError::Geometry),
            },
        );
        capture.finish();
        let report = collector.snapshot()?;
        assert!(matches!(report.report.records[0].kind,
            EventKind::Published { sequence: 2, allocation: Some(actual), .. } if actual == allocation));
        assert!(matches!(report.report.records[1].kind,
            EventKind::PendingDiscarded { allocation: Some(actual), .. } if actual == allocation));
        assert!(matches!(
            report.report.records[2].kind,
            EventKind::Published {
                sequence: 3,
                allocation: None,
                ..
            }
        ));
        let mut original = [0; RECORD_BYTES];
        file.read_exact_at(&mut original, HEADER_BYTES as u64)?;
        // Independent offsets: 2-byte length, sequence/time, tag3, publication
        // sequence, allocation tag, id/token/host-ns, then marker result.
        assert_eq!(original[18], 3);
        assert_eq!(original[27], 1);
        for (offset, value) in [
            (28, 0),
            (28, u64::MAX),
            (36, u64::MAX),
            (44, 0),
            (44, u64::MAX),
        ] {
            let mut malformed = original;
            malformed[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            file.write_all_at(&malformed, HEADER_BYTES as u64)?;
            assert!(
                collector.snapshot().is_err(),
                "accepted malformed allocation at {offset}"
            );
        }
        let mut malformed = original;
        malformed[27] = 2;
        file.write_all_at(&malformed, HEADER_BYTES as u64)?;
        assert!(collector.snapshot().is_err());
        file.write_all_at(&original, HEADER_BYTES as u64)?;
        assert!(collector.snapshot().is_ok());
        // A matched new binary must not claim compatibility with old raw storage.
        file.write_all_at(&1_u64.to_le_bytes(), 8)?;
        let error = MappedWriter::receive(file.try_clone()?.into())
            .err()
            .context("accepted old raw version")?;
        assert!(error
            .to_string()
            .contains("unsupported raw storage version"));
        Ok(())
    }

    #[test]
    fn private_capture_bootstrap_honors_expired_deadline() -> Result<()> {
        let (sender, receiver) = UnixStream::pair()?;
        let expired = Instant::now() - Duration::from_millis(1);
        assert!(send_bootstrap_storage(&sender, None, expired).is_err());
        send_bootstrap_storage(&sender, None, Instant::now() + Duration::from_secs(1))?;
        assert!(receive_bootstrap_storage(&receiver, expired).is_err());
        assert!(
            receive_bootstrap_storage(&receiver, Instant::now() + Duration::from_secs(1))?
                .is_none()
        );
        Ok(())
    }
}
