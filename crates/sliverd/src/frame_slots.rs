use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
#[cfg(test)]
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use memmap2::MmapMut;

const SLOT_COUNT: usize = 3;
const FREE: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;
const READING: u8 = 3;
const RECLAIMING: u8 = 4;
#[cfg(test)]
const MAX_FRAME_SLOT_WAIT: Duration = Duration::from_millis(50);

#[cfg(test)]
struct DropGate {
    released: std::sync::Barrier,
    resume: std::sync::Barrier,
}

#[cfg(test)]
impl DropGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            released: std::sync::Barrier::new(2),
            resume: std::sync::Barrier::new(2),
        })
    }
}

#[cfg(test)]
struct SelectionGate {
    selected: std::sync::Barrier,
    resume: std::sync::Barrier,
    active: AtomicU8,
}

#[cfg(test)]
impl SelectionGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            selected: std::sync::Barrier::new(2),
            resume: std::sync::Barrier::new(2),
            active: AtomicU8::new(1),
        })
    }
}

// One-shot, bounded interleaving gate for independently opened shared maps.
#[cfg(test)]
struct SharedSelectionGate {
    reached: std::sync::mpsc::Sender<()>,
    resume: Mutex<std::sync::mpsc::Receiver<()>>,
    active: AtomicU8,
}

#[cfg(test)]
impl SharedSelectionGate {
    fn new() -> (
        Self,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (reached, notification) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        (
            Self {
                reached,
                resume: Mutex::new(resume),
                active: AtomicU8::new(1),
            },
            notification,
            release,
        )
    }

    fn pause_once(&self) -> Result<()> {
        if self.active.swap(0, Ordering::AcqRel) != 0 {
            self.reached.send(())?;
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .context("timed out resuming shared frame selection")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FrameTiming {
    pub(crate) presentation_time: f64,
    pub(crate) delta: f64,
}

impl FrameTiming {
    pub(crate) fn new(presentation_time: f64, delta: f64) -> Result<Self> {
        ensure!(
            presentation_time.is_finite() && presentation_time >= 0.0,
            "frame presentation time must be finite and non-negative"
        );
        ensure!(
            delta.is_finite() && delta >= 0.0,
            "frame delta must be finite and non-negative"
        );
        Ok(Self {
            presentation_time,
            delta,
        })
    }
}

/// Private fixture identity, sampled before rendering, never inferred from pixels
/// or successful publications. IDs are scoped to the provisioned worker/mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameAllocation {
    pub(crate) id: u64,
    pub(crate) token: u64,
    pub(crate) allocated_ns: u64,
}

impl FrameAllocation {
    fn validate(self) -> Result<()> {
        ensure!(
            (1..=i64::MAX as u64).contains(&self.id)
                && self.token <= i64::MAX as u64
                && (1..=i64::MAX as u64).contains(&self.allocated_ns),
            "invalid fixture allocation metadata"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameCorrelation {
    pub(crate) allocation: FrameAllocation,
    pub(crate) sequence: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompletedFrame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) stride: usize,
    pub(crate) pixels: Vec<u8>,
    pub(crate) timing: FrameTiming,
    pub(crate) fixture_correlation: Option<FrameCorrelation>,
}

struct Slot {
    state: AtomicU8,
    sequence: AtomicU64,
    timing: Mutex<Option<(FrameTiming, Option<FrameAllocation>)>>,
}

struct SharedSlots {
    observer: Option<crate::diagnostic_observer::Capture>,
    timing: Option<crate::diagnostic_timing::TimingCapture>,
    storage: Mutex<MmapMut>,
    slot_bytes: usize,
    width: usize,
    height: usize,
    stride: usize,
    next_sequence: AtomicU64,
    #[cfg(test)]
    state_wait: Mutex<()>,
    state_changed: Condvar,
    slots: [Slot; SLOT_COUNT],
    #[cfg(test)]
    drop_gate: Option<Arc<DropGate>>,
    #[cfg(test)]
    selection_gate: Option<Arc<SelectionGate>>,
}

/// The fixed-size producer/broker handoff for decoded frames.
///
/// Pixel bytes live in one anonymous mmap split into three fixed slots. State
/// transitions publish a slot only after the complete row range has been
/// copied. The broker never reads a slot in `WRITING`, so a producer that
/// disappears halfway through a copy cannot expose those bytes. Worker death
/// detection and the supervisor's recovery reaction belong to #10 and #11.
#[derive(Clone)]
pub(crate) struct FrameSlots {
    inner: Arc<SharedSlots>,
    shared: Option<Arc<SharedFrameMap>>,
}

#[derive(Clone)]
pub(crate) struct FrameProducer {
    inner: Arc<SharedSlots>,
    shared: Option<Arc<SharedFrameMap>>,
}

#[derive(Clone)]
pub(crate) struct FrameBroker {
    inner: Arc<SharedSlots>,
    shared: Option<Arc<SharedFrameMap>>,
}

pub(crate) struct FrameWriter {
    inner: Arc<SharedSlots>,
    index: usize,
    published: bool,
}

// Matched private worker/supervisor binaries; never accept the old layout.
const SHARED_MAGIC: &[u8; 8] = b"SLVRFRM2";
const SHARED_HEADER_BYTES: usize = 128;
const SHARED_SLOT_META_BYTES: usize = 64;
const SHARED_PIXEL_OFFSET: usize = SHARED_HEADER_BYTES + SLOT_COUNT * SHARED_SLOT_META_BYTES;

struct SharedFrameMap {
    observer: Option<crate::diagnostic_observer::Capture>,
    storage: Mutex<MmapMut>,
    path: Option<PathBuf>,
    #[cfg(test)]
    snapshot_gate: Option<SharedSelectionGate>,
    #[cfg(test)]
    reading_gate: Option<SharedSelectionGate>,
    slot_bytes: usize,
    width: usize,
    height: usize,
    stride: usize,
}

impl SharedFrameMap {
    fn create(path: &Path, width: usize, height: usize, stride: usize) -> Result<Self> {
        let slot_bytes = stride
            .checked_mul(height)
            .ok_or_else(|| anyhow::anyhow!("shared frame slot size overflows usize"))?;
        let map_bytes = SHARED_PIXEL_OFFSET
            .checked_add(
                slot_bytes
                    .checked_mul(SLOT_COUNT)
                    .ok_or_else(|| anyhow::anyhow!("shared frame mapping size overflows usize"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("shared frame mapping size overflows usize"))?;
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)?;
        file.set_len(map_bytes as u64)?;
        let mut storage = unsafe { MmapMut::map_mut(&file)? };
        storage[..8].copy_from_slice(SHARED_MAGIC);
        write_u32(&mut storage, 8, width)?;
        write_u32(&mut storage, 12, height)?;
        write_u32(&mut storage, 16, stride)?;
        write_u32(&mut storage, 20, slot_bytes)?;
        unsafe {
            (&*(storage.as_ptr().add(32) as *const AtomicU64)).store(0, Ordering::Relaxed);
        }
        for index in 0..SLOT_COUNT {
            write_slot_state(&storage, index, FREE);
            write_u64(&mut storage, slot_offset(index) + 8, 0);
            write_u64(&mut storage, slot_offset(index) + 16, 0);
            write_u64(&mut storage, slot_offset(index) + 24, 0);
        }
        storage.flush()?;
        Ok(Self {
            storage: Mutex::new(storage),
            observer: None,
            path: Some(path.to_path_buf()),
            #[cfg(test)]
            snapshot_gate: None,
            #[cfg(test)]
            reading_gate: None,
            slot_bytes,
            width,
            height,
            stride,
        })
    }

    fn open(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let storage = unsafe { MmapMut::map_mut(&file)? };
        ensure!(
            storage.len() >= SHARED_PIXEL_OFFSET,
            "shared frame mapping is truncated"
        );
        ensure!(
            &storage[..8] == SHARED_MAGIC,
            "shared frame mapping has an invalid magic"
        );
        let width = read_u32(&storage, 8)?;
        let height = read_u32(&storage, 12)?;
        let stride = read_u32(&storage, 16)?;
        let slot_bytes = read_u32(&storage, 20)?;
        ensure!(
            width > 0 && height > 0,
            "shared frame dimensions are invalid"
        );
        ensure!(
            stride >= width.saturating_mul(4),
            "shared frame stride is invalid"
        );
        ensure!(
            slot_bytes == stride.saturating_mul(height),
            "shared frame slot size is invalid"
        );
        ensure!(
            storage.len()
                == SHARED_PIXEL_OFFSET
                    .checked_add(slot_bytes.saturating_mul(SLOT_COUNT))
                    .context("shared frame mapping size overflows usize")?,
            "shared frame mapping has an invalid size"
        );
        Ok(Self {
            storage: Mutex::new(storage),
            observer: None,
            path: None,
            #[cfg(test)]
            snapshot_gate: None,
            #[cfg(test)]
            reading_gate: None,
            slot_bytes,
            width,
            height,
            stride,
        })
    }

    fn try_publish(
        &self,
        pixels: &[u8],
        timing: FrameTiming,
        allocation: Option<FrameAllocation>,
    ) -> Result<bool> {
        ensure!(
            pixels.len() == self.slot_bytes,
            "frame pixels do not fill one shared slot"
        );
        let Some(index) = self.claim_write()? else {
            return Ok(false);
        };
        {
            let mut storage = self
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            let offset = pixel_offset(index, self.slot_bytes);
            storage[offset..offset + self.slot_bytes].copy_from_slice(pixels);
            let sequence = unsafe {
                (&*(storage.as_ptr().add(32) as *const AtomicU64))
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                        value.checked_add(1)
                    })
                    .map_err(|_| anyhow::anyhow!("shared publication sequence exhausted"))?
                    + 1
            };
            write_u64(&mut storage, slot_offset(index) + 8, sequence);
            write_u64(
                &mut storage,
                slot_offset(index) + 16,
                timing.presentation_time.to_bits(),
            );
            write_u64(
                &mut storage,
                slot_offset(index) + 24,
                timing.delta.to_bits(),
            );
            write_allocation(&mut storage, index, allocation);
            if let Some(observer) = &self.observer {
                let frame = crate::hardware::LogicalFrame::from_wire(
                    self.width,
                    self.height,
                    self.stride,
                    pixels.to_vec(),
                );
                observer.record(crate::diagnostic_observer::EventKind::Published {
                    sequence,
                    marker: crate::diagnostic_observer::decode(&frame),
                    allocation,
                });
            }
            write_slot_state(&storage, index, READY);
        }
        Ok(true)
    }

    fn claim_write(&self) -> Result<Option<usize>> {
        for state in [FREE, READY] {
            let candidate = if state == READY {
                let storage = self
                    .storage
                    .lock()
                    .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
                (0..SLOT_COUNT)
                    .filter(|index| read_slot_state(&storage, *index) == READY)
                    .min_by_key(|index| read_u64(&storage, slot_offset(*index) + 8).unwrap_or(0))
            } else {
                None
            };
            let indexes = candidate
                .map(|candidate| {
                    std::iter::once(candidate)
                        .chain((0..SLOT_COUNT).filter(move |index| *index != candidate))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| (0..SLOT_COUNT).collect());
            for index in indexes {
                let storage = self
                    .storage
                    .lock()
                    .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
                if read_slot_state(&storage, index) != state {
                    continue;
                }
                if compare_slot_state(&storage, index, state, WRITING) {
                    if state == READY {
                        if let Some(observer) = &self.observer {
                            observer.record(crate::diagnostic_observer::EventKind::Discarded {
                                sequence: read_u64(&storage, slot_offset(index) + 8)?,
                                reason: crate::diagnostic_observer::DiscardReason::ProducerReclaim,
                            });
                        }
                    }
                    return Ok(Some(index));
                }
            }
        }
        Ok(None)
    }

    fn take_newest(&self) -> Result<Option<CompletedFrame>> {
        // Selection and ownership must be separate operations. Another
        // process may publish or reclaim a slot after the snapshot, so retry
        // when the selected READY slot no longer belongs to us.
        let index = loop {
            let newest = {
                let storage = self
                    .storage
                    .lock()
                    .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
                (0..SLOT_COUNT)
                    .filter(|index| read_slot_state(&storage, *index) == READY)
                    .max_by_key(|index| read_u64(&storage, slot_offset(*index) + 8).unwrap_or(0))
            };
            let Some(index) = newest else {
                return Ok(None);
            };
            #[cfg(test)]
            if let Some(gate) = &self.snapshot_gate {
                gate.pause_once()?;
            }
            let storage = self
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            if compare_slot_state(&storage, index, READY, READING) {
                break index;
            }
        };
        let selected_sequence = {
            let storage = self
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            read_u64(&storage, slot_offset(index) + 8)?
        };
        if let Some(observer) = &self.observer {
            observer.record(crate::diagnostic_observer::EventKind::Selected {
                sequence: selected_sequence,
            });
        }
        #[cfg(test)]
        if let Some(gate) = &self.reading_gate {
            gate.pause_once()?;
        }
        let (pixels, timing, allocation) = {
            let storage = self
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            let offset = pixel_offset(index, self.slot_bytes);
            let pixels = storage[offset..offset + self.slot_bytes].to_vec();
            let timing = FrameTiming::new(
                f64::from_bits(read_u64(&storage, slot_offset(index) + 16)?),
                f64::from_bits(read_u64(&storage, slot_offset(index) + 24)?),
            )?;
            ensure!(
                selected_sequence != 0,
                "invalid shared publication sequence"
            );
            (pixels, timing, read_allocation(&storage, index)?)
        };
        {
            let storage = self
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            write_slot_state(&storage, index, FREE);
            for older in 0..SLOT_COUNT {
                if older == index || !compare_slot_state(&storage, older, READY, RECLAIMING) {
                    continue;
                }
                let older_sequence = read_u64(&storage, slot_offset(older) + 8)?;
                // RECLAIMING closes the gap between observing READY and
                // freeing it. A producer in another process can no longer
                // turn this slot into WRITING while we inspect its sequence.
                if older_sequence < selected_sequence {
                    if let Some(observer) = &self.observer {
                        observer.record(crate::diagnostic_observer::EventKind::Discarded {
                            sequence: older_sequence,
                            reason: crate::diagnostic_observer::DiscardReason::ConsumerSuperseded,
                        });
                    }
                    write_slot_state(&storage, older, FREE);
                } else {
                    write_slot_state(&storage, older, READY);
                }
            }
        }
        Ok(Some(CompletedFrame {
            width: self.width,
            height: self.height,
            stride: self.stride,
            pixels,
            timing,
            fixture_correlation: allocation.map(|allocation| FrameCorrelation {
                allocation,
                sequence: selected_sequence,
            }),
        }))
    }
}

impl Drop for SharedSlots {
    fn drop(&mut self) {
        if let Some(observer) = &self.observer {
            for slot in &self.slots {
                if slot.state.load(Ordering::Acquire) == READY {
                    observer.record(crate::diagnostic_observer::EventKind::Discarded {
                        sequence: slot.sequence.load(Ordering::Acquire),
                        reason: crate::diagnostic_observer::DiscardReason::MappingClosed,
                    });
                }
            }
        }
    }
}

impl Drop for SharedFrameMap {
    fn drop(&mut self) {
        // Only the creator owns global mapping teardown, after producer join.
        // A worker opener can disappear while the owner still consumes READY.
        if let Some(observer) = self.observer.as_ref().filter(|_| self.path.is_some()) {
            if let Ok(storage) = self.storage.lock() {
                for index in 0..SLOT_COUNT {
                    if read_slot_state(&storage, index) == READY {
                        if let Ok(sequence) = read_u64(&storage, slot_offset(index) + 8) {
                            observer.record(crate::diagnostic_observer::EventKind::Discarded {
                                sequence,
                                reason: crate::diagnostic_observer::DiscardReason::MappingClosed,
                            });
                        }
                    }
                }
            }
        }
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn slot_offset(index: usize) -> usize {
    SHARED_HEADER_BYTES + index * SHARED_SLOT_META_BYTES
}

fn pixel_offset(index: usize, slot_bytes: usize) -> usize {
    SHARED_PIXEL_OFFSET + index * slot_bytes
}

fn read_slot_state(storage: &[u8], index: usize) -> u8 {
    unsafe {
        (&*(storage.as_ptr().add(slot_offset(index)) as *const AtomicU8)).load(Ordering::Acquire)
    }
}

fn write_slot_state(storage: &[u8], index: usize, state: u8) {
    unsafe {
        (&*(storage.as_ptr().add(slot_offset(index)) as *const AtomicU8))
            .store(state, Ordering::Release)
    }
}

fn compare_slot_state(storage: &[u8], index: usize, old: u8, new: u8) -> bool {
    unsafe {
        (&*(storage.as_ptr().add(slot_offset(index)) as *const AtomicU8))
            .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

fn read_u32(storage: &[u8], offset: usize) -> Result<usize> {
    Ok(u32::from_ne_bytes(storage[offset..offset + 4].try_into()?) as usize)
}

fn write_u32(storage: &mut [u8], offset: usize, value: usize) -> Result<()> {
    storage[offset..offset + 4].copy_from_slice(&u32::try_from(value)?.to_ne_bytes());
    Ok(())
}

fn read_u64(storage: &[u8], offset: usize) -> Result<u64> {
    ensure!(
        offset.is_multiple_of(8) && offset + 8 <= storage.len(),
        "invalid mapped word offset"
    );
    // Selection snapshots may race a reclaiming producer's sequence update.
    // Atomic words avoid torn/data-racing sequence reads; slot ownership still
    // gates the pixel and allocation snapshot used by the consumer.
    Ok(unsafe { (&*(storage.as_ptr().add(offset) as *const AtomicU64)).load(Ordering::Relaxed) })
}

fn write_u64(storage: &mut [u8], offset: usize, value: u64) {
    assert!(offset.is_multiple_of(8) && offset + 8 <= storage.len());
    unsafe {
        (&*(storage.as_ptr().add(offset) as *const AtomicU64)).store(value, Ordering::Relaxed)
    }
}

fn write_allocation(storage: &mut [u8], index: usize, allocation: Option<FrameAllocation>) {
    let base = slot_offset(index);
    write_u64(storage, base + 32, u64::from(allocation.is_some()));
    write_u64(storage, base + 40, allocation.map_or(0, |value| value.id));
    write_u64(
        storage,
        base + 48,
        allocation.map_or(0, |value| value.token),
    );
    write_u64(
        storage,
        base + 56,
        allocation.map_or(0, |value| value.allocated_ns),
    );
}

fn read_allocation(storage: &[u8], index: usize) -> Result<Option<FrameAllocation>> {
    let base = slot_offset(index);
    let tag = read_u64(storage, base + 32)?;
    let allocation = FrameAllocation {
        id: read_u64(storage, base + 40)?,
        token: read_u64(storage, base + 48)?,
        allocated_ns: read_u64(storage, base + 56)?,
    };
    match tag {
        0 => {
            ensure!(
                allocation.id == 0 && allocation.token == 0 && allocation.allocated_ns == 0,
                "absent fixture allocation has nonzero metadata"
            );
            Ok(None)
        }
        1 => {
            allocation.validate()?;
            Ok(Some(allocation))
        }
        _ => anyhow::bail!("unknown fixture allocation metadata tag"),
    }
}

fn validate_dimensions(width: usize, height: usize, stride: usize) -> Result<usize> {
    ensure!(width > 0, "frame width must be positive");
    ensure!(height > 0, "frame height must be positive");
    ensure!(
        stride >= width.saturating_mul(4),
        "frame stride is too small"
    );
    stride
        .checked_mul(height)
        .ok_or_else(|| anyhow::anyhow!("frame slot size overflows usize"))
}

fn make_inner(
    width: usize,
    height: usize,
    stride: usize,
    slot_bytes: usize,
    storage: MmapMut,
) -> Arc<SharedSlots> {
    let slots = std::array::from_fn(|_| Slot {
        state: AtomicU8::new(FREE),
        sequence: AtomicU64::new(0),
        timing: Mutex::new(None),
    });
    Arc::new(SharedSlots {
        observer: None,
        timing: None,
        storage: Mutex::new(storage),
        slot_bytes,
        width,
        height,
        stride,
        next_sequence: AtomicU64::new(0),
        #[cfg(test)]
        state_wait: Mutex::new(()),
        state_changed: Condvar::new(),
        slots,
        #[cfg(test)]
        drop_gate: None,
        #[cfg(test)]
        selection_gate: None,
    })
}

impl FrameSlots {
    pub(crate) fn with_timing(mut self, timing: crate::diagnostic_timing::TimingCapture) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("unshared slots")
            .timing = Some(timing);
        self
    }

    pub(crate) fn with_observer(mut self, observer: crate::diagnostic_observer::Capture) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("unshared slots")
            .observer = Some(observer.clone());
        if let Some(shared) = &mut self.shared {
            Arc::get_mut(shared).expect("unshared map").observer = Some(observer);
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn new(width: usize, height: usize, stride: usize) -> Result<Self> {
        let slot_bytes = validate_dimensions(width, height, stride)?;
        let map_bytes = slot_bytes
            .checked_mul(SLOT_COUNT)
            .ok_or_else(|| anyhow::anyhow!("frame slot mapping size overflows usize"))?;
        Ok(Self {
            inner: make_inner(
                width,
                height,
                stride,
                slot_bytes,
                MmapMut::map_anon(map_bytes)?,
            ),
            shared: None,
        })
    }

    pub(crate) fn new_shared(
        path: &Path,
        width: usize,
        height: usize,
        stride: usize,
    ) -> Result<Self> {
        let slot_bytes = validate_dimensions(width, height, stride)?;
        let shared = SharedFrameMap::create(path, width, height, stride)?;
        Ok(Self {
            inner: make_inner(width, height, stride, slot_bytes, MmapMut::map_anon(1)?),
            shared: Some(Arc::new(shared)),
        })
    }

    pub(crate) fn open_shared(path: &Path) -> Result<Self> {
        let shared = SharedFrameMap::open(path)?;
        let slot_bytes = validate_dimensions(shared.width, shared.height, shared.stride)?;
        Ok(Self {
            inner: make_inner(
                shared.width,
                shared.height,
                shared.stride,
                slot_bytes,
                MmapMut::map_anon(1)?,
            ),
            shared: Some(Arc::new(shared)),
        })
    }

    #[cfg(test)]
    fn new_with_drop_gate(
        width: usize,
        height: usize,
        stride: usize,
        drop_gate: Arc<DropGate>,
    ) -> Result<Self> {
        let mut slots = Self::new(width, height, stride)?;
        Arc::get_mut(&mut slots.inner)
            .expect("new slots have one owner")
            .drop_gate = Some(drop_gate);
        Ok(slots)
    }

    #[cfg(test)]
    fn new_with_selection_gate(
        width: usize,
        height: usize,
        stride: usize,
        selection_gate: Arc<SelectionGate>,
    ) -> Result<Self> {
        let mut slots = Self::new(width, height, stride)?;
        Arc::get_mut(&mut slots.inner)
            .expect("new slots have one owner")
            .selection_gate = Some(selection_gate);
        Ok(slots)
    }

    pub(crate) fn producer(&self) -> FrameProducer {
        FrameProducer {
            inner: self.inner.clone(),
            shared: self.shared.clone(),
        }
    }

    pub(crate) fn broker(&self) -> FrameBroker {
        FrameBroker {
            inner: self.inner.clone(),
            shared: self.shared.clone(),
        }
    }
}

impl FrameProducer {
    pub(crate) fn timing(&self) -> Option<&crate::diagnostic_timing::TimingCapture> {
        self.inner.timing.as_ref()
    }

    pub(crate) fn observer(&self) -> Option<&crate::diagnostic_observer::Capture> {
        self.inner.observer.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn hold_slots_for_test(&self) -> Vec<FrameWriter> {
        (0..SLOT_COUNT).filter_map(|_| self.begin_write()).collect()
    }

    pub(crate) fn try_publish(
        &self,
        width: usize,
        height: usize,
        stride: usize,
        pixels: &[u8],
        timing: FrameTiming,
    ) -> Result<bool> {
        self.try_publish_fixture(width, height, stride, pixels, timing, None)
    }

    pub(crate) fn try_publish_fixture(
        &self,
        width: usize,
        height: usize,
        stride: usize,
        pixels: &[u8],
        timing: FrameTiming,
        allocation: Option<FrameAllocation>,
    ) -> Result<bool> {
        self.validate_frame(width, height, stride, pixels)?;
        if let Some(allocation) = allocation {
            allocation.validate()?;
        }
        if let Some(shared) = &self.shared {
            return shared.try_publish(pixels, timing, allocation);
        }
        let Some(mut writer) = self.begin_write() else {
            return Ok(false);
        };
        writer.write_complete(pixels)?;
        writer.publish_fixture(timing, allocation)?;
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn publish(
        &self,
        width: usize,
        height: usize,
        stride: usize,
        pixels: &[u8],
        timing: FrameTiming,
    ) -> Result<bool> {
        self.validate_frame(width, height, stride, pixels)?;
        let deadline = Instant::now() + MAX_FRAME_SLOT_WAIT;
        loop {
            if self.publish_once(pixels, timing)? {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let wait = self
                .inner
                .state_wait
                .lock()
                .map_err(|_| anyhow::anyhow!("frame slot wait state was poisoned"))?;
            if self.publish_once(pixels, timing)? {
                drop(wait);
                return Ok(true);
            }
            let (_wait, result) = self
                .inner
                .state_changed
                .wait_timeout(wait, remaining)
                .map_err(|_| anyhow::anyhow!("frame slot wait state was poisoned"))?;
            if result.timed_out() {
                return Ok(false);
            }
        }
    }

    fn validate_frame(
        &self,
        width: usize,
        height: usize,
        stride: usize,
        pixels: &[u8],
    ) -> Result<()> {
        ensure!(
            (width, height, stride) == (self.inner.width, self.inner.height, self.inner.stride),
            "frame dimensions do not match the shared slots"
        );
        ensure!(
            pixels.len() == self.inner.slot_bytes,
            "frame pixels do not fill one shared slot"
        );
        Ok(())
    }

    #[cfg(test)]
    fn publish_once(&self, pixels: &[u8], timing: FrameTiming) -> Result<bool> {
        let Some(mut writer) = self.begin_write() else {
            return Ok(false);
        };
        writer.write_complete(pixels)?;
        writer.publish(timing)?;
        Ok(true)
    }

    fn begin_write(&self) -> Option<FrameWriter> {
        let index = self
            .inner
            .slots
            .iter()
            .position(|slot| {
                slot.state
                    .compare_exchange(FREE, WRITING, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            })
            .or_else(|| {
                self.inner
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(_, slot)| slot.state.load(Ordering::Acquire) == READY)
                    .min_by_key(|(_, slot)| slot.sequence.load(Ordering::Acquire))
                    .and_then(|(index, slot)| {
                        slot.state
                            .compare_exchange(READY, WRITING, Ordering::Acquire, Ordering::Relaxed)
                            .ok()
                            .map(|_| {
                                if let Some(observer) = &self.inner.observer {
                                    observer.record(crate::diagnostic_observer::EventKind::Discarded {
                                        sequence: slot.sequence.load(Ordering::Acquire),
                                        reason: crate::diagnostic_observer::DiscardReason::ProducerReclaim,
                                    });
                                }
                                index
                            })
                    })
            })?;
        Some(FrameWriter {
            inner: self.inner.clone(),
            index,
            published: false,
        })
    }
}

impl FrameWriter {
    fn write_complete(&mut self, pixels: &[u8]) -> Result<()> {
        ensure!(
            pixels.len() == self.inner.slot_bytes,
            "frame pixels do not fill one shared slot"
        );
        self.write_bytes(pixels)
    }

    fn write_bytes(&mut self, pixels: &[u8]) -> Result<()> {
        ensure!(
            pixels.len() <= self.inner.slot_bytes,
            "partial frame write exceeds one shared slot"
        );
        let offset = self.index * self.inner.slot_bytes;
        let mut storage = self
            .inner
            .storage
            .lock()
            .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
        storage[offset..offset + pixels.len()].copy_from_slice(pixels);
        Ok(())
    }

    #[cfg(test)]
    fn publish(&mut self, timing: FrameTiming) -> Result<()> {
        self.publish_fixture(timing, None)
    }

    fn publish_fixture(
        &mut self,
        timing: FrameTiming,
        allocation: Option<FrameAllocation>,
    ) -> Result<()> {
        let sequence = self
            .inner
            .next_sequence
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| anyhow::anyhow!("publication sequence exhausted"))?
            + 1;
        let slot = &self.inner.slots[self.index];
        *slot
            .timing
            .lock()
            .map_err(|_| anyhow::anyhow!("shared frame metadata was poisoned"))? =
            Some((timing, allocation));
        slot.sequence.store(sequence, Ordering::Relaxed);
        if let Some(observer) = &self.inner.observer {
            let storage = self
                .inner
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            let offset = self.index * self.inner.slot_bytes;
            let frame = crate::hardware::LogicalFrame::from_wire(
                self.inner.width,
                self.inner.height,
                self.inner.stride,
                storage[offset..offset + self.inner.slot_bytes].to_vec(),
            );
            observer.record(crate::diagnostic_observer::EventKind::Published {
                sequence,
                marker: crate::diagnostic_observer::decode(&frame),
                allocation,
            });
        }
        slot.state.store(READY, Ordering::Release);
        self.inner.state_changed.notify_all();
        self.published = true;
        Ok(())
    }
}

impl Drop for FrameWriter {
    fn drop(&mut self) {
        if !self.published {
            let slot = &self.inner.slots[self.index];
            if let Ok(mut timing) = slot.timing.lock() {
                *timing = None;
            }
            let _ =
                slot.state
                    .compare_exchange(WRITING, FREE, Ordering::Release, Ordering::Relaxed);
            self.inner.state_changed.notify_all();
            #[cfg(test)]
            if let Some(drop_gate) = &self.inner.drop_gate {
                drop_gate.released.wait();
                drop_gate.resume.wait();
            }
        }
    }
}

impl FrameBroker {
    pub(crate) fn take_newest(&self) -> Result<Option<CompletedFrame>> {
        if let Some(shared) = &self.shared {
            return shared.take_newest();
        }
        let newest = self
            .inner
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.state.load(Ordering::Acquire) == READY)
            .max_by_key(|(_, slot)| slot.sequence.load(Ordering::Acquire))
            .map(|(index, _)| index);
        let Some(index) = newest else {
            return Ok(None);
        };

        let newest_slot = &self.inner.slots[index];
        if newest_slot
            .state
            .compare_exchange(READY, READING, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Ok(None);
        }
        let selected_sequence = newest_slot.sequence.load(Ordering::Acquire);
        if let Some(observer) = &self.inner.observer {
            observer.record(crate::diagnostic_observer::EventKind::Selected {
                sequence: selected_sequence,
            });
        }

        for (older_index, slot) in self.inner.slots.iter().enumerate() {
            if older_index == index {
                continue;
            }
            if slot
                .state
                .compare_exchange(READY, RECLAIMING, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            let observed_sequence = slot.sequence.load(Ordering::Acquire);
            #[cfg(test)]
            if let Some(selection_gate) = &self.inner.selection_gate {
                if observed_sequence < selected_sequence
                    && selection_gate.active.swap(0, Ordering::AcqRel) != 0
                {
                    selection_gate.selected.wait();
                    selection_gate.resume.wait();
                }
            }
            if observed_sequence < selected_sequence {
                if let Some(observer) = &self.inner.observer {
                    observer.record(crate::diagnostic_observer::EventKind::Discarded {
                        sequence: observed_sequence,
                        reason: crate::diagnostic_observer::DiscardReason::ConsumerSuperseded,
                    });
                }
                slot.state.store(FREE, Ordering::Release);
            } else {
                slot.state.store(READY, Ordering::Release);
            }
            self.inner.state_changed.notify_all();
        }

        let offset = index * self.inner.slot_bytes;
        let pixels = match self.inner.storage.lock() {
            Ok(storage) => storage[offset..offset + self.inner.slot_bytes].to_vec(),
            Err(_) => {
                newest_slot.state.store(FREE, Ordering::Release);
                self.inner.state_changed.notify_all();
                return Err(anyhow::anyhow!("shared frame storage was poisoned"));
            }
        };
        let (timing, allocation) = match newest_slot.timing.lock() {
            Ok(mut timing) => match timing.take() {
                Some(timing) => timing,
                None => {
                    newest_slot.state.store(FREE, Ordering::Release);
                    self.inner.state_changed.notify_all();
                    return Err(anyhow::anyhow!("ready frame had no timing metadata"));
                }
            },
            Err(_) => {
                newest_slot.state.store(FREE, Ordering::Release);
                self.inner.state_changed.notify_all();
                return Err(anyhow::anyhow!("shared frame metadata was poisoned"));
            }
        };
        newest_slot.state.store(FREE, Ordering::Release);
        self.inner.state_changed.notify_all();

        Ok(Some(CompletedFrame {
            width: self.inner.width,
            height: self.inner.height,
            stride: self.inner.stride,
            pixels,
            timing,
            fixture_correlation: allocation.map(|allocation| FrameCorrelation {
                allocation,
                sequence: selected_sequence,
            }),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: u8) -> Vec<u8> {
        vec![value; 8]
    }

    fn allocation(id: u64) -> FrameAllocation {
        FrameAllocation {
            id,
            token: 7,
            allocated_ns: 123_456_789 + id,
        }
    }

    #[test]
    fn private_allocation_survives_shared_selection_without_marker_pixels() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("frames");
        let owner = FrameSlots::new_shared(&path, 2, 1, 8)?;
        let child = FrameSlots::open_shared(&path)?;
        for (sequence, id) in [(1, 41), (2, 73)] {
            assert!(child.producer().try_publish_fixture(
                2,
                1,
                8,
                &frame(0),
                FrameTiming::new(987.0, 0.5)?,
                Some(allocation(id)),
            )?);
            let completed = owner
                .broker()
                .take_newest()?
                .context("missing private frame")?;
            let expected = Some(FrameCorrelation {
                allocation: allocation(id),
                sequence,
            });
            assert_eq!(completed.fixture_correlation, expected);
            let (logical, timing) = crate::hardware::LogicalFrame::from_completed(completed);
            assert_eq!(logical.fixture_correlation(), expected);
            assert_eq!(logical.clone().fixture_correlation(), expected);
            assert_eq!(timing.presentation_time, 987.0);
            assert!(crate::diagnostic_observer::decode(&logical).is_err());
            // The complete broker payload is still pixels only.
            let wire = crate::hardware::LogicalFrame::from_wire(
                logical.width(),
                logical.height(),
                logical.stride(),
                logical.pixels().to_vec(),
            );
            assert_eq!(wire.fixture_correlation(), None);
        }
        // Reuse of the same slot must erase all prior fixture metadata.
        assert!(child
            .producer()
            .try_publish(2, 1, 8, &frame(0), FrameTiming::new(987.0, 0.5)?)?);
        assert_eq!(
            owner.broker().take_newest()?.unwrap().fixture_correlation,
            None
        );
        Ok(())
    }

    #[test]
    fn private_shared_allocation_rejects_malformed_tags_and_values() -> Result<()> {
        // Includes absent-but-nonzero payload, unknown tag, zero ID/time and
        // invalid publication sequence. Never return pixels with bad identity.
        for (offset, value) in [
            (32, 2),
            (32, 0),
            (40, 0),
            (56, 0),
            (8, 0),
            (40, u64::MAX),
            (48, u64::MAX),
            (56, u64::MAX),
        ] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("frames");
            let owner = FrameSlots::new_shared(&path, 2, 1, 8)?;
            let child = FrameSlots::open_shared(&path)?;
            assert!(child.producer().try_publish_fixture(
                2,
                1,
                8,
                &frame(8),
                FrameTiming::new(0.0, 0.0)?,
                Some(allocation(9)),
            )?);
            let mut storage = child.shared.as_ref().unwrap().storage.lock().unwrap();
            write_u64(&mut storage, slot_offset(0) + offset, value);
            drop(storage);
            assert!(
                owner.broker().take_newest().is_err(),
                "accepted offset {offset} value {value}"
            );
        }
        Ok(())
    }

    #[test]
    fn private_shared_reclaim_preserves_exact_publication_dispositions() -> Result<()> {
        use crate::diagnostic_observer::{Capture, DiscardReason, EventKind};
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("frames");
        let capture = Capture::new(32)?;
        let owner = FrameSlots::new_shared(&path, 2, 1, 8)?.with_observer(capture.clone());
        let child = FrameSlots::open_shared(&path)?.with_observer(capture.clone());
        for id in [10, 20, 30, 40] {
            assert!(child.producer().try_publish_fixture(
                2,
                1,
                8,
                &frame(0),
                FrameTiming::new(0.0, 0.0)?,
                Some(allocation(id)),
            )?);
        }
        let selected = owner.broker().take_newest()?.unwrap();
        assert_eq!(
            selected.fixture_correlation,
            Some(FrameCorrelation {
                allocation: allocation(40),
                sequence: 4,
            })
        );
        drop(child);
        drop(owner);
        let report = capture.close();
        let published: Vec<_> = report
            .records
            .iter()
            .filter_map(|record| match record.kind {
                EventKind::Published {
                    sequence,
                    allocation,
                    ..
                } => Some((sequence, allocation)),
                _ => None,
            })
            .collect();
        assert_eq!(
            published,
            vec![
                (1, Some(allocation(10))),
                (2, Some(allocation(20))),
                (3, Some(allocation(30))),
                (4, Some(allocation(40)))
            ]
        );
        let mut discarded: Vec<_> = report
            .records
            .iter()
            .filter_map(|record| match record.kind {
                EventKind::Discarded { sequence, reason } => Some((sequence, reason)),
                _ => None,
            })
            .collect();
        discarded.sort_by_key(|(sequence, _)| *sequence);
        assert_eq!(
            discarded,
            vec![
                (1, DiscardReason::ProducerReclaim),
                (2, DiscardReason::ConsumerSuperseded),
                (3, DiscardReason::ConsumerSuperseded)
            ]
        );
        assert_eq!(report.lost, 0);
        Ok(())
    }

    #[test]
    fn private_shared_concurrent_reclaim_keeps_correlation_and_exact_dispositions() -> Result<()> {
        use crate::diagnostic_observer::{Capture, DiscardReason, EventKind};
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("frames");
        let capture = Capture::new(64)?;
        let mut owner = FrameSlots::new_shared(&path, 2, 1, 8)?.with_observer(capture.clone());
        let child = FrameSlots::open_shared(&path)?.with_observer(capture.clone());
        let (snapshot_gate, snapshot_reached, resume_snapshot) = SharedSelectionGate::new();
        let (reading_gate, reading_reached, resume_reading) = SharedSelectionGate::new();
        let shared = Arc::get_mut(owner.shared.as_mut().unwrap()).unwrap();
        shared.snapshot_gate = Some(snapshot_gate);
        shared.reading_gate = Some(reading_gate);

        // Neither the IDs nor timestamps are derived from publication success.
        // Tokens and pixels also differ so a mixed publication cannot pass.
        let allocations =
            [101, 205, 309, 413, 517, 621, 725, 829, 933, 1037, 1141].map(|id| FrameAllocation {
                token: id + 10,
                ..allocation(id)
            });
        let publish = |value: u8| -> Result<()> {
            let producer = child.producer();
            let pixels = frame(value);
            let timing = FrameTiming::new(f64::from(value), f64::from(value) / 100.0)?;
            let published = if value == 10 {
                producer.try_publish(2, 1, 8, &pixels, timing)?
            } else {
                producer.try_publish_fixture(
                    2,
                    1,
                    8,
                    &pixels,
                    timing,
                    Some(allocations[usize::from(value - 1)]),
                )?
            };
            ensure!(published, "publication {value} unexpectedly blocked");
            Ok(())
        };
        for value in 1..=3 {
            publish(value)?;
        }
        let broker = owner.broker();
        let consumer = std::thread::spawn(move || broker.take_newest());
        let interleaving = (|| -> Result<()> {
            snapshot_reached.recv_timeout(Duration::from_secs(5))?;
            // The consumer chose slot 2 holding publication 3, but does not
            // own it yet. Reclaim every slot, including that chosen slot.
            for value in 4..=6 {
                publish(value)?;
            }
            resume_snapshot.send(())?;
            reading_reached.recv_timeout(Duration::from_secs(5))?;
            // The consumer now owns publication 6, before copying its pixels
            // and allocation. Reclaim both other slots, then one again: the
            // pinned publication must stay intact and newer work must survive.
            for value in 7..=9 {
                publish(value)?;
            }
            resume_reading.send(())?;
            Ok(())
        })();
        // Gate waits are bounded, and even an interleaving failure joins the
        // consumer before owner teardown rather than leaving a blocked thread.
        let selected = consumer.join().expect("shared consumer panicked");
        interleaving?;
        let first = selected?.context("reclaimed selection disappeared")?;
        let second = owner.broker().take_newest()?.context("newer frame lost")?;
        // Publication 9 used slot 0, originally populated with fixture metadata.
        // Ordinary publication 10 reuses it and must clear every identity word.
        publish(10)?;
        let ordinary = owner
            .broker()
            .take_newest()?
            .context("ordinary frame lost")?;
        for (completed, value, sequence) in [(first, 6, 6), (second, 9, 9), (ordinary, 10, 10)] {
            assert_eq!(
                (completed.width, completed.height, completed.stride),
                (2, 1, 8)
            );
            assert_eq!(completed.pixels, frame(value));
            assert_eq!(
                completed.timing,
                FrameTiming::new(f64::from(value), f64::from(value) / 100.0)?
            );
            assert_eq!(
                completed.fixture_correlation,
                (value != 10).then_some(FrameCorrelation {
                    allocation: allocations[usize::from(value - 1)],
                    sequence,
                })
            );
        }
        assert!(owner.broker().take_newest()?.is_none());
        // Include a READY publication closed only by the owner, after the
        // producer is gone, in the same exact-once disposition ledger.
        publish(11)?;
        drop(child);
        drop(owner);
        let report = capture.close();
        let published: Vec<_> = report
            .records
            .iter()
            .filter_map(|record| match record.kind {
                EventKind::Published {
                    sequence,
                    allocation,
                    ..
                } => Some((sequence, allocation)),
                _ => None,
            })
            .collect();
        let expected: Vec<_> = allocations
            .iter()
            .enumerate()
            .map(|(index, allocation)| ((index + 1) as u64, (index != 9).then_some(*allocation)))
            .collect();
        assert_eq!(published, expected);
        let selected: Vec<_> = report
            .records
            .iter()
            .filter_map(|record| match record.kind {
                EventKind::Selected { sequence } => Some(sequence),
                _ => None,
            })
            .collect();
        assert_eq!(selected, vec![6, 9, 10]);
        let mut discarded: Vec<_> = report
            .records
            .iter()
            .filter_map(|record| match record.kind {
                EventKind::Discarded { sequence, reason } => Some((sequence, reason)),
                _ => None,
            })
            .collect();
        discarded.sort_by_key(|(sequence, _)| *sequence);
        assert_eq!(
            discarded,
            vec![
                (1, DiscardReason::ProducerReclaim),
                (2, DiscardReason::ProducerReclaim),
                (3, DiscardReason::ProducerReclaim),
                (4, DiscardReason::ProducerReclaim),
                (5, DiscardReason::ProducerReclaim),
                (7, DiscardReason::ProducerReclaim),
                (8, DiscardReason::ConsumerSuperseded),
                (11, DiscardReason::MappingClosed),
            ]
        );
        let mut disposed = selected;
        disposed.extend(discarded.iter().map(|(sequence, _)| *sequence));
        disposed.sort_unstable();
        assert_eq!(disposed, (1..=11).collect::<Vec<_>>());
        assert_eq!(report.lost, 0);
        Ok(())
    }

    #[test]
    fn private_shared_blocked_publication_does_not_allocate_a_sequence() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("frames");
        let owner = FrameSlots::new_shared(&path, 2, 1, 8)?;
        let child = FrameSlots::open_shared(&path)?;
        let set_states = |state| {
            let storage = owner.shared.as_ref().unwrap().storage.lock().unwrap();
            for index in 0..SLOT_COUNT {
                write_slot_state(&storage, index, state);
            }
        };
        set_states(READING);
        assert!(!child.producer().try_publish_fixture(
            2,
            1,
            8,
            &frame(0),
            FrameTiming::new(0.0, 0.0)?,
            Some(allocation(45)),
        )?);
        set_states(FREE);
        assert!(child.producer().try_publish_fixture(
            2,
            1,
            8,
            &frame(0),
            FrameTiming::new(0.0, 0.0)?,
            Some(allocation(45)),
        )?);
        assert_eq!(
            owner.broker().take_newest()?.unwrap().fixture_correlation,
            Some(FrameCorrelation {
                allocation: allocation(45),
                sequence: 1
            })
        );
        Ok(())
    }

    #[test]
    fn private_shared_layout_and_sequence_exhaustion_fail_closed() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("frames");
        let owner = FrameSlots::new_shared(&path, 2, 1, 8)?;
        {
            let mut storage = owner.shared.as_ref().unwrap().storage.lock().unwrap();
            storage[..8].copy_from_slice(b"SLVRFRM1");
        }
        assert!(
            FrameSlots::open_shared(&path).is_err(),
            "old metadata layout accepted"
        );
        {
            let mut storage = owner.shared.as_ref().unwrap().storage.lock().unwrap();
            storage[..8].copy_from_slice(SHARED_MAGIC);
            write_u64(&mut storage, 32, u64::MAX);
        }
        let child = FrameSlots::open_shared(&path)?;
        assert!(child
            .producer()
            .try_publish_fixture(
                2,
                1,
                8,
                &frame(0),
                FrameTiming::new(0.0, 0.0)?,
                Some(allocation(1))
            )
            .is_err());
        assert!(
            owner.broker().take_newest()?.is_none(),
            "exhaustion published wrapped sequence"
        );
        Ok(())
    }

    #[test]
    fn private_publication_rejects_invalid_allocation_before_slot_claim() -> Result<()> {
        let slots = FrameSlots::new(2, 1, 8)?;
        let producer = slots.producer();
        for invalid in [
            FrameAllocation {
                id: 0,
                ..allocation(1)
            },
            FrameAllocation {
                allocated_ns: 0,
                ..allocation(1)
            },
            FrameAllocation {
                id: u64::MAX,
                ..allocation(1)
            },
            FrameAllocation {
                token: u64::MAX,
                ..allocation(1)
            },
            FrameAllocation {
                allocated_ns: u64::MAX,
                ..allocation(1)
            },
        ] {
            assert!(producer
                .try_publish_fixture(
                    2,
                    1,
                    8,
                    &frame(0),
                    FrameTiming::new(0.0, 0.0)?,
                    Some(invalid)
                )
                .is_err());
        }
        assert!(slots.broker().take_newest()?.is_none());
        assert_eq!(slots.inner.next_sequence.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[test]
    fn shared_mappings_exchange_complete_frames_between_broker_instances() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("frames.bin");
        let slots = FrameSlots::new_shared(&path, 2, 1, 8)?;
        let producer = slots.producer();
        let reader = FrameSlots::open_shared(&path)?.broker();
        assert!(producer.try_publish(2, 1, 8, &frame(7), FrameTiming::new(3.0, 0.5)?)?);
        let completed = reader
            .take_newest()?
            .context("shared frame was not visible")?;
        assert_eq!(completed.pixels, frame(7));
        assert_eq!(completed.timing.presentation_time, 3.0);
        Ok(())
    }

    #[test]
    fn broker_keeps_frames_published_after_selection_snapshot() -> Result<()> {
        let gate = SelectionGate::new();
        let slots = FrameSlots::new_with_selection_gate(2, 1, 8, gate.clone())?;
        let producer = slots.producer();
        for value in 1..=3 {
            assert!(producer.publish(
                2,
                1,
                8,
                &frame(value),
                FrameTiming::new(f64::from(value), if value == 1 { 0.0 } else { 1.0 })?,
            )?);
        }
        let broker = slots.broker();
        let broker_thread = std::thread::spawn(move || broker.take_newest());

        gate.selected.wait();
        assert!(producer.publish(2, 1, 8, &frame(4), FrameTiming::new(4.0, 1.0)?,)?);
        gate.resume.wait();

        let first = broker_thread
            .join()
            .expect("broker thread panicked")?
            .expect("selected frame was lost");
        assert_eq!(first.pixels, frame(3));
        let second = slots
            .broker()
            .take_newest()?
            .expect("newer frame was dropped");
        assert_eq!(second.pixels, frame(4));
        Ok(())
    }

    #[test]
    fn producer_can_retry_after_all_slots_held_by_broker_reclamation() -> Result<()> {
        let selection_gate = SelectionGate::new();
        let slots = FrameSlots::new_with_selection_gate(2, 1, 8, selection_gate.clone())?;
        let producer = slots.producer();
        assert!(producer.try_publish(2, 1, 8, &frame(1), FrameTiming::new(1.0, 0.0)?)?);
        assert!(producer.try_publish(2, 1, 8, &frame(2), FrameTiming::new(2.0, 1.0)?)?);
        let _held_writer = producer.begin_write().expect("third slot was unavailable");
        let broker = slots.broker();
        let broker_thread = std::thread::spawn(move || broker.take_newest());

        selection_gate.selected.wait();
        // Exercise the actual nonblocking producer API. A failed attempt while
        // all slots are held must be retryable after broker reclamation, without
        // depending on the test-only publish helper's wall-clock deadline.
        let blocked = producer.try_publish(2, 1, 8, &frame(3), FrameTiming::new(3.0, 1.0)?);
        selection_gate.resume.wait();
        let selected = broker_thread
            .join()
            .expect("broker thread panicked")?
            .expect("broker did not select a frame");
        assert!(!blocked?);
        assert_eq!(selected.pixels, frame(2));
        assert!(producer.try_publish(2, 1, 8, &frame(3), FrameTiming::new(3.0, 1.0)?)?);
        let retried = slots
            .broker()
            .take_newest()?
            .expect("retried frame was lost");
        assert_eq!(retried.pixels, frame(3));
        Ok(())
    }

    #[test]
    fn private_observed_opener_close_does_not_dispose_the_owners_ready_frame() -> Result<()> {
        use crate::diagnostic_observer::{Capture, DiscardReason, EventKind};
        for consume in [false, true] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("frames");
            let owner_capture = Capture::new(16)?;
            let worker_capture = Capture::new(16)?;
            let owner =
                FrameSlots::new_shared(&path, 2, 1, 8)?.with_observer(owner_capture.clone());
            let opener = FrameSlots::open_shared(&path)?.with_observer(worker_capture.clone());
            assert!(opener.producer().try_publish(
                2,
                1,
                8,
                &frame(7),
                FrameTiming::new(0.0, 0.0)?
            )?);
            drop(opener);
            let worker_report = worker_capture.close();
            assert!(
                !worker_report.records.iter().any(|record| matches!(
                    record.kind,
                    EventKind::Discarded {
                        reason: DiscardReason::MappingClosed,
                        ..
                    }
                )),
                "worker unmapping is not global publication disposal"
            );
            assert!(path.exists());
            if consume {
                assert_eq!(
                    owner
                        .broker()
                        .take_newest()?
                        .expect("ready frame lost")
                        .pixels,
                    frame(7)
                );
            }
            // The owner tears down only after the producer is gone.
            drop(owner);
            let owner_report = owner_capture.close();
            let closed: Vec<_> = owner_report
                .records
                .iter()
                .filter_map(|record| match record.kind {
                    EventKind::Discarded {
                        sequence,
                        reason: DiscardReason::MappingClosed,
                    } => Some(sequence),
                    _ => None,
                })
                .collect();
            assert_eq!(closed, if consume { vec![] } else { vec![1] });
            assert!(!path.exists());
        }
        Ok(())
    }

    #[test]
    fn broker_presents_the_newest_complete_frame_and_drops_older_ready_frames() -> Result<()> {
        let slots = FrameSlots::new(2, 1, 8)?;
        let producer = slots.producer();
        let broker = slots.broker();
        for value in 1..=4 {
            assert!(producer.publish(
                2,
                1,
                8,
                &frame(value),
                FrameTiming::new(f64::from(value), if value == 1 { 0.0 } else { 1.0 })?,
            )?);
        }

        let presented = broker.take_newest()?.expect("complete frame was lost");
        assert_eq!(presented.pixels, frame(4));
        assert_eq!(presented.timing.presentation_time, 4.0);
        assert!(broker.take_newest()?.is_none());
        Ok(())
    }

    #[test]
    fn dropped_writer_cannot_clear_a_successor_metadata() -> Result<()> {
        let gate = DropGate::new();
        let slots = FrameSlots::new_with_drop_gate(2, 1, 8, gate.clone())?;
        let producer = slots.producer();
        let broker = slots.broker();
        let writer = producer.begin_write().expect("first slot was unavailable");
        let dropper = std::thread::spawn(move || drop(writer));

        gate.released.wait();
        assert!(producer.publish(2, 1, 8, &frame(9), FrameTiming::new(9.0, 0.0)?,)?);
        gate.resume.wait();
        dropper.join().expect("writer cleanup thread panicked");

        let completed = broker.take_newest()?.expect("successor frame was lost");
        assert_eq!(completed.timing.presentation_time, 9.0);
        Ok(())
    }

    #[test]
    fn incomplete_slot_is_invisible_while_another_slot_can_present() -> Result<()> {
        let slots = FrameSlots::new(2, 1, 8)?;
        let producer = slots.producer();
        let broker = slots.broker();
        let mut partial = producer.begin_write().expect("first slot was unavailable");
        partial.write_bytes(&[7, 7, 7, 7])?;

        assert!(broker.take_newest()?.is_none());
        assert!(producer.publish(2, 1, 8, &frame(9), FrameTiming::new(2.0, 0.0)?,)?);
        assert_eq!(
            broker.take_newest()?.expect("complete slot stalled").pixels,
            frame(9)
        );
        drop(partial);
        Ok(())
    }

    #[test]
    fn abandoned_producer_mid_write_leaves_incomplete_slot_ignored() -> Result<()> {
        let slots = FrameSlots::new(2, 1, 8)?;
        let producer = slots.producer();
        let broker = slots.broker();
        std::thread::spawn(move || {
            let mut partial = producer.begin_write().expect("first slot was unavailable");
            partial
                .write_bytes(&[3, 3, 3, 3])
                .expect("partial write failed");
            // Abandonment models a producer disappearing, not process detection.
            std::mem::forget(partial);
        })
        .join()
        .expect("producer thread panicked");

        assert!(broker.take_newest()?.is_none());
        let producer = slots.producer();
        assert!(producer.publish(2, 1, 8, &frame(5), FrameTiming::new(5.0, 0.0)?,)?);
        let completed = broker.take_newest()?.expect("broker stalled");
        assert_eq!(completed.pixels, frame(5));
        assert_eq!(completed.timing.presentation_time, 5.0);
        Ok(())
    }
}
