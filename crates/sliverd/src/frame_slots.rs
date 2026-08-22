use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{ensure, Result};
use memmap2::MmapMut;

const SLOT_COUNT: usize = 3;
const FREE: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;
const READING: u8 = 3;
const RECLAIMING: u8 = 4;

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
}

#[derive(Clone)]
pub(crate) struct FrameProducer {
    inner: Arc<SharedSlots>,
}

#[derive(Clone)]
pub(crate) struct FrameBroker {
    inner: Arc<SharedSlots>,
}

pub(crate) struct FrameWriter {
    inner: Arc<SharedSlots>,
    index: usize,
    published: bool,
}

impl FrameSlots {
    pub(crate) fn new(width: usize, height: usize, stride: usize) -> Result<Self> {
        ensure!(width > 0, "frame width must be positive");
        ensure!(height > 0, "frame height must be positive");
        ensure!(
            stride >= width.saturating_mul(4),
            "frame stride is too small"
        );
        let slot_bytes = stride
            .checked_mul(height)
            .ok_or_else(|| anyhow::anyhow!("frame slot size overflows usize"))?;
        let map_bytes = slot_bytes
            .checked_mul(SLOT_COUNT)
            .ok_or_else(|| anyhow::anyhow!("frame slot mapping size overflows usize"))?;
        let storage = MmapMut::map_anon(map_bytes)?;
        let slots = std::array::from_fn(|_| Slot {
            state: AtomicU8::new(FREE),
            sequence: AtomicU64::new(0),
            timing: Mutex::new(None),
        });
        Ok(Self {
            inner: Arc::new(SharedSlots {
                storage: Mutex::new(storage),
                slot_bytes,
                width,
                height,
                stride,
                next_sequence: AtomicU64::new(0),
                slots,
                #[cfg(test)]
                drop_gate: None,
                #[cfg(test)]
                selection_gate: None,
            }),
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
        }
    }

    pub(crate) fn broker(&self) -> FrameBroker {
        FrameBroker {
            inner: self.inner.clone(),
        }
    }
}

impl FrameProducer {
    pub(crate) fn publish(
        &self,
        width: usize,
        height: usize,
        stride: usize,
        pixels: &[u8],
        timing: FrameTiming,
    ) -> Result<bool> {
        ensure!(
            (width, height, stride) == (self.inner.width, self.inner.height, self.inner.stride),
            "frame dimensions do not match the shared slots"
        );
        ensure!(
            pixels.len() == self.inner.slot_bytes,
            "frame pixels do not fill one shared slot"
        );

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
        }

        let offset = index * self.inner.slot_bytes;
        let pixels = match self.inner.storage.lock() {
            Ok(storage) => storage[offset..offset + self.inner.slot_bytes].to_vec(),
            Err(_) => {
                newest_slot.state.store(FREE, Ordering::Release);
                return Err(anyhow::anyhow!("shared frame storage was poisoned"));
            }
        };
        let timing = match newest_slot.timing.lock() {
            Ok(mut timing) => match timing.take() {
                Some(timing) => timing,
                None => {
                    newest_slot.state.store(FREE, Ordering::Release);
                    return Err(anyhow::anyhow!("ready frame had no timing metadata"));
                }
            },
            Err(_) => {
                newest_slot.state.store(FREE, Ordering::Release);
                return Err(anyhow::anyhow!("shared frame metadata was poisoned"));
            }
        };
        newest_slot.state.store(FREE, Ordering::Release);

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
