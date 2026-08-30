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

#[cfg(test)]
struct AcquisitionGate {
    attempted: std::sync::Barrier,
    resume: std::sync::Barrier,
    active: AtomicU8,
}

#[cfg(test)]
impl AcquisitionGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            attempted: std::sync::Barrier::new(2),
            resume: std::sync::Barrier::new(2),
            active: AtomicU8::new(1),
        })
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompletedFrame {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) stride: usize,
    pub(crate) pixels: Vec<u8>,
    pub(crate) timing: FrameTiming,
}

struct Slot {
    state: AtomicU8,
    sequence: AtomicU64,
    timing: Mutex<Option<FrameTiming>>,
}

struct SharedSlots {
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
    #[cfg(test)]
    acquisition_gate: Option<Arc<AcquisitionGate>>,
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

const SHARED_MAGIC: &[u8; 8] = b"SLVRFRM1";
const SHARED_HEADER_BYTES: usize = 128;
const SHARED_SLOT_META_BYTES: usize = 32;
const SHARED_PIXEL_OFFSET: usize = SHARED_HEADER_BYTES + SLOT_COUNT * SHARED_SLOT_META_BYTES;

struct SharedFrameMap {
    storage: Mutex<MmapMut>,
    path: Option<PathBuf>,
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
            path: Some(path.to_path_buf()),
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
            path: None,
            slot_bytes,
            width,
            height,
            stride,
        })
    }

    fn try_publish(&self, pixels: &[u8], timing: FrameTiming) -> Result<bool> {
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
                    .fetch_add(1, Ordering::Relaxed)
                    .wrapping_add(1)
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
                    return Ok(Some(index));
                }
            }
        }
        Ok(None)
    }

    fn take_newest(&self) -> Result<Option<CompletedFrame>> {
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
        {
            let storage = self
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            if !compare_slot_state(&storage, index, READY, READING) {
                return Ok(None);
            }
        }
        let (pixels, timing) = {
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
            (pixels, timing)
        };
        {
            let storage = self
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            write_slot_state(&storage, index, FREE);
            for older in 0..SLOT_COUNT {
                if older != index
                    && read_slot_state(&storage, older) == READY
                    && read_u64(&storage, slot_offset(older) + 8)?
                        < read_u64(&storage, slot_offset(index) + 8)?
                {
                    write_slot_state(&storage, older, FREE);
                }
            }
        }
        Ok(Some(CompletedFrame {
            width: self.width,
            height: self.height,
            stride: self.stride,
            pixels,
            timing,
        }))
    }
}

impl Drop for SharedFrameMap {
    fn drop(&mut self) {
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
    Ok(u64::from_ne_bytes(storage[offset..offset + 8].try_into()?))
}

fn write_u64(storage: &mut [u8], offset: usize, value: u64) {
    storage[offset..offset + 8].copy_from_slice(&value.to_ne_bytes());
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
        #[cfg(test)]
        acquisition_gate: None,
    })
}

impl FrameSlots {
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

    #[cfg(test)]
    fn new_with_reclamation_gates(
        width: usize,
        height: usize,
        stride: usize,
        selection_gate: Arc<SelectionGate>,
        acquisition_gate: Arc<AcquisitionGate>,
    ) -> Result<Self> {
        let mut slots = Self::new_with_selection_gate(width, height, stride, selection_gate)?;
        Arc::get_mut(&mut slots.inner)
            .expect("new slots have one owner")
            .acquisition_gate = Some(acquisition_gate);
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
        self.validate_frame(width, height, stride, pixels)?;
        if let Some(shared) = &self.shared {
            return shared.try_publish(pixels, timing);
        }
        self.publish_once(pixels, timing)
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
            if let Some(acquisition_gate) = &self.inner.acquisition_gate {
                if acquisition_gate.active.swap(0, Ordering::AcqRel) != 0 {
                    acquisition_gate.attempted.wait();
                    acquisition_gate.resume.wait();
                }
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
                            .map(|_| index)
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

    fn publish(&mut self, timing: FrameTiming) -> Result<()> {
        let sequence = self
            .inner
            .next_sequence
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let slot = &self.inner.slots[self.index];
        *slot
            .timing
            .lock()
            .map_err(|_| anyhow::anyhow!("shared frame metadata was poisoned"))? = Some(timing);
        slot.sequence.store(sequence, Ordering::Relaxed);
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
        let timing = match newest_slot.timing.lock() {
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
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: u8) -> Vec<u8> {
        vec![value; 8]
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
    fn producer_waits_through_all_slots_held_by_broker_reclamation() -> Result<()> {
        let selection_gate = SelectionGate::new();
        let acquisition_gate = AcquisitionGate::new();
        let slots = FrameSlots::new_with_reclamation_gates(
            2,
            1,
            8,
            selection_gate.clone(),
            acquisition_gate.clone(),
        )?;
        let producer = slots.producer();
        assert!(producer.publish(2, 1, 8, &frame(1), FrameTiming::new(1.0, 0.0)?,)?);
        assert!(producer.publish(2, 1, 8, &frame(2), FrameTiming::new(2.0, 1.0)?,)?);
        let _held_writer = producer.begin_write().expect("third slot was unavailable");
        let broker = slots.broker();
        let broker_thread = std::thread::spawn(move || broker.take_newest());

        selection_gate.selected.wait();
        let retry_producer = producer.clone();
        let publish_thread = std::thread::spawn(move || {
            retry_producer.publish(2, 1, 8, &frame(3), FrameTiming::new(3.0, 1.0).unwrap())
        });
        acquisition_gate.attempted.wait();
        acquisition_gate.resume.wait();
        selection_gate.resume.wait();

        assert!(publish_thread
            .join()
            .expect("producer thread panicked")
            .expect("producer publish failed"));
        broker_thread
            .join()
            .expect("broker thread panicked")?
            .expect("broker did not select a frame");
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
