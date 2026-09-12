//! Private reviewed P1 workload capability. Not a native collector or release
//! verdict. Only explicit Rust provisioning can inject it; ordinary Lua has no
//! capability. Synthetic-clock runs are always identified as synthetic.
#![allow(dead_code)] // Process/coordinator provisioning is deliberately not installed.

use crate::hardware::{TouchEvent, TouchPhase};
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{ensure, Context, Result};
use mlua::{AnyUserData, Function, Lua, Table, UserData, UserDataFields, UserDataMethods};

pub(crate) const SOURCE: &[u8] = include_bytes!("../../../scripts/native-performance-p1.lua");
const MARKER: &[u8] = include_bytes!("../../../scripts/native-performance-marker.lua");
const MAX_OBSERVATIONS: usize = 16_384;
const MAX_OUTSTANDING: usize = 64;
const WARMUP_NS: u64 = 5_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    A,
    B,
    C,
}

#[derive(Clone)]
pub(crate) struct Plan {
    run: String,
    generation: String,
    pub(crate) start_ns: u64,
    pub(crate) stop_ns: u64,
    pub(crate) end_ns: u64,
    pub(crate) rate: u32,
    pub(crate) mode: Mode,
    causal: bool,
}
impl Plan {
    pub(crate) fn new(
        run: &str,
        generation: &str,
        start_ns: u64,
        rate: u32,
        mode: Mode,
    ) -> Result<Self> {
        for identity in [run, generation] {
            ensure!(
                !identity.is_empty()
                    && identity.len() <= 128
                    && identity
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)),
                "invalid fixture identity"
            );
        }
        ensure!(matches!(rate, 30 | 60), "P1 requires 30 or 60 Hz");
        let stop_ns = start_ns
            .checked_add(60_000_000_000)
            .context("fixture stop overflow")?;
        let end_ns = stop_ns
            .checked_add(2_000_000_000)
            .context("fixture end overflow")?;
        ensure!(
            end_ns <= i64::MAX as u64,
            "fixture time outside Lua integer range"
        );
        Ok(Self {
            run: run.into(),
            generation: generation.into(),
            start_ns,
            stop_ns,
            end_ns,
            rate,
            mode,
            causal: false,
        })
    }
    pub(crate) fn causal_response(mut self) -> Result<Self> {
        ensure!(self.rate == 30, "P1 causal case uses 30 Hz");
        self.causal = true;
        Ok(self)
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct SyntheticClock {
    now: Arc<AtomicU64>,
    reads: Arc<Mutex<VecDeque<u64>>>,
}
#[cfg(test)]
impl SyntheticClock {
    pub(crate) fn new(now: u64) -> Self {
        Self {
            now: Arc::new(AtomicU64::new(now)),
            reads: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
    pub(crate) fn set(&self, now: u64) {
        self.reads.lock().unwrap().clear();
        self.now.store(now, Ordering::SeqCst);
    }
    pub(crate) fn script(&self, reads: &[u64]) {
        assert!(reads.len() <= 64, "bounded synthetic clock script");
        *self.reads.lock().unwrap() = reads.iter().copied().collect();
    }
    fn read(&self) -> u64 {
        if let Some(now) = self.reads.lock().unwrap().pop_front() {
            self.now.store(now, Ordering::SeqCst);
        }
        self.now.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
pub(crate) enum Clock {
    HostMonotonic,
    #[cfg(test)]
    Synthetic(SyntheticClock),
}
impl Clock {
    fn now(&self) -> Result<u64> {
        match self {
            Self::HostMonotonic => crate::diagnostic_observer::clock_ns(false),
            #[cfg(test)]
            Self::Synthetic(clock) => Ok(clock.read()),
        }
    }
    fn synthetic(&self) -> bool {
        !matches!(self, Self::HostMonotonic)
    }
    fn sample(&self) -> Result<Stamp> {
        let host_ns = crate::diagnostic_observer::clock_ns(false)?;
        // Native decisions and allocation observations share exactly one read.
        // Synthetic decision time is never put in a raw host timestamp field.
        let decision_ns = if self.synthetic() {
            self.now()?
        } else {
            host_ns
        };
        Ok(Stamp {
            host_ns,
            decision_ns,
        })
    }
}

#[derive(Clone, Copy)]
struct Stamp {
    host_ns: u64,
    decision_ns: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Warmup,
    Quiescing,
    Armed,
    Measured,
    Drain,
    Ended,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Quiescing => "quiescing",
            Self::Armed => "armed",
            Self::Measured => "measured",
            Self::Drain => "drain",
            Self::Ended => "ended",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    Presented,
    Discarded,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationKind {
    Phase(Phase),
    Allocated {
        frame_id: u64,
        token: u64,
    },
    Suppressed,
    Resolved {
        frame_id: u64,
        resolution: Resolution,
        resolved_ns: u64,
    },
    WarmupClosed,
    PeriodicRetired {
        timer_id: u64,
    },
    ReceiptSupplied {
        receipt_sequence: u64,
        received_ns: u64,
        contact: u32,
        phase: TouchPhase,
    },
    Delivered {
        receipt_sequence: u64,
    },
    TokenMutated {
        receipt_sequence: u64,
        previous: u64,
        token: u64,
    },
    ResponseConfirmed {
        receipt_sequence: u64,
        frame_id: u64,
        token: u64,
        responded_ns: u64,
    },
    Failed,
    Closed,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Observation {
    pub(crate) host_ns: u64,
    /// Authoritative for phase tests ONLY when report.synthetic is true.
    pub(crate) decision_ns: u64,
    pub(crate) kind: ObservationKind,
}

#[derive(Clone, Copy)]
pub(crate) struct Frame {
    pub(crate) id: u64,
    pub(crate) allocated_ns: u64,
    decision_ns: u64,
    pub(crate) token: u64,
}
#[derive(Clone, Debug)]
pub(crate) struct Report {
    pub(crate) synthetic: bool,
    pub(crate) allocated: u64,
    pub(crate) phase: Phase,
    pub(crate) warmup_closed_ns: Option<u64>,
    pub(crate) failed: bool,
    pub(crate) closed: bool,
    pub(crate) lost: u64,
    pub(crate) observations: Vec<Observation>,
}
/// Trusted coordinator input, in the decision-clock domain declared by Clock.
/// A synthetic receipt is NOT an observed native broker timestamp. Lua cannot
/// construct, select, or alter this context; event.time is never used for it.
#[derive(Clone, Copy)]
pub(crate) struct Receipt {
    pub(crate) sequence: u64,
    pub(crate) received_ns: u64,
    pub(crate) event: TouchEvent,
}
#[derive(Clone, Copy)]
struct PendingInput {
    receipt: Receipt,
    token: u64,
}
struct Delivery {
    receipt: Receipt,
    mutated: bool,
}
struct State {
    bound: bool,
    observer: Option<crate::diagnostic_observer::Capture>,
    receipts: VecDeque<Receipt>,
    last_receipt_sequence: u64,
    last_receipt_ns: u64,
    coordinator_frontier_ns: u64,
    delivery: Option<Delivery>,
    contact: Option<u32>,
    receipt_contact: Option<u32>,
    receipt_unanswered: Option<u64>,
    token: u64,
    unanswered: Option<PendingInput>,
    first_warmup_ns: Option<u64>,
    last_decision_ns: u64,
    periodic: Option<u64>,
    retired: bool,
    outstanding: Vec<Frame>,
    current_frame: Option<Frame>,
    report: Report,
}
impl State {
    fn observe_raw(&self, at: Stamp, kind: ObservationKind) {
        if let Some(observer) = &self.observer {
            observer.record_at(
                Ok(at.host_ns),
                crate::diagnostic_observer::EventKind::FixtureObserved {
                    synthetic: self.report.synthetic,
                    decision_ns: at.decision_ns,
                    kind,
                },
            );
        }
    }
    fn observe(&mut self, plan: &Plan, at: Stamp, kind: ObservationKind) -> Result<()> {
        if plan.mode != Mode::C {
            return Ok(());
        }
        self.observe_raw(at, kind);
        if self.report.observations.len() == MAX_OBSERVATIONS {
            self.report.lost = self.report.lost.saturating_add(1);
            self.report.failed = true;
            if !matches!(kind, ObservationKind::Failed) {
                self.observe_raw(at, ObservationKind::Failed);
            }
            anyhow::bail!("fixture local observation capacity exhausted");
        }
        self.report.observations.push(Observation {
            host_ns: at.host_ns,
            decision_ns: at.decision_ns,
            kind,
        });
        if self
            .observer
            .as_ref()
            .is_some_and(|observer| observer.ensure_open_and_healthy().is_err())
        {
            self.report.failed = true;
            if !matches!(kind, ObservationKind::Failed) {
                self.observe_raw(at, ObservationKind::Failed);
            }
            anyhow::bail!("fixture raw source lost an observation or is unhealthy");
        }
        Ok(())
    }
    fn require(&mut self, plan: &Plan, at: Stamp, condition: bool, reason: &str) -> Result<()> {
        if !condition {
            self.report.failed = true;
            let _ = self.observe(plan, at, ObservationKind::Failed);
            anyhow::bail!("{reason}");
        }
        Ok(())
    }
    fn check_clock(&mut self, plan: &Plan, at: Stamp) -> Result<()> {
        self.require(
            plan,
            at,
            at.decision_ns >= self.last_decision_ns,
            "fixture clock regressed",
        )?;
        self.last_decision_ns = at.decision_ns;
        Ok(())
    }
    fn check_observer(&mut self, plan: &Plan, at: Stamp) -> Result<()> {
        let healthy = self
            .observer
            .as_ref()
            .is_none_or(|observer| observer.ensure_open_and_healthy().is_ok());
        self.require(
            plan,
            at,
            healthy,
            "fixture raw source is closed or unhealthy",
        )
    }
    fn refresh(&mut self, plan: &Plan, at: Stamp) -> Result<Phase> {
        self.check_observer(plan, at)?;
        self.require(plan, at, !self.report.failed, "fixture attempt has failed")?;
        self.require(plan, at, !self.report.closed, "fixture already closed")?;
        self.check_clock(plan, at)?;
        let phase = if at.decision_ns >= plan.start_ns {
            self.require(
                plan,
                at,
                self.report.warmup_closed_ns.is_some(),
                "warmup was not closed strictly before T",
            )?;
            if at.decision_ns >= plan.end_ns {
                Phase::Ended
            } else if at.decision_ns >= plan.stop_ns {
                Phase::Drain
            } else {
                Phase::Measured
            }
        } else if self.report.warmup_closed_ns.is_some() {
            Phase::Armed
        } else if self
            .first_warmup_ns
            .is_some_and(|first| at.decision_ns.saturating_sub(first) >= WARMUP_NS)
        {
            Phase::Quiescing
        } else {
            Phase::Warmup
        };
        if self.report.phase != phase {
            self.report.phase = phase;
            self.observe(plan, at, ObservationKind::Phase(phase))?;
        }
        Ok(phase)
    }
}
#[derive(Clone)]
pub(crate) struct Fixture {
    plan: Arc<Plan>,
    clock: Clock,
    state: Arc<Mutex<State>>,
}
impl Fixture {
    pub(crate) fn new(plan: Plan, clock: Clock) -> Result<Self> {
        let at = clock.sample()?;
        ensure!(
            at.decision_ns < plan.start_ns,
            "fixture epoch must be future"
        );
        let mut observations = Vec::new();
        if plan.mode == Mode::C {
            observations.try_reserve_exact(MAX_OBSERVATIONS)?;
        }
        let mut outstanding = Vec::new();
        outstanding.try_reserve_exact(MAX_OUTSTANDING)?;
        Ok(Self {
            plan: Arc::new(plan),
            state: Arc::new(Mutex::new(State {
                bound: false,
                observer: None,
                receipts: VecDeque::with_capacity(MAX_OUTSTANDING),
                last_receipt_sequence: 0,
                last_receipt_ns: at.decision_ns,
                coordinator_frontier_ns: at.decision_ns,
                delivery: None,
                contact: None,
                receipt_contact: None,
                receipt_unanswered: None,
                token: 0,
                unanswered: None,
                first_warmup_ns: None,
                last_decision_ns: at.decision_ns,
                periodic: None,
                retired: false,
                outstanding,
                current_frame: None,
                report: Report {
                    synthetic: clock.synthetic(),
                    allocated: 0,
                    phase: Phase::Warmup,
                    warmup_closed_ns: None,
                    failed: false,
                    closed: false,
                    lost: 0,
                    observations,
                },
            })),
            clock,
        })
    }
    pub(crate) fn mode(&self) -> Mode {
        self.plan.mode
    }
    pub(crate) fn attach_observer(
        &self,
        observer: crate::diagnostic_observer::Capture,
    ) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        let valid = self.plan.mode == Mode::C && !state.bound && state.observer.is_none();
        state.require(
            &self.plan,
            at,
            valid,
            "fixture sink requires unused C capability",
        )?;
        state.observer = Some(observer);
        Ok(())
    }
    pub(crate) fn bind_source(&self, bytes: &[u8]) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        let valid = bytes == SOURCE && !state.bound && !state.report.failed;
        state.require(
            &self.plan,
            at,
            valid,
            "private fixture source does not match reviewed bytes, or capability already used",
        )?;
        state.bound = true;
        Ok(())
    }
    pub(crate) fn allocate(&self) -> Result<Option<Frame>> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        let phase = state.refresh(&self.plan, at)?;
        let causal_drain = phase == Phase::Drain
            && state.unanswered.is_some_and(|input| {
                input.receipt.received_ns >= self.plan.start_ns
                    && input.receipt.received_ns < self.plan.stop_ns
                    && !state
                        .outstanding
                        .iter()
                        .any(|frame| frame.token == input.token)
            });
        if !matches!(phase, Phase::Warmup | Phase::Measured) && !causal_drain {
            state.observe(&self.plan, at, ObservationKind::Suppressed)?;
            return Ok(None);
        }
        let valid = state.bound
            && state.current_frame.is_none()
            && state.outstanding.len() < MAX_OUTSTANDING;
        state.require(
            &self.plan,
            at,
            valid,
            "invalid or exhausted fixture allocation context",
        )?;
        state.report.allocated = state
            .report
            .allocated
            .checked_add(1)
            .context("fixture allocation ID overflow")?;
        if phase == Phase::Warmup {
            state.first_warmup_ns.get_or_insert(at.decision_ns);
        }
        let frame = Frame {
            id: state.report.allocated,
            allocated_ns: at.host_ns,
            decision_ns: at.decision_ns,
            token: state.token,
        };
        state.current_frame = Some(frame);
        state.outstanding.push(frame);
        state.observe(
            &self.plan,
            at,
            ObservationKind::Allocated {
                frame_id: frame.id,
                token: frame.token,
            },
        )?;
        Ok(Some(frame))
    }
    pub(crate) fn bind_periodic(&self, id: u64, interval: f64) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        let valid = state.periodic.is_none()
            && interval.to_bits() == (1.0 / self.plan.rate as f64).to_bits();
        state.require(
            &self.plan,
            at,
            valid,
            "fixture must install its single declared periodic timer",
        )?;
        state.periodic = Some(id);
        Ok(())
    }
    pub(crate) fn retire_periodic(&self) -> Result<Option<u64>> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        state.refresh(&self.plan, at)?;
        if at.decision_ns < self.plan.stop_ns || state.retired {
            return Ok(None);
        }
        state.retired = true;
        if let Some(timer_id) = state.periodic {
            state.observe(
                &self.plan,
                at,
                ObservationKind::PeriodicRetired { timer_id },
            )?;
        }
        Ok(state.periodic)
    }
    fn status(&self) -> Result<Phase> {
        let at = self.clock.sample()?;
        self.state.lock().unwrap().refresh(&self.plan, at)
    }
    /// Trusted coordinator contract: receive/resolve notifications MUST follow
    /// original source event order (ties use call order), not forwarding arrival
    /// order. The coordinator must reconcile/buffer out-of-order sources first.
    /// `resolved_ns` is its supplied terminal endpoint in the declared decision
    /// clock domain; local confirmation time is separately observed, never used
    /// as the endpoint. These assertions do NOT authenticate a native response.
    pub(crate) fn resolve_frame(
        &self,
        id: u64,
        resolution: Resolution,
        resolved_ns: u64,
    ) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        state.check_clock(&self.plan, at)?;
        let index = state.outstanding.iter().position(|frame| frame.id == id);
        state.require(
            &self.plan,
            at,
            index.is_some(),
            "unknown or repeated frame resolution",
        )?;
        let valid = resolved_ns >= state.outstanding[index.unwrap()].decision_ns
            && resolved_ns >= state.coordinator_frontier_ns
            && resolved_ns <= at.decision_ns
            && !state.report.closed;
        state.require(
            &self.plan,
            at,
            valid,
            "out-of-order or impossible coordinator terminal endpoint",
        )?;
        state.coordinator_frontier_ns = resolved_ns;
        let frame = state.outstanding.swap_remove(index.unwrap());
        state.observe(
            &self.plan,
            at,
            ObservationKind::Resolved {
                frame_id: id,
                resolution,
                resolved_ns,
            },
        )?;
        state.require(
            &self.plan,
            at,
            resolution != Resolution::Failed && resolved_ns <= self.plan.end_ns,
            "failed or late frame resolution",
        )?;
        if resolution == Resolution::Presented {
            if let Some(input) = state.unanswered.filter(|input| input.token == frame.token) {
                state.unanswered = None;
                state.receipt_unanswered = None;
                state.observe(
                    &self.plan,
                    at,
                    ObservationKind::ResponseConfirmed {
                        receipt_sequence: input.receipt.sequence,
                        frame_id: id,
                        token: input.token,
                        responded_ns: resolved_ns,
                    },
                )?;
            }
        }
        Ok(())
    }
    /// Explicit cross-role assertion: no outstanding warmup frames, inputs or
    /// contacts remain. The worker must not invent this from local readiness.
    pub(crate) fn confirm_warmup_closed(&self) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        let phase = state.refresh(&self.plan, at)?;
        let valid = phase == Phase::Quiescing
            && state.receipt_contact.is_none()
            && state.receipt_unanswered.is_none()
            && state.outstanding.is_empty()
            && state.periodic.is_some()
            && state.receipts.is_empty()
            && state.delivery.is_none()
            && state.contact.is_none()
            && state.unanswered.is_none();
        state.require(
            &self.plan,
            at,
            valid,
            "warmup closure requires five seconds and resolved work before T",
        )?;
        state.observe(&self.plan, at, ObservationKind::WarmupClosed)?;
        state.report.warmup_closed_ns = Some(at.decision_ns);
        state.report.phase = Phase::Armed;
        Ok(())
    }
    pub(crate) fn finish(&self) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        let phase = state.refresh(&self.plan, at)?;
        let valid = phase == Phase::Ended
            && state.receipt_contact.is_none()
            && state.receipt_unanswered.is_none()
            && state.outstanding.is_empty()
            && state.receipts.is_empty()
            && state.delivery.is_none()
            && state.contact.is_none()
            && state.unanswered.is_none();
        state.require(
            &self.plan,
            at,
            valid,
            "fixture closure requires E and resolved work",
        )?;
        state.observe(&self.plan, at, ObservationKind::Closed)?;
        state.report.closed = true;
        Ok(())
    }
    pub(crate) fn receive(&self, receipt: Receipt) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        state.observe(
            &self.plan,
            at,
            ObservationKind::ReceiptSupplied {
                receipt_sequence: receipt.sequence,
                received_ns: receipt.received_ns,
                contact: receipt.event.id,
                phase: receipt.event.phase,
            },
        )?;
        state.check_clock(&self.plan, at)?;
        let valid = receipt.sequence == state.last_receipt_sequence + 1
            && receipt.received_ns >= state.last_receipt_ns
            && receipt.received_ns >= state.coordinator_frontier_ns
            && receipt.received_ns <= at.decision_ns
            && state.receipts.len() < MAX_OUTSTANDING
            && !state.report.closed;
        state.require(
            &self.plan,
            at,
            valid,
            "invalid, out-of-order or exhausted coordinator receipt context",
        )?;
        // Never exclude an unexpected down silently, or use worker delivery time
        // or TouchEvent.time to decide which side of S it belongs to.
        let allowed = receipt.received_ns <= self.plan.end_ns
            && !(state.report.warmup_closed_ns.is_some()
                && receipt.received_ns < self.plan.start_ns)
            && (receipt.received_ns < self.plan.start_ns || self.plan.causal)
            && !(receipt.received_ns >= self.plan.stop_ns
                && receipt.event.phase == TouchPhase::Down)
            && receipt.event.phase != TouchPhase::Cancel;
        state.require(
            &self.plan,
            at,
            allowed,
            "unexpected input for fixture case or phase",
        )?;
        match receipt.event.phase {
            TouchPhase::Down => {
                let valid = state.receipt_contact.is_none() && state.receipt_unanswered.is_none();
                state.require(
                    &self.plan,
                    at,
                    valid,
                    "concurrent or unanswered down at trusted receipt",
                )?;
                state.receipt_contact = Some(receipt.event.id);
                state.receipt_unanswered = Some(receipt.sequence);
            }
            TouchPhase::Move | TouchPhase::Up => {
                let valid = state.receipt_contact == Some(receipt.event.id);
                state.require(
                    &self.plan,
                    at,
                    valid,
                    "broken trusted receipt contact order",
                )?;
                if receipt.event.phase == TouchPhase::Up {
                    state.receipt_contact = None;
                }
            }
            TouchPhase::Cancel => unreachable!("cancel receipt rejected above"),
        }
        state.last_receipt_sequence = receipt.sequence;
        state.last_receipt_ns = receipt.received_ns;
        state.coordinator_frontier_ns = receipt.received_ns;
        state.receipts.push_back(receipt);
        Ok(())
    }
    pub(crate) fn enter_delivery(&self, event: TouchEvent) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        state.refresh(&self.plan, at)?;
        state.require(
            &self.plan,
            at,
            at.decision_ns <= self.plan.end_ns,
            "callback delivery after E",
        )?;
        let receipt = state.receipts.pop_front();
        let valid = state.delivery.is_none() && receipt.is_some_and(|r| r.event == event);
        state.require(
            &self.plan,
            at,
            valid,
            "missing or mismatched trusted receipt for callback",
        )?;
        let receipt = receipt.unwrap();
        match event.phase {
            TouchPhase::Down => {
                let valid = state.contact.is_none() && state.unanswered.is_none();
                state.require(
                    &self.plan,
                    at,
                    valid,
                    "concurrent contact or unanswered token",
                )?;
                state.contact = Some(event.id);
            }
            TouchPhase::Move | TouchPhase::Up => {
                let valid = state.contact == Some(event.id);
                state.require(&self.plan, at, valid, "broken contact ordering")?;
                if event.phase == TouchPhase::Up {
                    state.contact = None;
                }
            }
            TouchPhase::Cancel => {
                state.require(&self.plan, at, false, "cancelled isolated contact")?
            }
        }
        state.delivery = Some(Delivery {
            receipt,
            mutated: false,
        });
        state.observe(
            &self.plan,
            at,
            ObservationKind::Delivered {
                receipt_sequence: receipt.sequence,
            },
        )?;
        Ok(())
    }
    fn advance_token(&self) -> Result<u64> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        state.check_clock(&self.plan, at)?;
        let valid = state
            .delivery
            .as_ref()
            .is_some_and(|d| d.receipt.event.phase == TouchPhase::Down && !d.mutated)
            && state.token < i64::MAX as u64;
        state.require(
            &self.plan,
            at,
            valid,
            "token mutation requires the delivered down callback exactly once",
        )?;
        let receipt = state.delivery.as_ref().unwrap().receipt;
        let previous = state.token;
        state.token += 1;
        let token = state.token;
        state.delivery.as_mut().unwrap().mutated = true;
        state.unanswered = Some(PendingInput { receipt, token });
        state.observe(
            &self.plan,
            at,
            ObservationKind::TokenMutated {
                receipt_sequence: receipt.sequence,
                previous,
                token,
            },
        )?;
        Ok(token)
    }
    pub(crate) fn exit_delivery(&self, success: bool) -> Result<()> {
        let at = self.clock.sample()?;
        let mut state = self.state.lock().unwrap();
        state.check_clock(&self.plan, at)?;
        let delivery = state.delivery.take();
        let valid = success
            && delivery.is_some_and(|d| d.receipt.event.phase != TouchPhase::Down || d.mutated);
        state.require(
            &self.plan,
            at,
            valid,
            "failed callback or missing down-token mutation",
        )
    }
    /// Runtime errors outside capability validation also invalidate the attempt.
    /// A failed clock read must not manufacture a host or decision timestamp.
    pub(crate) fn fail_runtime(&self) {
        let at = self.clock.sample();
        let mut state = self.state.lock().unwrap();
        if !state.report.failed {
            state.report.failed = true;
            if let Ok(at) = at {
                let _ = state.observe(&self.plan, at, ObservationKind::Failed);
            }
        }
    }
    pub(crate) fn ensure_runtime_active(&self) -> Result<()> {
        let state = self.state.lock().unwrap();
        ensure!(
            !state.report.failed && !state.report.closed,
            "fixture is failed or closed"
        );
        Ok(())
    }
    pub(crate) fn finish_render(&self) {
        self.state.lock().unwrap().current_frame = None;
    }
    pub(crate) fn report(&self) -> Report {
        self.state.lock().unwrap().report.clone()
    }
    fn frame(&self) -> Result<Frame> {
        self.state
            .lock()
            .unwrap()
            .current_frame
            .context("fixture identity only available during rendering")
    }
    pub(crate) fn capability(&self, lua: &Lua) -> mlua::Result<AnyUserData> {
        let marker = lua
            .load(MARKER)
            .set_name("=private reviewed marker")
            .eval::<Table>()?
            .get::<Function>("draw")?;
        lua.create_userdata(Capability {
            fixture: self.clone(),
            marker,
        })
    }
}
struct Capability {
    fixture: Fixture,
    marker: Function,
}
impl UserData for Capability {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("rate", |_, this| Ok(this.fixture.plan.rate));
        fields.add_field_method_get("marker_enabled", |_, this| {
            Ok(this.fixture.plan.mode != Mode::A)
        });
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("advance_token", |_, this, ()| {
            this.fixture.advance_token().map_err(mlua::Error::external)
        });
        methods.add_method("status", |_, this, ()| {
            this.fixture
                .status()
                .map(Phase::name)
                .map_err(mlua::Error::external)
        });
        methods.add_method("frame", |lua, this, ()| {
            let frame = this.fixture.frame().map_err(mlua::Error::external)?;
            let table = lua.create_table()?;
            table.set("frame_id", frame.id)?;
            table.set("allocated_ns", frame.allocated_ns)?;
            table.set("token", frame.token)?;
            Ok(table)
        });
        methods.add_method("draw_marker", |lua, this, canvas: AnyUserData| {
            let frame = this.fixture.frame().map_err(mlua::Error::external)?;
            let identity = lua.create_table()?;
            identity.set("run_id", this.fixture.plan.run.as_str())?;
            identity.set("generation", this.fixture.plan.generation.as_str())?;
            identity.set("frame_id", frame.id)?;
            if frame.token != 0 {
                identity.set("input_id", frame.token.to_string())?;
            }
            this.marker.call::<()>((canvas, identity))
        });
    }
}
