//! Private adapter-boundary instrumentation, installed explicitly before the
//! broker's fallback supervisor is constructed. No service arming or authority.
#![allow(dead_code)] // Trusted production provisioning remains off.

use std::time::Duration;

use anyhow::Result;

use crate::diagnostic_observer::Capture;
use crate::diagnostic_timing::{SpanKind, TimingCapture};
use crate::hardware::{
    HardwareCapability, HardwareEvent, InputState, LogicalFrame, SyntheticKeyEvent,
    TouchBarHardware,
};

pub(crate) struct ObservedHardware<H> {
    hardware: H,
    observer: Option<Capture>,
    timing: Option<TimingCapture>,
}

impl<H: TouchBarHardware> ObservedHardware<H> {
    pub(crate) fn new(
        hardware: H,
        observer: Option<Capture>,
        timing: Option<TimingCapture>,
    ) -> Self {
        Self {
            hardware,
            observer,
            timing,
        }
    }
}

impl<H: TouchBarHardware> TouchBarHardware for ObservedHardware<H> {
    fn claim(&mut self) -> Result<()> {
        self.hardware.claim()
    }
    fn reacquire(&mut self) -> Result<()> {
        self.hardware.reacquire()
    }
    fn is_available(&self) -> bool {
        self.hardware.is_available()
    }
    fn unavailable_capability(&self) -> Option<HardwareCapability> {
        self.hardware.unavailable_capability()
    }
    fn session_revoked(&self) -> bool {
        self.hardware.session_revoked()
    }
    fn connection_lost(&self) -> bool {
        self.hardware.connection_lost()
    }
    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        match &self.observer {
            Some(observer) => observer.poll(&mut self.hardware, timeout),
            None => self.hardware.poll(timeout),
        }
    }
    fn input_state(&self) -> InputState {
        self.hardware.input_state()
    }
    fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
        // The complete decorated adapter operation includes detailed decoding
        // and retention when enabled. It is not a bare ioctl duration; raw
        // PresentEntered/Returned remain the independent adapter-call seams.
        let span = self
            .timing
            .as_ref()
            .map(|timing| timing.begin(SpanKind::Present));
        let result = match &self.observer {
            Some(observer) => observer.present(&mut self.hardware, frame),
            None => self.hardware.present(frame),
        };
        if let Some(span) = span {
            span.finish(true, result.is_ok());
        }
        result
    }
    fn confirm_owner(&mut self) -> Result<()> {
        self.hardware.confirm_owner()
    }
    fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
        self.hardware.emit_key_events(events)
    }
    fn get_backlight(&mut self) -> Result<f64> {
        self.hardware.get_backlight()
    }
    fn set_backlight(&mut self, level: f64) -> Result<()> {
        self.hardware.set_backlight(level)
    }
    fn release(&mut self) -> Result<()> {
        self.hardware.release()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic_observer::EventKind;
    use crate::diagnostic_timing::SpanStatus;
    use crate::hardware::FakeTouchBar;

    #[test]
    fn private_broker_capture_preserves_adapter_failures_and_lifecycle_with_capture_off_or_on(
    ) -> Result<()> {
        let (frame, _) = LogicalFrame::from_completed(crate::frame_slots::CompletedFrame {
            width: 2,
            height: 1,
            stride: 8,
            pixels: vec![255; 8],
            fixture_correlation: None,
            timing: crate::frame_slots::FrameTiming::new(0.0, 0.0)?,
        });
        for enabled in [false, true] {
            let raw = Capture::new(32)?;
            let timing = TimingCapture::new(8)?;
            let mut fake = FakeTouchBar::new();
            fake.inject(HardwareEvent::Capability {
                capability: HardwareCapability::Display,
                present: false,
            });
            let mut hardware = ObservedHardware::new(
                fake,
                enabled.then(|| raw.clone()),
                enabled.then(|| timing.clone()),
            );
            assert!(hardware
                .present(&frame)
                .unwrap_err()
                .to_string()
                .contains("not claimed"));
            assert!(hardware
                .poll(Duration::ZERO)
                .unwrap_err()
                .to_string()
                .contains("not claimed"));
            hardware.claim()?;
            assert!(hardware.is_available());
            assert_eq!(hardware.poll(Duration::ZERO)?.len(), 1);
            assert!(!hardware.is_available());
            assert_eq!(
                hardware.unavailable_capability(),
                Some(HardwareCapability::Display)
            );
            hardware.release()?;
            hardware.reacquire()?;
            assert!(hardware.is_available());
            hardware.set_backlight(0.25)?;
            assert_eq!(hardware.get_backlight()?, 0.25);
            hardware.present(&frame)?;
            hardware.release()?;
            let report = raw.close();
            let minimal = timing.close();
            if enabled {
                assert_eq!(report.failed_operations, 2);
                assert_eq!(report.decode_failures, 2);
                assert_eq!(report.lost, 0);
                assert_eq!(minimal.failed_spans, 1);
                assert_eq!(minimal.records.len(), 2);
                assert_eq!(minimal.records[0].status, SpanStatus::Failed);
                assert_eq!(minimal.records[1].status, SpanStatus::Succeeded);
                for sample in &minimal.records {
                    let entered = report.records.iter().find(|record| {
                        matches!(record.kind, EventKind::PresentEntered { .. })
                            && record.at_ns >= sample.start_ns
                            && record.at_ns <= sample.end_ns
                    });
                    let returned = report.records.iter().find(|record| {
                        matches!(record.kind, EventKind::PresentReturned { .. })
                            && record.at_ns >= sample.start_ns
                            && record.at_ns <= sample.end_ns
                    });
                    assert!(
                        entered.is_some() && returned.is_some(),
                        "minimal span did not enclose detailed adapter seams"
                    );
                }
            } else {
                assert!(report.records.is_empty());
                assert!(minimal.records.is_empty());
            }
        }
        Ok(())
    }
}
