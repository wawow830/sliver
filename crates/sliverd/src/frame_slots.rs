use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{ensure, Result};
use memmap2::MmapMut;

const SLOT_COUNT: usize = 3;
const FREE: u8 = 0;
const WRITING: u8 = 1;
const READY: u8 = 2;
const READING: u8 = 3;

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
}

/// The fixed-size producer/broker handoff for decoded frames.
///
/// Pixel bytes live in one anonymous mmap split into three fixed slots. State
/// transitions publish a slot only after the complete row range has been
/// copied. The broker never reads a slot in `WRITING`, so a producer that
/// disappears halfway through a copy cannot expose those bytes.
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
            }),
        })
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
            let _ =
                slot.state
                    .compare_exchange(WRITING, FREE, Ordering::Release, Ordering::Relaxed);
            if let Ok(mut timing) = slot.timing.lock() {
                *timing = None;
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

        for (older_index, slot) in self.inner.slots.iter().enumerate() {
            if older_index != index {
                let _ =
                    slot.state
                        .compare_exchange(READY, FREE, Ordering::AcqRel, Ordering::Relaxed);
            }
        }

        let offset = index * self.inner.slot_bytes;
        let pixels = {
            let storage = self
                .inner
                .storage
                .lock()
                .map_err(|_| anyhow::anyhow!("shared frame storage was poisoned"))?;
            storage[offset..offset + self.inner.slot_bytes].to_vec()
        };
        let timing = newest_slot
            .timing
            .lock()
            .map_err(|_| anyhow::anyhow!("shared frame metadata was poisoned"))?
            .take()
            .ok_or_else(|| anyhow::anyhow!("ready frame had no timing metadata"))?;
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
    use std::time::Duration;

    use crate::hardware::{LogicalFrame, TouchBarHardware};

    use super::*;

    fn frame(value: u8) -> Vec<u8> {
        vec![value; 8]
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
    fn native_2008_by_60_decoded_frames_keep_up_without_latency_growth() -> Result<()> {
        let width = 2008;
        let height = 60;
        let stride = width * 4;
        let slots = FrameSlots::new(width, height, stride)?;
        let producer = slots.producer();
        let broker = slots.broker();
        let mut hardware = crate::hardware::FakeTouchBar::new();
        hardware.claim()?;
        let mut pixels = vec![0u8; stride * height];
        let interval = Duration::from_nanos(1_000_000_000 / 60);
        let start = std::time::Instant::now();
        let mut missed_deadlines = 0;

        for index in 0..60u64 {
            let deadline = start + interval.mul_f64(index as f64);
            if std::time::Instant::now() > deadline {
                missed_deadlines += 1;
            } else if let Some(wait) = deadline.checked_duration_since(std::time::Instant::now()) {
                std::thread::sleep(wait);
            }
            pixels.fill(index as u8);
            pixels
                .chunks_exact_mut(4)
                .for_each(|pixel| pixel[3] = u8::MAX);
            assert!(producer.publish(
                width,
                height,
                stride,
                &pixels,
                FrameTiming::new(
                    index as f64 / 60.0,
                    if index == 0 { 0.0 } else { 1.0 / 60.0 }
                )?,
            )?);
            let completed = broker
                .take_newest()?
                .expect("published frame was not ready");
            let (frame, timing) = LogicalFrame::from_completed(completed);
            assert_eq!(frame.width(), width);
            assert_eq!(frame.height(), height);
            assert_eq!(timing.presentation_time, index as f64 / 60.0);
            hardware.present(&frame)?;
            assert!(broker.take_newest()?.is_none());
        }

        let elapsed = start.elapsed();
        let fps = 60.0 / elapsed.as_secs_f64();
        eprintln!(
            "native decoded frame target: {width}x{height} at {fps:.1} FPS, {missed_deadlines} missed deadlines"
        );
        assert_eq!(hardware.presented_frames().len(), 60);
        assert!(elapsed < Duration::from_secs(5));
        hardware.release()?;
        Ok(())
    }

    #[test]
    fn a_producer_that_disappears_during_write_does_not_stall_the_broker() -> Result<()> {
        let slots = FrameSlots::new(2, 1, 8)?;
        let producer = slots.producer();
        let broker = slots.broker();
        std::thread::spawn(move || {
            let mut partial = producer.begin_write().expect("first slot was unavailable");
            partial
                .write_bytes(&[3, 3, 3, 3])
                .expect("partial write failed");
            std::mem::forget(partial);
        })
        .join()
        .expect("producer thread panicked");

        assert!(broker.take_newest()?.is_none());
        let producer = slots.producer();
        assert!(producer.publish(2, 1, 8, &frame(5), FrameTiming::new(5.0, 0.0)?,)?);
        assert_eq!(
            broker.take_newest()?.expect("broker stalled").pixels,
            frame(5)
        );
        Ok(())
    }
}
