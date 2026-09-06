use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};

use crate::apply_ipc::absolute_lexical;
use crate::authorization::{AuthorizationGrant, SessionAuthorizer};
use crate::config_selection::ConfigSelection;
use crate::default_source;
use crate::frame_canvas::FrameCanvas;
use crate::hardware::{
    modifier_output_keys, tap_key_events, ContactId, HardwareCapability, HardwareEvent, InputState,
    InputTransition, KeyboardKey, LogicalFrame, ObservedKey, OutputKey, SyntheticKeyEvent,
    TouchBarHardware, TouchEvent, TouchPhase,
};
use crate::logind::{Logind, RealLogind};
use crate::lua_worker::{
    earliest_deadline, DriveRequest, KeyOperation, KeyRequest, LuaSource, LuaWorker, ModifierMode,
    StagedLuaWorker, StopReason, VisibilityReason, WorkerEffects, WorkerIdentity,
};
use crate::path_state::{PathStateSnapshot, PreparedPathState};
use crate::peer_credentials::PeerCredentials;
use crate::recovery::{RecoverySession, RecoveryTouchResult};

const MAX_POLL_WAIT: Duration = Duration::from_millis(50);
const PHYSICAL_FN_RECOVERY_HOLD_SECONDS: f64 = 2.0;
const REQUEST_QUEUE_CAPACITY: usize = 16;
const TOUCH_QUEUE_CAPACITY: usize = 256;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

struct ActiveConfig {
    worker: LuaWorker,
    _source: LuaSource,
    frame: LogicalFrame,
    backlight: f64,
    contacts: BTreeMap<ContactId, TouchEvent>,
}

struct ApplyAuthorization {
    peer: PeerCredentials,
    grant: AuthorizationGrant,
}

struct AuthorizedRequest {
    request: ConfigSelection,
    authorization: ApplyAuthorization,
}

enum SelectionState {
    Keep,
    Set(PathBuf),
    Clear,
}

enum CandidateFailure {
    Candidate(anyhow::Error),
    Authorization(anyhow::Error),
}

impl From<anyhow::Error> for CandidateFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::Candidate(error)
    }
}

struct TouchQueue {
    events: Vec<Option<TouchEvent>>,
    moves: BTreeMap<ContactId, usize>,
}

#[derive(Clone, Default)]
struct SyntheticState {
    held: Vec<HeldSyntheticKey>,
}

#[derive(Clone)]
struct HeldSyntheticKey {
    key: OutputKey,
    modifiers: Vec<OutputKey>,
}

struct CandidateRollback<'a> {
    previous_path_state: Option<&'a PathStateSnapshot>,
    old_frame: Option<&'a LogicalFrame>,
    old_backlight: f64,
    restore_frame: bool,
    restore_backlight: bool,
    old_synthetic: Option<&'a SyntheticState>,
}

fn cancel_contacts(
    worker: &LuaWorker,
    contacts: &BTreeMap<ContactId, TouchEvent>,
    now: f64,
    request: DriveRequest,
) -> Result<WorkerEffects> {
    cancel_contacts_until(
        worker,
        contacts,
        now,
        request,
        Instant::now() + Duration::from_secs(2),
    )
}

fn cancel_contacts_until(
    worker: &LuaWorker,
    contacts: &BTreeMap<ContactId, TouchEvent>,
    now: f64,
    request: DriveRequest,
    deadline: Instant,
) -> Result<WorkerEffects> {
    let events = contacts
        .values()
        .map(|event| TouchEvent {
            phase: TouchPhase::Cancel,
            time: now,
            ..*event
        })
        .collect();
    worker.drive_until(request.with_events(events), deadline)
}

impl TouchQueue {
    fn new() -> Self {
        Self {
            events: Vec::with_capacity(TOUCH_QUEUE_CAPACITY),
            moves: BTreeMap::new(),
        }
    }

    fn push(&mut self, event: TouchEvent) -> Result<()> {
        if event.phase == TouchPhase::Move {
            if let Some(index) = self.moves.get(&event.id).copied() {
                self.events[index] = None;
                if self.events.len() >= TOUCH_QUEUE_CAPACITY {
                    self.compact();
                }
                self.moves.insert(event.id, self.events.len());
            } else {
                if self.events.len() >= TOUCH_QUEUE_CAPACITY {
                    return Ok(());
                }
                self.moves.insert(event.id, self.events.len());
            }
        } else {
            self.moves.remove(&event.id);
        }
        ensure!(
            self.events.len() < TOUCH_QUEUE_CAPACITY,
            "non-droppable Touch Bar input queue overflow"
        );
        self.events.push(Some(event));
        Ok(())
    }

    fn compact(&mut self) {
        let mut moves = BTreeMap::new();
        let events = std::mem::take(&mut self.events)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(index, event)| {
                if event.phase == TouchPhase::Move {
                    moves.insert(event.id, index);
                }
                Some(event)
            })
            .collect();
        self.events = events;
        self.moves = moves;
    }

    fn drain(&mut self) -> Vec<TouchEvent> {
        self.moves.clear();
        std::mem::take(&mut self.events)
            .into_iter()
            .flatten()
            .collect()
    }
}

impl SyntheticState {
    fn plan(
        &self,
        requests: &[KeyRequest],
        input_state: InputState,
    ) -> Result<(Self, Vec<SyntheticKeyEvent>)> {
        let mut next = self.clone();
        let mut events = Vec::new();
        for request in requests {
            match request.operation {
                KeyOperation::Down => {
                    ensure!(
                        !next.is_key_held(request.key),
                        "synthetic key is already held"
                    );
                    let modifiers = next.resolve_modifiers(&request.modifiers, input_state)?;
                    ensure!(
                        !modifiers.contains(&request.key),
                        "a synthetic key cannot mirror itself"
                    );
                    for modifier in &modifiers {
                        if next.modifier_count(*modifier) == 0 {
                            events.push(SyntheticKeyEvent {
                                key: *modifier,
                                active: true,
                            });
                        }
                    }
                    if !is_modifier_key(request.key) || next.modifier_count(request.key) == 0 {
                        events.push(SyntheticKeyEvent {
                            key: request.key,
                            active: true,
                        });
                    }
                    next.held.push(HeldSyntheticKey {
                        key: request.key,
                        modifiers,
                    });
                }
                KeyOperation::Up => {
                    let index = next
                        .held
                        .iter()
                        .position(|held| held.key == request.key)
                        .context("synthetic key is not held")?;
                    let HeldSyntheticKey { modifiers, .. } = next.held.remove(index);
                    if !is_modifier_key(request.key) || next.modifier_count(request.key) == 0 {
                        events.push(SyntheticKeyEvent {
                            key: request.key,
                            active: false,
                        });
                    }
                    for modifier in modifiers.into_iter().rev() {
                        if next.modifier_count(modifier) == 0 {
                            events.push(SyntheticKeyEvent {
                                key: modifier,
                                active: false,
                            });
                        }
                    }
                }
                KeyOperation::Tap => {
                    ensure!(
                        !is_modifier_key(request.key) || next.modifier_count(request.key) == 0,
                        "synthetic modifier is already held"
                    );
                    ensure!(
                        !next.is_key_held(request.key),
                        "synthetic key is already held"
                    );
                    let modifiers = next.resolve_modifiers(&request.modifiers, input_state)?;
                    ensure!(
                        !modifiers.contains(&request.key),
                        "a synthetic key cannot mirror itself"
                    );
                    let mirrored: Vec<_> = modifiers
                        .into_iter()
                        .filter(|modifier| next.modifier_count(*modifier) == 0)
                        .collect();
                    events.extend(tap_key_events(request.key, &mirrored));
                }
            }
        }
        Ok((next, events))
    }

    fn resolve_modifiers(
        &self,
        mode: &ModifierMode,
        input_state: InputState,
    ) -> Result<Vec<OutputKey>> {
        let modifiers = match mode {
            ModifierMode::Inherit => modifier_output_keys(input_state.modifiers),
            ModifierMode::None => Vec::new(),
            ModifierMode::Explicit(keys) => keys.clone(),
        };
        let mut seen = BTreeSet::new();
        for key in &modifiers {
            ensure!(
                is_modifier_key(*key),
                "explicit modifiers must be modifier keys"
            );
            ensure!(
                seen.insert(*key),
                "explicit modifiers must not contain duplicates"
            );
        }
        Ok(modifiers)
    }

    fn is_key_held(&self, key: OutputKey) -> bool {
        self.held.iter().any(|held| held.key == key)
    }

    fn modifier_count(&self, modifier: OutputKey) -> usize {
        self.held.iter().filter(|held| held.key == modifier).count()
            + self
                .held
                .iter()
                .flat_map(|held| &held.modifiers)
                .filter(|held| **held == modifier)
                .count()
    }

    fn release(&self) -> (Self, Vec<SyntheticKeyEvent>) {
        let mut remaining = self.clone();
        let mut events = Vec::new();
        while let Some(HeldSyntheticKey { key, modifiers }) = remaining.held.pop() {
            if !is_modifier_key(key) || remaining.modifier_count(key) == 0 {
                events.push(SyntheticKeyEvent { key, active: false });
            }
            for modifier in modifiers.into_iter().rev() {
                if remaining.modifier_count(modifier) == 0 {
                    events.push(SyntheticKeyEvent {
                        key: modifier,
                        active: false,
                    });
                }
            }
        }
        (Self::default(), events)
    }

    fn restore_events(&self) -> Vec<SyntheticKeyEvent> {
        let mut restored = Self::default();
        let mut events = Vec::new();
        for held in &self.held {
            for modifier in &held.modifiers {
                if restored.modifier_count(*modifier) == 0 {
                    events.push(SyntheticKeyEvent {
                        key: *modifier,
                        active: true,
                    });
                }
            }
            if !is_modifier_key(held.key) || restored.modifier_count(held.key) == 0 {
                events.push(SyntheticKeyEvent {
                    key: held.key,
                    active: true,
                });
            }
            restored.held.push(held.clone());
        }
        events
    }
}

fn is_modifier_key(key: OutputKey) -> bool {
    matches!(
        key,
        OutputKey::Keyboard(
            KeyboardKey::LeftCtrl
                | KeyboardKey::RightCtrl
                | KeyboardKey::LeftAlt
                | KeyboardKey::RightAlt
                | KeyboardKey::LeftShift
                | KeyboardKey::RightShift
                | KeyboardKey::LeftSuper
                | KeyboardKey::RightSuper
        )
    )
}

pub(crate) struct Supervisor<H: TouchBarHardware, L: Logind = RealLogind> {
    hardware: H,
    state_file: PathBuf,
    default_source: LuaSource,
    fallback_worker: bool,
    #[cfg(test)]
    worker_process_backend: bool,
    #[cfg(test)]
    worker_systemd_backend: bool,
    active: Option<ActiveConfig>,
    recovery: Option<RecoverySession>,
    claimed: bool,
    hardware_available: bool,
    missing_capabilities: BTreeSet<HardwareCapability>,
    suspended: bool,
    resume_pending: bool,
    worker_visible: bool,
    origin: Instant,
    backlight: f64,
    input_state: InputState,
    down_contacts: BTreeMap<ContactId, TouchEvent>,
    ignored_contacts: BTreeSet<ContactId>,
    touch_queue: TouchQueue,
    input_transitions: Vec<InputTransition>,
    next_worker_deadline: Option<f64>,
    fn_hold_started: Option<f64>,
    #[cfg(test)]
    worker_failure: Option<String>,
    synthetic: SyntheticState,
    authorizer: SessionAuthorizer<L>,
    last_presented_time: Option<f64>,
}

type RecoveryTouchDispatch = fn(&mut RecoverySession, TouchEvent) -> RecoveryTouchResult;
type ActiveTouchDispatch<H, L> = fn(&mut Supervisor<H, L>, TouchEvent) -> Result<()>;

impl<H: TouchBarHardware> Supervisor<H, RealLogind> {
    #[cfg(test)]
    pub(crate) fn new(hardware: H, state_file: PathBuf) -> Result<Self> {
        Self::new_with_logind(hardware, state_file, RealLogind::default())
    }

    pub(crate) fn new_fallback(hardware: H, state_file: PathBuf) -> Result<Self> {
        let mut supervisor = Self::new_with_logind_and_default(
            hardware,
            state_file,
            RealLogind::default(),
            default_source::source(),
        )?;
        supervisor.fallback_worker = true;
        Ok(supervisor)
    }
}

impl<H: TouchBarHardware, L: Logind> Supervisor<H, L> {
    #[cfg(test)]
    pub(crate) fn new_fallback_with_logind(
        hardware: H,
        state_file: PathBuf,
        logind: L,
        injected_default: Option<LuaSource>,
    ) -> Result<Self> {
        let default_source = injected_default.unwrap_or_else(default_source::source);
        let mut supervisor =
            Self::new_with_logind_and_default(hardware, state_file, logind, default_source)?;
        supervisor.fallback_worker = true;
        #[cfg(test)]
        {
            supervisor.worker_process_backend = true;
        }
        Ok(supervisor)
    }

    #[cfg(test)]
    pub(crate) fn new_with_logind(hardware: H, state_file: PathBuf, logind: L) -> Result<Self> {
        Self::new_with_logind_and_default(hardware, state_file, logind, default_source::source())
    }

    #[cfg(test)]
    pub(crate) fn new_with_logind_process(
        hardware: H,
        state_file: PathBuf,
        logind: L,
    ) -> Result<Self> {
        let mut supervisor = Self::new_with_logind(hardware, state_file, logind)?;
        supervisor.worker_process_backend = true;
        Ok(supervisor)
    }

    #[cfg(test)]
    pub(crate) fn new_with_startup_candidate_process(
        hardware: H,
        state_file: PathBuf,
        logind: L,
        injected_default: Option<LuaSource>,
    ) -> Result<Self> {
        Self::new_with_startup_candidate_backend(
            hardware,
            state_file,
            logind,
            injected_default,
            false,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_startup_candidate_systemd(
        hardware: H,
        state_file: PathBuf,
        logind: L,
        injected_default: Option<LuaSource>,
    ) -> Result<Self> {
        Self::new_with_startup_candidate_backend(
            hardware,
            state_file,
            logind,
            injected_default,
            true,
        )
    }

    #[cfg(test)]
    fn new_with_startup_candidate_backend(
        hardware: H,
        state_file: PathBuf,
        logind: L,
        injected_default: Option<LuaSource>,
        systemd_backend: bool,
    ) -> Result<Self> {
        let default_source = injected_default.unwrap_or_else(default_source::source);
        let mut supervisor = Self::new_with_logind_process(hardware, state_file.clone(), logind)?;
        supervisor.worker_systemd_backend = systemd_backend;
        supervisor.default_source = default_source;
        let saved = read_selected_path(&state_file)?;
        let selection = saved.map_or(ConfigSelection::Default, ConfigSelection::Path);
        supervisor.startup_candidate_or_default(selection)?;
        Ok(supervisor)
    }

    fn new_with_logind_and_default(
        mut hardware: H,
        state_file: PathBuf,
        logind: L,
        default_source: LuaSource,
    ) -> Result<Self> {
        let claimed = match hardware.claim() {
            Ok(()) => true,
            Err(error) if !hardware.is_available() => {
                eprintln!("Touch Bar hardware is unavailable during startup: {error:#}");
                false
            }
            Err(error) => return Err(error),
        };
        let input_state = hardware.input_state();
        let backlight = if claimed {
            match hardware.get_backlight() {
                Ok(level) => level,
                Err(error) if !hardware.is_available() => {
                    eprintln!("Touch Bar backlight is unavailable during startup: {error:#}");
                    0.0
                }
                Err(error) => {
                    let _ = hardware.release();
                    return Err(error).context("reading initial Touch Bar backlight");
                }
            }
        } else {
            0.0
        };
        let hardware_available = claimed && hardware.is_available();
        let missing_capabilities = if hardware_available {
            BTreeSet::new()
        } else {
            std::iter::once(
                hardware
                    .unavailable_capability()
                    .unwrap_or(HardwareCapability::Display),
            )
            .collect()
        };
        Ok(Self {
            hardware,
            state_file,
            default_source,
            fallback_worker: false,
            #[cfg(test)]
            worker_process_backend: false,
            #[cfg(test)]
            worker_systemd_backend: false,
            active: None,
            recovery: None,
            claimed,
            hardware_available,
            missing_capabilities,
            suspended: false,
            resume_pending: false,
            worker_visible: false,
            origin: Instant::now(),
            backlight,
            input_state,
            down_contacts: BTreeMap::new(),
            ignored_contacts: BTreeSet::new(),
            touch_queue: TouchQueue::new(),
            input_transitions: Vec::new(),
            next_worker_deadline: None,
            fn_hold_started: None,
            #[cfg(test)]
            worker_failure: None,
            synthetic: SyntheticState::default(),
            authorizer: SessionAuthorizer::new(logind),
            last_presented_time: None,
        })
    }

    /// Build the running supervisor and make one startup attempt. A saved path
    /// is tried once, then a failed saved source gets one embedded-default
    /// attempt without changing the path. Absent state selects the embedded
    /// source and never writes a path to state.
    #[cfg(test)]
    pub(crate) fn new_with_startup_candidate(
        hardware: H,
        state_file: PathBuf,
        logind: L,
        injected_default: Option<LuaSource>,
    ) -> Result<Self> {
        Self::new_with_startup_candidate_source(
            hardware,
            state_file,
            logind,
            injected_default.unwrap_or_else(default_source::source),
        )
    }

    #[cfg(not(test))]
    pub(crate) fn new_with_startup_candidate(
        hardware: H,
        state_file: PathBuf,
        logind: L,
    ) -> Result<Self> {
        Self::new_with_startup_candidate_source(
            hardware,
            state_file,
            logind,
            default_source::source(),
        )
    }

    fn new_with_startup_candidate_source(
        hardware: H,
        state_file: PathBuf,
        logind: L,
        default_source: LuaSource,
    ) -> Result<Self> {
        let mut supervisor = Self::new_with_logind_and_default(
            hardware,
            state_file.clone(),
            logind,
            default_source,
        )?;
        let saved = read_selected_path(&state_file)?;
        let selection = saved.map_or(ConfigSelection::Default, ConfigSelection::Path);
        supervisor.startup_candidate_or_default(selection)?;
        Ok(supervisor)
    }

    fn startup_candidate_or_default(&mut self, selection: ConfigSelection) -> Result<()> {
        let saved_source = matches!(&selection, ConfigSelection::Path(_));
        if let Err(error) = self.startup_candidate(selection) {
            eprintln!("selected Lua worker failed during startup: {error:#}");
            if saved_source {
                if let Err(default_error) = self.startup_candidate(ConfigSelection::Default) {
                    eprintln!("embedded default Lua worker entered recovery: {default_error:#}");
                    self.enter_recovery()?;
                }
            } else {
                self.enter_recovery()?;
            }
        }
        Ok(())
    }

    fn now_seconds(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    pub(crate) fn has_active_worker(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn has_recovery(&self) -> bool {
        self.recovery.is_some()
    }

    pub(crate) fn is_hardware_available(&self) -> bool {
        self.hardware_available && !self.suspended
    }

    fn hardware_unavailable_error(&self) -> anyhow::Error {
        let reason = self
            .missing_capabilities
            .iter()
            .next()
            .copied()
            .map(HardwareCapability::name)
            .unwrap_or("hardware contract");
        anyhow::anyhow!("hardware interface unavailable: {reason}")
    }

    fn ensure_hardware_available(&self) -> Result<()> {
        ensure!(!self.suspended, "hardware interface unavailable: suspended");
        ensure!(
            self.hardware_available,
            "{:#}",
            self.hardware_unavailable_error()
        );
        Ok(())
    }

    pub(crate) fn poll(&mut self, timeout: Duration) -> Result<()> {
        self.poll_events(timeout).map(|_| ())
    }

    fn ensure_fallback_unowned(&self) -> Result<()> {
        ensure!(
            self.authorizer.active_session("seat0")?.is_none(),
            "a user session became active during fallback staging"
        );
        Ok(())
    }

    fn confirm_candidate_owner(&mut self) -> Result<()> {
        if self.fallback_worker {
            self.ensure_fallback_unowned()
        } else {
            self.hardware.confirm_owner()
        }
    }

    pub(crate) fn start_fallback(&mut self) -> Result<()> {
        ensure!(self.fallback_worker, "supervisor is not the fallback owner");
        if self.authorizer.active_session("seat0")?.is_some() {
            return Ok(());
        }
        if !self.hardware_available || self.suspended {
            return Ok(());
        }
        let state_file = self.state_file.clone();
        let result = self.apply_candidate(
            ConfigSelection::Default,
            None,
            SelectionState::Keep,
            &state_file,
            StopReason::Replaced,
        );
        match result {
            Ok(()) => Ok(()),
            Err(failure) => {
                let error = match failure {
                    CandidateFailure::Candidate(error) | CandidateFailure::Authorization(error) => {
                        error
                    }
                };
                eprintln!("fallback Lua worker entered recovery: {error:#}");
                self.enter_recovery()
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn apply(&mut self, requested_path: &Path) -> Result<()> {
        self.apply_request(ConfigSelection::Path(requested_path.to_path_buf()), None)
    }

    #[cfg(test)]
    pub(crate) fn apply_default(&mut self) -> Result<()> {
        self.apply_request(ConfigSelection::Default, None)
    }

    #[cfg(test)]
    fn set_default_source_for_test(&mut self, bytes: Vec<u8>) {
        self.default_source = LuaSource::embedded(bytes);
    }

    fn apply_authorized(&mut self, request: AuthorizedRequest) -> Result<()> {
        let AuthorizedRequest {
            request,
            authorization,
        } = request;
        self.authorizer
            .recheck(authorization.peer, &authorization.grant)?;
        self.apply_request(request, Some(authorization))
    }

    fn apply_request(
        &mut self,
        selection: ConfigSelection,
        authorization: Option<ApplyAuthorization>,
    ) -> Result<()> {
        self.ensure_hardware_available()?;
        let state_file = authorization
            .as_ref()
            .map(|authorization| state_file_for_uid(&self.state_file, authorization.grant.uid()))
            .unwrap_or_else(|| self.state_file.clone());
        let state_update = match &selection {
            ConfigSelection::Path(path) => SelectionState::Set(absolute_lexical(path)?),
            ConfigSelection::Default => SelectionState::Clear,
        };
        let candidate_error = match self.apply_candidate(
            selection.clone(),
            authorization.as_ref(),
            state_update,
            &state_file,
            StopReason::Replaced,
        ) {
            Ok(()) => return Ok(()),
            Err(CandidateFailure::Authorization(error)) => return Err(error),
            Err(CandidateFailure::Candidate(error)) => error,
        };
        if self.active.is_none() {
            if let ConfigSelection::Path(path) = selection {
                let selected_path = absolute_lexical(&path)?;
                let path_state = PreparedPathState::prepare(&state_file, &selected_path)?;
                if let Some(authorization) = authorization.as_ref() {
                    self.authorizer
                        .recheck(authorization.peer, &authorization.grant)?;
                }
                if let Err(state_error) = path_state.commit() {
                    return Err(candidate_error.context(format!(
                        "preserving failed selected path also failed: {state_error:#}"
                    )));
                }
            }
            if let Some(authorization) = authorization.as_ref() {
                self.authorizer
                    .recheck(authorization.peer, &authorization.grant)?;
            }
            if let Err(recovery_error) = self.enter_recovery() {
                return Err(candidate_error
                    .context(format!("entering recovery also failed: {recovery_error:#}")));
            }
        }
        Err(candidate_error)
    }

    fn startup_candidate(&mut self, selection: ConfigSelection) -> Result<()> {
        let state_file = self.state_file.clone();
        self.apply_candidate(
            selection,
            None,
            SelectionState::Keep,
            &state_file,
            StopReason::Replaced,
        )
        .map_err(|failure| match failure {
            CandidateFailure::Candidate(error) | CandidateFailure::Authorization(error) => error,
        })
    }

    fn apply_candidate(
        &mut self,
        selection: ConfigSelection,
        authorization: Option<&ApplyAuthorization>,
        state_update: SelectionState,
        state_file: &Path,
        stop_reason: StopReason,
    ) -> std::result::Result<(), CandidateFailure> {
        let source = match selection {
            ConfigSelection::Path(requested_path) => {
                let selected_path = absolute_lexical(&requested_path)?;
                let metadata = std::fs::metadata(&selected_path).with_context(|| {
                    format!("reading config metadata for {}", selected_path.display())
                })?;
                if !metadata.is_file() {
                    return Err(anyhow::anyhow!(
                        "config is not a regular file: {}",
                        selected_path.display()
                    )
                    .into());
                }
                LuaSource::file(selected_path)
            }
            ConfigSelection::Default => self.default_source.clone(),
        };

        self.poll_events(Duration::ZERO)?;
        self.ensure_hardware_available()
            .map_err(CandidateFailure::Candidate)?;
        let current_backlight = match self.hardware.get_backlight() {
            Ok(level) => level,
            Err(error) => {
                return Err(self
                    .record_hardware_failure(error, HardwareCapability::Backlight)
                    .into())
            }
        };
        self.backlight = current_backlight;
        let identity = if self.fallback_worker {
            WorkerIdentity::RestrictedFallback
        } else {
            WorkerIdentity::User
        };
        #[cfg(test)]
        let staged_worker = if self.worker_process_backend {
            let stage = if self.worker_systemd_backend {
                LuaWorker::stage_source_with_identity_systemd
            } else {
                LuaWorker::stage_source_with_identity_process
            };
            stage(
                source.clone(),
                current_backlight,
                self.input_state,
                identity,
            )
        } else {
            LuaWorker::stage_source_with_identity(
                source.clone(),
                current_backlight,
                self.input_state,
                identity,
            )
        }?;
        #[cfg(not(test))]
        let staged_worker = LuaWorker::stage_source_with_identity(
            source.clone(),
            current_backlight,
            self.input_state,
            identity,
        )?;
        let StagedLuaWorker { worker } = staged_worker;
        self.poll_hardware_deferred(Duration::ZERO)?;
        self.ensure_hardware_available()
            .map_err(CandidateFailure::Candidate)?;
        let render_now = self.now_seconds();
        let render_time = self
            .last_presented_time
            .map_or(render_now, |last| render_now.max(last));
        let staged_frame = worker.render_at_with_input(render_time, 0.0, self.input_state)?;
        let pending_backlight = worker.pending_backlight()?;
        let frame = staged_frame.frame;
        if let Some(authorization) = authorization {
            self.authorizer
                .recheck(authorization.peer, &authorization.grant)
                .map_err(CandidateFailure::Authorization)?;
        }
        let latest_backlight = match self.hardware.get_backlight() {
            Ok(level) => level,
            Err(error) => {
                return Err(self
                    .record_hardware_failure(error, HardwareCapability::Backlight)
                    .into())
            }
        };
        self.ensure_hardware_available()
            .map_err(CandidateFailure::Candidate)?;
        self.backlight = latest_backlight;
        let (previous_path_state, path_state) = match state_update {
            SelectionState::Keep => (None, None),
            SelectionState::Set(path) => (
                Some(PathStateSnapshot::capture(state_file)?),
                Some(PreparedPathState::prepare(state_file, &path)?),
            ),
            SelectionState::Clear => (
                Some(PathStateSnapshot::capture(state_file)?),
                Some(PreparedPathState::prepare_clear(state_file)?),
            ),
        };
        if let Some(authorization) = authorization {
            self.authorizer
                .recheck(authorization.peer, &authorization.grant)
                .map_err(CandidateFailure::Authorization)?;
        }
        self.confirm_candidate_owner()
            .map_err(CandidateFailure::Candidate)?;

        let old_frame = self.active.as_ref().map(|active| active.frame.clone());
        let old_synthetic = self.synthetic.clone();
        let preserve_recovery = self.has_healthy_recovery();
        let old_backlight = latest_backlight;
        let candidate_backlight = pending_backlight.unwrap_or(old_backlight);
        let brightness_attempted = pending_backlight.is_some();
        let mut brightness_changed = false;
        // This is the last ownership check before committing the staged
        // worker. The broker confirms again after presentation to catch a
        // session change during the hardware operation.
        if let Some(authorization) = authorization {
            self.authorizer
                .recheck(authorization.peer, &authorization.grant)
                .map_err(CandidateFailure::Authorization)?;
        }
        if let Err(error) = self.confirm_candidate_owner() {
            return self.rollback_candidate(
                CandidateRollback {
                    previous_path_state: previous_path_state.as_ref(),
                    old_frame: old_frame.as_ref(),
                    old_backlight,
                    restore_frame: false,
                    restore_backlight: false,
                    old_synthetic: None,
                },
                error,
            );
        }
        let now = self.now_seconds();
        if let Err(error) = worker.commit(now, self.input_state) {
            return self.rollback_candidate(
                CandidateRollback {
                    previous_path_state: previous_path_state.as_ref(),
                    old_frame: old_frame.as_ref(),
                    old_backlight,
                    restore_frame: false,
                    restore_backlight: false,
                    old_synthetic: None,
                },
                error,
            );
        }
        if let Some(level) = pending_backlight {
            if !preserve_recovery {
                if let Err(error) = self.hardware.set_backlight(level) {
                    let error = self.record_hardware_failure(error, HardwareCapability::Backlight);
                    return self.rollback_candidate(
                        CandidateRollback {
                            previous_path_state: previous_path_state.as_ref(),
                            old_frame: old_frame.as_ref(),
                            old_backlight,
                            restore_frame: false,
                            restore_backlight: brightness_attempted,
                            old_synthetic: None,
                        },
                        error,
                    );
                }
                brightness_changed = true;
            }
        }

        if !preserve_recovery {
            if let Err(error) = self.hardware.present(&frame) {
                let error = self.record_hardware_failure(error, HardwareCapability::Display);
                return self.rollback_candidate(
                    CandidateRollback {
                        previous_path_state: previous_path_state.as_ref(),
                        old_frame: old_frame.as_ref(),
                        old_backlight,
                        restore_frame: true,
                        restore_backlight: brightness_changed,
                        old_synthetic: None,
                    },
                    error,
                );
            }
        }

        // Presentation changes the broker's visible frame. Recheck after it
        // so a session that changes during presentation cannot keep its frame.
        if let Err(error) = self.confirm_candidate_owner() {
            return self.rollback_candidate(
                CandidateRollback {
                    previous_path_state: previous_path_state.as_ref(),
                    old_frame: old_frame.as_ref(),
                    old_backlight,
                    restore_frame: !preserve_recovery,
                    restore_backlight: brightness_changed,
                    old_synthetic: None,
                },
                error,
            );
        }
        if let Err(error) = self.release_synthetic_keys() {
            let error = self.record_hardware_failure(error, HardwareCapability::SyntheticKeys);
            return self.rollback_candidate(
                CandidateRollback {
                    previous_path_state: previous_path_state.as_ref(),
                    old_frame: old_frame.as_ref(),
                    old_backlight,
                    restore_frame: !preserve_recovery,
                    restore_backlight: brightness_changed,
                    old_synthetic: None,
                },
                error,
            );
        }

        if let Some(path_state) = path_state {
            if let Some(authorization) = authorization {
                if let Err(error) = self
                    .authorizer
                    .recheck(authorization.peer, &authorization.grant)
                {
                    return self
                        .rollback_candidate(
                            CandidateRollback {
                                previous_path_state: previous_path_state.as_ref(),
                                old_frame: old_frame.as_ref(),
                                old_backlight,
                                restore_frame: !preserve_recovery,
                                restore_backlight: brightness_changed,
                                old_synthetic: Some(&old_synthetic),
                            },
                            error,
                        )
                        .map_err(|failure| match failure {
                            CandidateFailure::Candidate(error)
                            | CandidateFailure::Authorization(error) => {
                                CandidateFailure::Authorization(error)
                            }
                        });
                }
            }
            if let Err(error) = path_state.commit() {
                return self.rollback_candidate(
                    CandidateRollback {
                        previous_path_state: previous_path_state.as_ref(),
                        old_frame: old_frame.as_ref(),
                        old_backlight,
                        restore_frame: !preserve_recovery,
                        restore_backlight: brightness_changed,
                        old_synthetic: Some(&old_synthetic),
                    },
                    error,
                );
            }
        }

        if !preserve_recovery {
            self.backlight = candidate_backlight;
            self.last_presented_time = Some(now);
            if self.input_state.fn_active {
                if self.fn_hold_started.is_none() {
                    self.fn_hold_started = Some(now);
                }
            } else {
                self.fn_hold_started = None;
            }
            self.recovery = None;
            self.worker_visible = true;
            self.ignored_contacts
                .extend(self.down_contacts.keys().copied());
        }
        let deferred_touches = self.touch_queue.drain();
        let deferred_transitions = std::mem::take(&mut self.input_transitions);
        self.next_worker_deadline = Some(now);
        let replaced = self.active.replace(ActiveConfig {
            worker,
            _source: source,
            frame,
            backlight: candidate_backlight,
            contacts: BTreeMap::new(),
        });
        if preserve_recovery {
            if let Some(effects) = self.drive_active_worker(
                DriveRequest::without_input(now, self.input_state).with_visibility(
                    false,
                    VisibilityReason::Recovery,
                    false,
                ),
            )? {
                self.next_worker_deadline = effects.next_worker_deadline;
                let _ = self.apply_hidden_effects(&effects)?;
            }
        } else if self.recovery_due(now) {
            self.enter_recovery()?;
        }
        if let Some(replaced) = replaced {
            let stop_deadline = Instant::now() + Duration::from_millis(500);
            if self.recovery.is_none()
                && (!deferred_transitions.is_empty() || !deferred_touches.is_empty())
            {
                if let Err(error) = replaced.worker.drive_until(
                    DriveRequest::new(
                        now,
                        self.input_state,
                        deferred_transitions,
                        0.0,
                        deferred_touches,
                    ),
                    stop_deadline,
                ) {
                    eprintln!("replaced Lua worker did not receive deferred input: {error:#}");
                }
            }
            if !replaced.contacts.is_empty() {
                if let Err(error) = cancel_contacts_until(
                    &replaced.worker,
                    &replaced.contacts,
                    now,
                    DriveRequest::without_input(now, self.input_state),
                    stop_deadline,
                ) {
                    eprintln!(
                        "replaced Lua worker did not receive contact cancellation: {error:#}"
                    );
                }
            }
            if let Err(error) = replaced.worker.shutdown_until(stop_reason, stop_deadline) {
                eprintln!("replaced Lua worker did not stop cleanly: {error:#}");
            }
        }
        Ok(())
    }

    fn release_synthetic_keys(&mut self) -> Result<()> {
        let (empty, events) = self.synthetic.release();
        let result = if events.is_empty() || self.hardware.connection_lost() {
            Ok(())
        } else {
            self.hardware.emit_key_events(&events)
        };
        self.synthetic = empty;
        result
    }

    fn restore_synthetic_state(&mut self, state: &SyntheticState) -> Result<()> {
        let events = state.restore_events();
        if !events.is_empty() {
            self.hardware.emit_key_events(&events)?;
        }
        self.synthetic = state.clone();
        Ok(())
    }

    fn rollback_candidate(
        &mut self,
        rollback: CandidateRollback<'_>,
        error: anyhow::Error,
    ) -> std::result::Result<(), CandidateFailure> {
        let CandidateRollback {
            previous_path_state,
            old_frame,
            old_backlight,
            restore_frame,
            restore_backlight,
            old_synthetic,
        } = rollback;
        let mut error = error;
        if restore_frame {
            let restore_result = match old_frame {
                Some(frame) => self.hardware.present(frame),
                None if self.recovery.is_some() => self.present_recovery(),
                None => Ok(()),
            };
            if let Err(restore_error) = restore_result {
                error = error.context(format!(
                    "restoring the previous frame after candidate failure also failed: {restore_error:#}"
                ));
            }
        }
        if restore_backlight {
            if let Err(restore_error) = self.hardware.set_backlight(old_backlight) {
                error = error.context(format!(
                    "restoring the previous backlight after candidate failure also failed: {restore_error:#}"
                ));
            }
        }
        if let Some(previous_path_state) = previous_path_state {
            if let Err(restore_error) = previous_path_state.restore() {
                error = error.context(format!(
                    "restoring selected path after candidate failure also failed: {restore_error:#}"
                ));
            }
        }
        if let Some(old_synthetic) = old_synthetic {
            if let Err(restore_error) = self.restore_synthetic_state(old_synthetic) {
                error = error.context(format!(
                    "restoring synthetic keys after candidate failure also failed: {restore_error:#}"
                ));
            }
        }
        Err(CandidateFailure::Candidate(error))
    }

    fn cancel_active_contacts_for_visibility(&mut self, now: f64) -> Vec<TouchEvent> {
        self.ignored_contacts
            .extend(self.down_contacts.keys().copied());
        self.down_contacts.clear();
        self.touch_queue.drain();
        self.input_transitions.clear();
        self.active
            .as_mut()
            .map(|active| {
                let contacts = std::mem::take(&mut active.contacts);
                contacts
                    .values()
                    .map(|event| TouchEvent {
                        phase: TouchPhase::Cancel,
                        time: now,
                        ..*event
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn hide_active_worker(&mut self, reason: VisibilityReason, now: f64) -> Result<()> {
        let events = self.cancel_active_contacts_for_visibility(now);
        let should_notify = self.worker_visible || reason == VisibilityReason::Suspend;
        self.worker_visible = false;
        let mut request = DriveRequest::new(now, self.input_state, Vec::new(), 0.0, events);
        if should_notify {
            request = request.with_visibility(false, reason, false);
        }
        let Some(effects) = self.drive_active_worker(request)? else {
            self.next_worker_deadline = None;
            return Ok(());
        };
        self.next_worker_deadline = effects.next_worker_deadline;
        let _ = self.apply_hidden_effects(&effects)?;
        Ok(())
    }

    fn release_physical_synthetic_keys(&mut self) -> Result<()> {
        let (_, events) = self.synthetic.release();
        if events.is_empty() {
            return Ok(());
        }
        self.hardware.emit_key_events(&events)
    }

    fn mark_hardware_unavailable(&mut self, capability: HardwareCapability) -> Result<()> {
        self.set_hardware_capability(capability, false, self.now_seconds())
    }

    fn record_hardware_failure(
        &mut self,
        mut error: anyhow::Error,
        fallback: HardwareCapability,
    ) -> anyhow::Error {
        if !self.hardware.is_available() {
            let capability = self.hardware.unavailable_capability().unwrap_or(fallback);
            if let Err(state_error) = self.mark_hardware_unavailable(capability) {
                error = error.context(format!(
                    "recording hardware loss after the operation also failed: {state_error:#}"
                ));
            }
        }
        error
    }

    fn set_hardware_capability(
        &mut self,
        capability: HardwareCapability,
        present: bool,
        now: f64,
    ) -> Result<()> {
        if present {
            self.missing_capabilities.remove(&capability);
            if self.missing_capabilities.is_empty() && !self.hardware_available {
                self.hardware_available = true;
                self.input_state = self.hardware.input_state();
                if let Err(error) = self.recover_visible_worker(VisibilityReason::Device, now) {
                    eprintln!("Touch Bar recovery failed: {error:#}");
                    self.hardware_available = false;
                    self.missing_capabilities.insert(
                        self.hardware
                            .unavailable_capability()
                            .unwrap_or(HardwareCapability::Display),
                    );
                }
            }
        } else {
            let was_available = self.hardware_available;
            self.fn_hold_started = None;
            self.missing_capabilities.insert(capability);
            self.hardware_available = false;
            if was_available {
                self.hide_active_worker(VisibilityReason::Device, now)?;
            }
        }
        Ok(())
    }

    fn recover_visible_worker(&mut self, reason: VisibilityReason, now: f64) -> Result<()> {
        if self.suspended {
            return Ok(());
        }
        if self.recovery.is_some() {
            if self.resume_pending && self.active.is_some() {
                let effects = self.drive_active_worker(
                    DriveRequest::without_input(now, self.input_state).with_timer_resume(),
                )?;
                if let Some(effects) = effects {
                    self.next_worker_deadline = effects.next_worker_deadline;
                    let _ = self.apply_hidden_effects(&effects)?;
                }
                self.resume_pending = false;
            }
            if let Err(error) = self.hardware.set_backlight(0.75) {
                self.hardware_available = false;
                self.missing_capabilities
                    .insert(HardwareCapability::Backlight);
                return Err(error);
            }
            self.backlight = 0.75;
            return self.present_recovery();
        }
        if self.active.is_none() || self.worker_visible {
            return Ok(());
        }
        let desired_backlight = self
            .active
            .as_ref()
            .expect("active worker was checked")
            .backlight;
        if let Err(error) = self.hardware.set_backlight(desired_backlight) {
            self.hardware_available = false;
            self.missing_capabilities
                .insert(HardwareCapability::Backlight);
            return Err(error);
        }
        self.backlight = desired_backlight;
        let active_synthetic = self.synthetic.clone();
        if let Err(error) = self.restore_synthetic_state(&active_synthetic) {
            self.hardware_available = false;
            self.missing_capabilities
                .insert(HardwareCapability::SyntheticKeys);
            return Err(error);
        }
        let reason = if self.resume_pending {
            VisibilityReason::Suspend
        } else {
            reason
        };
        let Some(effects) = self.drive_active_worker(
            DriveRequest::without_input(now, self.input_state).with_visibility(true, reason, true),
        )?
        else {
            return Ok(());
        };
        self.schedule_effects(now, &effects);
        self.apply_effects(effects)?;
        self.worker_visible = self.hardware_available && self.recovery.is_none();
        if self.worker_visible {
            self.resume_pending = false;
        }
        Ok(())
    }

    fn set_visibility(&mut self, visible: bool, now: f64) -> Result<()> {
        if !visible {
            if self.suspended {
                return Ok(());
            }
            self.suspended = true;
            self.resume_pending = self.active.is_some();
            self.fn_hold_started = None;
            if self.hardware_available {
                self.hide_active_worker(VisibilityReason::Suspend, now)?;
                if let Err(error) = self.release_physical_synthetic_keys() {
                    self.hardware_available = false;
                    self.missing_capabilities
                        .insert(HardwareCapability::SyntheticKeys);
                    eprintln!("synthetic key cleanup during suspend failed: {error:#}");
                }
                if let Err(error) = self.hardware.set_backlight(0.0) {
                    self.hardware_available = false;
                    self.missing_capabilities
                        .insert(HardwareCapability::Backlight);
                    eprintln!("backlight off during suspend failed: {error:#}");
                }
            }
            self.next_worker_deadline = None;
            return Ok(());
        }
        if !self.suspended {
            return Ok(());
        }
        self.suspended = false;
        if let Err(error) = self.hardware.reacquire() {
            let capability = self
                .hardware
                .unavailable_capability()
                .unwrap_or(HardwareCapability::Display);
            self.hardware_available = false;
            self.missing_capabilities.insert(capability);
            eprintln!("hardware resume reacquisition failed: {error:#}");
            return Ok(());
        }
        if !self.hardware.is_available() {
            let capability = self
                .hardware
                .unavailable_capability()
                .unwrap_or(HardwareCapability::Display);
            self.hardware_available = false;
            self.missing_capabilities.insert(capability);
            return Ok(());
        }
        self.claimed = true;
        self.hardware_available = true;
        self.missing_capabilities.clear();
        self.input_state = self.hardware.input_state();
        self.fn_hold_started = self.input_state.fn_active.then_some(now);
        if let Err(error) = self.recover_visible_worker(VisibilityReason::Suspend, now) {
            eprintln!("hardware resume failed: {error:#}");
        }
        Ok(())
    }

    fn route_input(&mut self, key: ObservedKey, active: bool, now: f64) -> Result<()> {
        if !self.input_state.apply(key, active) {
            return Ok(());
        }
        if key == ObservedKey::Fn {
            self.fn_hold_started = active.then_some(now);
        }
        let should_queue = if self.recovery.is_some() {
            key == ObservedKey::Fn && !active && self.active.is_some()
        } else {
            self.active.is_some()
        };
        if should_queue {
            if self.input_transitions.len() >= TOUCH_QUEUE_CAPACITY {
                self.fail_active_worker(anyhow::anyhow!(
                    "non-droppable input transition queue overflow"
                ))?;
                return Ok(());
            }
            self.input_transitions.push(InputTransition {
                key,
                active,
                state: self.input_state,
            });
        }
        Ok(())
    }

    fn route_touch(&mut self, event: TouchEvent) -> Result<()> {
        let (recovery_dispatch, active_dispatch): (
            RecoveryTouchDispatch,
            ActiveTouchDispatch<H, L>,
        ) = match event.phase {
            TouchPhase::Down => {
                if self.down_contacts.insert(event.id, event).is_some()
                    || self.ignored_contacts.contains(&event.id)
                {
                    return Ok(());
                }
                (RecoverySession::touch_down, Self::route_active_down)
            }
            TouchPhase::Move => {
                let Some(contact) = self.down_contacts.get_mut(&event.id) else {
                    return Ok(());
                };
                *contact = event;
                if self.ignored_contacts.contains(&event.id) {
                    return Ok(());
                }
                (RecoverySession::touch_move, Self::route_active_move)
            }
            TouchPhase::Up => {
                self.down_contacts.remove(&event.id);
                if self.ignored_contacts.remove(&event.id) {
                    return Ok(());
                }
                (RecoverySession::touch_up, Self::route_active_end)
            }
            TouchPhase::Cancel => {
                self.down_contacts.remove(&event.id);
                if self.ignored_contacts.remove(&event.id) {
                    return Ok(());
                }
                (RecoverySession::touch_cancel, Self::route_active_end)
            }
        };
        if self.recovery.is_some() {
            self.route_recovery_phase(event, recovery_dispatch)
        } else {
            active_dispatch(self, event)
        }
    }

    fn route_recovery_phase(
        &mut self,
        event: TouchEvent,
        dispatch: fn(&mut RecoverySession, TouchEvent) -> RecoveryTouchResult,
    ) -> Result<()> {
        let result = {
            let recovery = self
                .recovery
                .as_mut()
                .expect("recovery session disappeared");
            dispatch(recovery, event)
        };
        self.apply_recovery_touch(result)
    }

    fn apply_recovery_touch(&mut self, result: RecoveryTouchResult) -> Result<()> {
        match result {
            RecoveryTouchResult::Ignored => Ok(()),
            RecoveryTouchResult::RowPressChanged => self.present_recovery(),
            RecoveryTouchResult::Activate(key) => {
                self.present_recovery()?;
                self.activate_recovery_key(key)
            }
        }
    }

    fn route_active_down(&mut self, event: TouchEvent) -> Result<()> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        active.contacts.insert(event.id, event);
        if let Err(error) = self.touch_queue.push(event) {
            active.contacts.remove(&event.id);
            self.fail_active_worker(error)?;
        }
        Ok(())
    }

    fn route_active_move(&mut self, event: TouchEvent) -> Result<()> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        if let std::collections::btree_map::Entry::Occupied(mut contact) =
            active.contacts.entry(event.id)
        {
            contact.insert(event);
            self.touch_queue.push(event)?;
        }
        Ok(())
    }

    fn route_active_end(&mut self, event: TouchEvent) -> Result<()> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        if active.contacts.remove(&event.id).is_some() {
            if let Err(error) = self.touch_queue.push(event) {
                self.fail_active_worker(error)?;
            }
        }
        Ok(())
    }

    fn activate_recovery_key(&mut self, key: OutputKey) -> Result<()> {
        let events = tap_key_events(key, &modifier_output_keys(self.input_state.modifiers));
        self.hardware
            .emit_key_events(&events)
            .context("emitting recovery function key")
    }

    fn present_recovery(&mut self) -> Result<()> {
        let frame = self
            .recovery
            .as_ref()
            .context("recovery session disappeared while rendering")?
            .render()?;
        self.hardware.present(&frame)
    }

    fn enter_recovery(&mut self) -> Result<()> {
        if self.recovery.is_some() {
            return Ok(());
        }
        let now = self.now_seconds();
        let mut owner_is_healthy = false;
        let hide_result = self.active.as_mut().map(|active| {
            owner_is_healthy = true;
            let hide_effects = cancel_contacts(
                &active.worker,
                &active.contacts,
                now,
                DriveRequest::without_input(now, self.input_state).with_visibility(
                    false,
                    VisibilityReason::Recovery,
                    false,
                ),
            );
            active.contacts.clear();
            hide_effects
        });
        let next_worker_deadline = match hide_result {
            Some(Ok(effects)) => {
                if !self.apply_hidden_effects(&effects)? {
                    return Ok(());
                }
                effects.next_worker_deadline
            }
            Some(Err(error)) => {
                self.fail_active_worker(error)?;
                return Ok(());
            }
            None => None,
        };
        self.ignored_contacts
            .extend(self.down_contacts.keys().copied());
        self.touch_queue.drain();
        self.input_transitions.clear();
        self.next_worker_deadline = next_worker_deadline;
        self.fn_hold_started = None;
        self.worker_visible = false;
        self.recovery = Some(RecoverySession::new(owner_is_healthy));
        if !self.hardware_available || self.suspended {
            return Ok(());
        }
        self.show_recovery_frame()
    }

    fn show_recovery_frame(&mut self) -> Result<()> {
        if self.suspended {
            return Ok(());
        }
        self.hardware
            .set_backlight(0.75)
            .context("setting recovery backlight")?;
        self.backlight = 0.75;
        self.present_recovery()
    }

    /// Replace any retained user frame with the broker-owned recovery row.
    /// This is also needed when recovery already exists, because the panel
    /// may still contain a frame from the revoked owner.
    pub(crate) fn fence_owner_output(&mut self) -> Result<()> {
        let was_missing = self.recovery.is_none();
        if was_missing {
            self.enter_recovery()?;
            if self.suspended {
                return Ok(());
            }
            if self.hardware_available {
                return Ok(());
            }
        }
        self.show_recovery_frame()
    }

    fn exit_recovery(&mut self, now: f64) -> Result<()> {
        let Some(recovery) = self.recovery.take() else {
            return Ok(());
        };
        if !recovery.owner_is_healthy() || self.active.is_none() {
            self.recovery = Some(recovery);
            return Ok(());
        }
        self.ignored_contacts
            .extend(self.down_contacts.keys().copied());
        let transitions = std::mem::take(&mut self.input_transitions);
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        self.hardware
            .set_backlight(active.backlight)
            .context("restoring worker backlight after recovery")?;
        self.backlight = active.backlight;
        let delta = self
            .last_presented_time
            .map(|previous| (now - previous).max(0.0))
            .unwrap_or(0.0);
        let Some(effects) = self.drive_active_worker(
            DriveRequest::new(now, self.input_state, transitions, delta, Vec::new())
                .with_visibility(true, VisibilityReason::Recovery, true),
        )?
        else {
            return Ok(());
        };
        self.schedule_effects(now, &effects);
        self.apply_effects(effects)?;
        self.worker_visible = true;
        Ok(())
    }

    pub(crate) fn poll_events(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        if !self.claimed && !self.suspended {
            let now = self.now_seconds();
            match self.hardware.reacquire() {
                Ok(()) => {
                    self.claimed = true;
                    self.hardware_available = true;
                    self.missing_capabilities.clear();
                    self.input_state = self.hardware.input_state();
                    match self.hardware.get_backlight() {
                        Ok(level) => self.backlight = level,
                        Err(error) if !self.hardware.is_available() => {
                            self.set_hardware_capability(
                                HardwareCapability::Backlight,
                                false,
                                now,
                            )?;
                            eprintln!("Touch Bar backlight reacquisition failed: {error:#}");
                            return Ok(Vec::new());
                        }
                        Err(error) => return Err(error),
                    }
                    self.recover_visible_worker(VisibilityReason::Device, now)?;
                }
                Err(error) if !self.hardware.is_available() => {
                    eprintln!("Touch Bar reacquisition failed: {error:#}");
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            }
        }
        let events = match self.hardware.poll(timeout) {
            Ok(events) => events,
            Err(error) if !self.hardware.is_available() => {
                self.mark_hardware_unavailable(
                    self.hardware
                        .unavailable_capability()
                        .unwrap_or(HardwareCapability::Display),
                )?;
                eprintln!("Touch Bar hardware poll failed: {error:#}");
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        let now = self.now_seconds();
        self.process_events_at(now, events.clone())?;
        Ok(events)
    }

    fn poll_hardware_deferred(&mut self, timeout: Duration) -> Result<()> {
        let now = self.now_seconds();
        let events = match self.hardware.poll(timeout) {
            Ok(events) => events,
            Err(error) if !self.hardware.is_available() => {
                self.mark_hardware_unavailable(
                    self.hardware
                        .unavailable_capability()
                        .unwrap_or(HardwareCapability::Display),
                )?;
                eprintln!("Touch Bar hardware poll failed during staging: {error:#}");
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        for event in events {
            self.route_hardware_event(event, now)?;
        }
        Ok(())
    }

    fn recovery_deadline(&self) -> Option<f64> {
        if self.recovery.is_some() || self.active.is_none() {
            return None;
        }
        self.fn_hold_started
            .map(|started| started + PHYSICAL_FN_RECOVERY_HOLD_SECONDS)
    }

    fn recovery_due(&self, now: f64) -> bool {
        self.recovery_deadline()
            .is_some_and(|deadline| deadline <= now)
    }

    fn has_healthy_recovery(&self) -> bool {
        self.recovery
            .as_ref()
            .is_some_and(|recovery| recovery.owner_is_healthy())
    }

    fn route_hardware_event(&mut self, event: HardwareEvent, now: f64) -> Result<()> {
        if (self.suspended || !self.hardware_available)
            && matches!(
                event,
                HardwareEvent::Touch(_) | HardwareEvent::Fn { .. } | HardwareEvent::Modifier { .. }
            )
        {
            return Ok(());
        }
        match event {
            HardwareEvent::Touch(touch) => self.route_touch(touch),
            HardwareEvent::Fn { active } => {
                self.route_input(ObservedKey::Fn, active, now)?;
                if !active && self.has_healthy_recovery() {
                    self.exit_recovery(now)?;
                }
                Ok(())
            }
            HardwareEvent::Modifier { modifier, active } => {
                self.route_input(ObservedKey::Modifier(modifier), active, now)
            }
            HardwareEvent::Device { present } => {
                for capability in HardwareCapability::ALL {
                    self.set_hardware_capability(capability, present, now)?;
                }
                Ok(())
            }
            HardwareEvent::Capability {
                capability,
                present,
            } => self.set_hardware_capability(capability, present, now),
            HardwareEvent::Visibility { visible } => self.set_visibility(visible, now),
        }
    }

    fn check_worker_liveness(&mut self) -> Result<()> {
        let failure = self.active.as_ref().and_then(|active| {
            (!active.worker.is_alive()).then(|| {
                active
                    .worker
                    .failure_reason()
                    .unwrap_or_else(|| "Lua worker process exited".into())
            })
        });
        if let Some(failure) = failure {
            self.fail_active_worker(anyhow::anyhow!(failure))?;
            #[cfg(test)]
            {
                self.worker_failure = None;
            }
        }
        Ok(())
    }

    fn process_events_at(&mut self, now: f64, events: Vec<HardwareEvent>) -> Result<()> {
        self.check_worker_liveness()?;
        if self.recovery_due(now) {
            self.enter_recovery()?;
        }
        for event in events {
            self.route_hardware_event(event, now)?;
        }
        if self.recovery.is_some() && !self.input_state.fn_active {
            self.exit_recovery(now)?;
        }
        let worker_due = self
            .next_worker_deadline
            .is_some_and(|deadline| deadline <= now);
        if !self.suspended && self.active.is_some() && !self.worker_visible && worker_due {
            self.drive_hidden(now)?;
        } else if !self.suspended
            && self.recovery.is_none()
            && self.active.is_some()
            && (!self.input_transitions.is_empty()
                || !self.touch_queue.events.is_empty()
                || worker_due)
        {
            let transitions = std::mem::take(&mut self.input_transitions);
            let touches = self.touch_queue.drain();
            self.drive_active(now, transitions, touches)?;
        }
        Ok(())
    }

    fn drive_active_worker(&mut self, request: DriveRequest) -> Result<Option<WorkerEffects>> {
        let Some(result) = self
            .active
            .as_ref()
            .map(|active| active.worker.drive(request))
        else {
            return Ok(None);
        };
        match result {
            Ok(effects) => Ok(Some(effects)),
            Err(error) => {
                self.fail_active_worker(error)?;
                Ok(None)
            }
        }
    }

    fn drive_hidden(&mut self, now: f64) -> Result<()> {
        if self.suspended || self.active.is_none() || self.worker_visible {
            return Ok(());
        }
        if self.recovery.is_some() && !self.has_healthy_recovery() {
            return Ok(());
        }
        let Some(effects) =
            self.drive_active_worker(DriveRequest::without_input(now, self.input_state))?
        else {
            return Ok(());
        };
        self.next_worker_deadline = effects.next_worker_deadline;
        if !self.apply_hidden_effects(&effects)? {
            return Ok(());
        }
        Ok(())
    }

    fn drive_active(
        &mut self,
        now: f64,
        transitions: Vec<InputTransition>,
        touches: Vec<TouchEvent>,
    ) -> Result<()> {
        let delta = self
            .last_presented_time
            .map(|previous| (now - previous).max(0.0))
            .unwrap_or(0.0);
        let Some(effects) = self.drive_active_worker(DriveRequest::new(
            now,
            self.input_state,
            transitions,
            delta,
            touches,
        ))?
        else {
            return Ok(());
        };
        self.schedule_effects(now, &effects);
        self.apply_effects(effects)
    }

    fn schedule_effects(&mut self, now: f64, effects: &WorkerEffects) {
        self.next_worker_deadline = if effects.redraw_pending {
            Some(now)
        } else {
            effects
                .next_worker_deadline
                .or_else(|| effects.frame.as_ref().map(|_| now))
        };
    }

    fn fail_active_worker(&mut self, error: anyhow::Error) -> Result<()> {
        eprintln!("Lua worker entered recovery: {error:#}");
        #[cfg(test)]
        {
            self.worker_failure = Some(format!("{error:#}"));
        }
        let now = self.now_seconds();
        if let Some(active) = self.active.take() {
            if !active.contacts.is_empty() && !active.worker.uses_process_backend() {
                if let Err(cancel_error) = cancel_contacts(
                    &active.worker,
                    &active.contacts,
                    now,
                    DriveRequest::without_input(now, self.input_state),
                ) {
                    eprintln!(
                        "failed Lua worker did not receive contact cancellation: {cancel_error:#}"
                    );
                }
            }
        }
        if let Err(cleanup_error) = self.release_synthetic_keys() {
            crate::system_log::supervisor_error(format!(
                "releasing synthetic keys after worker failure failed: {cleanup_error:#}"
            ));
        }
        self.next_worker_deadline = None;
        if let Some(recovery) = self.recovery.as_mut() {
            recovery.mark_unhealthy();
            Ok(())
        } else {
            self.enter_recovery()
        }
    }

    fn apply_key_effects(&mut self, requests: &[KeyRequest]) -> Result<bool> {
        let (next_synthetic, key_events) = match self.synthetic.plan(requests, self.input_state) {
            Ok(effects) => effects,
            Err(error) => {
                self.fail_active_worker(error)?;
                return Ok(false);
            }
        };
        if !key_events.is_empty() && self.hardware_available && !self.suspended {
            if let Err(error) = self.hardware.emit_key_events(&key_events) {
                if !self.hardware.is_available() {
                    self.synthetic = next_synthetic;
                    self.mark_hardware_unavailable(
                        self.hardware
                            .unavailable_capability()
                            .unwrap_or(HardwareCapability::SyntheticKeys),
                    )?;
                    return Ok(false);
                }
                return Err(error);
            }
        }
        self.synthetic = next_synthetic;
        Ok(true)
    }

    fn apply_hidden_effects(&mut self, effects: &WorkerEffects) -> Result<bool> {
        if !self.apply_key_effects(&effects.key_requests)? {
            return Ok(false);
        }
        if let Some(level) = effects.backlight {
            self.active
                .as_mut()
                .expect("active worker disappeared")
                .backlight = level;
        }
        Ok(true)
    }

    fn apply_effects(&mut self, effects: WorkerEffects) -> Result<()> {
        if self.active.is_none() {
            return Ok(());
        }
        if !self.apply_key_effects(&effects.key_requests)? {
            return Ok(());
        }
        let active = self.active.as_ref().expect("active worker disappeared");
        let old_frame = active.frame.clone();
        let old_backlight = active.backlight;
        let frame = effects.frame;
        let backlight = effects.backlight;
        let frame_time = frame.as_ref().map(|frame| frame.timing.presentation_time);
        let mut brightness_changed = false;

        if let Some(level) = backlight {
            if let Err(error) = self.hardware.set_backlight(level) {
                if !self.hardware.is_available() {
                    self.mark_hardware_unavailable(
                        self.hardware
                            .unavailable_capability()
                            .unwrap_or(HardwareCapability::Backlight),
                    )?;
                    return Ok(());
                }
                let mut error = error.context("applying Lua backlight request");
                if let Err(restore_error) = self.hardware.set_backlight(old_backlight) {
                    error = error.context(format!(
                        "restoring the previous backlight also failed: {restore_error:#}"
                    ));
                }
                if let Err(restore_error) = active.worker.restore_backlight(old_backlight) {
                    error = error.context(format!(
                        "restoring Lua backlight state also failed: {restore_error:#}"
                    ));
                }
                return Err(error);
            }
            brightness_changed = true;
        }
        if let Some(frame) = frame.as_ref() {
            if let Err(error) = self.hardware.present(&frame.frame) {
                if !self.hardware.is_available() {
                    self.mark_hardware_unavailable(
                        self.hardware
                            .unavailable_capability()
                            .unwrap_or(HardwareCapability::Display),
                    )?;
                    return Ok(());
                }
                let mut error = error.context("presenting Lua frame");
                if let Err(restore_error) = self.hardware.present(&old_frame) {
                    error = error.context(format!(
                        "restoring the previous frame also failed: {restore_error:#}"
                    ));
                }
                if brightness_changed {
                    if let Err(restore_error) = self.hardware.set_backlight(old_backlight) {
                        error = error.context(format!(
                            "restoring the previous backlight also failed: {restore_error:#}"
                        ));
                    }
                    if let Err(restore_error) = active.worker.restore_backlight(old_backlight) {
                        error = error.context(format!(
                            "restoring Lua backlight state also failed: {restore_error:#}"
                        ));
                    }
                }
                return Err(error);
            }
        }

        let active = self.active.as_mut().expect("active worker disappeared");
        if let Some(frame) = frame {
            active.frame = frame.frame;
            self.last_presented_time = frame_time;
        }
        if let Some(level) = backlight {
            active.backlight = level;
            self.backlight = level;
        }
        Ok(())
    }

    fn poll_wait(&self, now: f64) -> Duration {
        let deadline = earliest_deadline(self.next_worker_deadline, self.recovery_deadline());
        let Some(deadline) = deadline else {
            return MAX_POLL_WAIT;
        };
        if deadline <= now {
            return Duration::ZERO;
        }
        Duration::from_secs_f64((deadline - now).min(MAX_POLL_WAIT.as_secs_f64()))
    }

    fn stop_active_worker(&mut self, reason: StopReason) -> Result<()> {
        let now = self.now_seconds();
        let stop_deadline = Instant::now() + Duration::from_millis(500);
        self.input_transitions.clear();
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        if !active.contacts.is_empty() {
            if let Err(error) = cancel_contacts_until(
                &active.worker,
                &active.contacts,
                now,
                DriveRequest::without_input(now, self.input_state),
                stop_deadline,
            ) {
                eprintln!("active Lua worker did not receive contact cancellation: {error:#}");
            }
        }
        active.worker.shutdown_until(reason, stop_deadline)
    }

    #[allow(dead_code)]
    fn reset_owner_state(&mut self, input_state: InputState) {
        self.down_contacts.clear();
        self.ignored_contacts.clear();
        self.touch_queue.drain();
        self.input_transitions.clear();
        self.next_worker_deadline = None;
        self.input_state = input_state;
    }

    #[allow(dead_code)]
    pub(crate) fn handoff_owner(&mut self) -> Result<()> {
        self.handoff_owner_with_reason(StopReason::Logout)
    }

    pub(crate) fn handoff_owner_with_reason(&mut self, reason: StopReason) -> Result<()> {
        if let Err(error) = self.poll_events(Duration::ZERO) {
            eprintln!("hardware poll failed during owner handoff: {error:#}");
        }
        if let Err(error) = self.release_synthetic_keys() {
            eprintln!("synthetic key cleanup failed during owner handoff: {error:#}");
        }
        let stop_result = self.stop_active_worker(reason);
        let input_state = self.hardware.input_state();
        self.reset_owner_state(input_state);
        self.recovery = None;
        self.worker_visible = false;
        self.resume_pending = false;
        if let Err(error) = stop_result {
            eprintln!("Lua worker logout cleanup failed during owner handoff: {error:#}");
        }
        Ok(())
    }

    #[cfg(test)]
    fn step_at(&mut self, now: f64) -> Result<()> {
        let events = self.hardware.poll(Duration::ZERO)?;
        self.process_events_at(now, events)?;
        #[cfg(test)]
        if let Some(error) = self.worker_failure.take() {
            return Err(anyhow::anyhow!(error));
        }
        Ok(())
    }

    pub(crate) fn shutdown(self) -> Result<()> {
        self.shutdown_with_reason(StopReason::Shutdown, true)
    }

    pub(crate) fn shutdown_for_logout(self) -> Result<()> {
        self.shutdown_with_reason(StopReason::Logout, true)
    }

    /// The system broker lets the concrete adapter blank during release. This
    /// keeps the adapter's final DRM operation together with ownership drop.
    pub(crate) fn shutdown_for_broker(self) -> Result<()> {
        self.shutdown_with_reason(StopReason::Shutdown, false)
    }

    fn shutdown_with_reason(mut self, reason: StopReason, paint_black: bool) -> Result<()> {
        let synthetic_result = if self.hardware_available {
            self.release_synthetic_keys()
        } else {
            Ok(())
        };
        let stop_result = self.stop_active_worker(reason);
        let blank_result = if paint_black
            && self.hardware_available
            && self.hardware.is_available()
            && !self.hardware.session_revoked()
        {
            FrameCanvas::new().and_then(|canvas| {
                canvas
                    .finish()
                    .and_then(|frame| self.hardware.present(&frame))
            })
        } else {
            Ok(())
        };
        let backlight_result = if paint_black
            && self.hardware_available
            && self.hardware.is_available()
            && !self.hardware.session_revoked()
        {
            self.hardware.set_backlight(0.0)
        } else {
            Ok(())
        };
        let release_result = self.hardware.release();
        self.claimed = false;

        let mut hardware_error = None;
        for (operation, result) in [
            ("painting the shutdown frame", blank_result),
            ("turning off the Touch Bar backlight", backlight_result),
            ("releasing supervisor hardware", release_result),
        ] {
            if let Err(error) = result {
                if hardware_error.is_none() {
                    hardware_error = Some(error.context(operation));
                }
            }
        }
        if self.hardware.connection_lost() {
            // The broker owns the device. If it disappeared first, its
            // release path has already run or systemd is killing it now.
            // A stale supervisor must not turn that expected disconnect into
            // a restart loop.
            hardware_error = None;
        }
        let worker_result = stop_result.and(synthetic_result);
        match (worker_result, hardware_error) {
            (Err(error), Some(hardware_error)) => {
                crate::system_log::supervisor_error(format!(
                    "hardware shutdown failed after Lua stop error: {hardware_error:#}"
                ));
                Err(error)
            }
            (Err(error), None) => Err(error),
            (Ok(()), Some(error)) => Err(error),
            (Ok(()), None) => Ok(()),
        }
    }

    pub(crate) fn hardware(&self) -> &H {
        &self.hardware
    }

    pub(crate) fn hardware_mut(&mut self) -> &mut H {
        &mut self.hardware
    }
}

fn state_file_for_uid(base: &Path, uid: libc::uid_t) -> PathBuf {
    if uid == unsafe { libc::getuid() } {
        return base.to_path_buf();
    }
    let filename = base
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("config-path"));
    base.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("users")
        .join(uid.to_string())
        .join(filename)
}

fn read_selected_path(state_file: &Path) -> Result<Option<PathBuf>> {
    let contents = match std::fs::read(state_file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading selected-path state {}", state_file.display()))
        }
    };
    ensure!(!contents.is_empty(), "selected-path state is empty");
    let path = PathBuf::from(std::ffi::OsString::from_vec(contents));
    ensure!(path.is_absolute(), "selected-path state is not absolute");
    Ok(Some(path))
}

struct PendingRequest {
    stream: UnixStream,
    peer: PeerCredentials,
    header: [u8; 4],
    header_len: usize,
    length: Option<usize>,
    payload: Vec<u8>,
    payload_len: usize,
}

impl PendingRequest {
    fn new(stream: UnixStream) -> Result<Self> {
        let peer = crate::peer_credentials::read(&stream)?;
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            peer,
            header: [0; 4],
            header_len: 0,
            length: None,
            payload: Vec::new(),
            payload_len: 0,
        })
    }

    fn try_request(&mut self) -> Result<Option<ConfigSelection>> {
        while self.header_len < self.header.len() {
            match self.stream.read(&mut self.header[self.header_len..]) {
                Ok(0) => bail!("apply request ended before its length header"),
                Ok(read) => self.header_len += read,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(error).context("reading apply request length"),
            }
        }
        if self.length.is_none() {
            let length = u32::from_be_bytes(self.header) as usize;
            ensure!(length <= MAX_REQUEST_BYTES, "IPC message is too large");
            ensure!(length > 0, "config path is empty");
            self.payload.resize(length, 0);
            self.length = Some(length);
        }
        let length = self.length.expect("request length was initialized");
        while self.payload_len < length {
            match self.stream.read(&mut self.payload[self.payload_len..]) {
                Ok(0) => bail!("apply request ended before its path payload"),
                Ok(read) => self.payload_len += read,
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(error).context("reading apply request path"),
            }
        }
        let payload = std::mem::take(&mut self.payload);
        Ok(Some(crate::apply_ipc::decode_request(&payload)?))
    }
}

struct QueuedRequest {
    stream: UnixStream,
    request: Result<AuthorizedRequest>,
}

pub(crate) fn serve_until<H: TouchBarHardware, L: Logind>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H, L>,
    running: Arc<AtomicBool>,
) -> Result<()> {
    serve_queue(listener, supervisor, None, Some(running))
}

#[cfg(test)]
fn serve_for_test<H: TouchBarHardware, L: Logind>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H, L>,
    request_limit: usize,
) -> Result<()> {
    serve_queue(listener, supervisor, Some(request_limit), None)
}

fn serve_queue<H: TouchBarHardware, L: Logind>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H, L>,
    request_limit: Option<usize>,
    external_stop: Option<Arc<AtomicBool>>,
) -> Result<()> {
    let authorizer = supervisor.authorizer.clone();
    let (sender, receiver) = mpsc::sync_channel(REQUEST_QUEUE_CAPACITY);
    let running = external_stop.unwrap_or_else(|| Arc::new(AtomicBool::new(true)));
    let acceptor_stop = Arc::new(AtomicBool::new(false));
    let acceptor_stop_signal = acceptor_stop.clone();
    let acceptor = thread::spawn(move || {
        accept_requests(listener, authorizer, sender, request_limit, acceptor_stop)
    });

    let mut service_result = Ok(());
    let mut processed = 0;
    loop {
        if !running.load(Ordering::Acquire) {
            break;
        }
        match receiver.try_recv() {
            Ok(queued) => {
                if let Err(error) = serve_queued_request(queued, supervisor) {
                    service_result = Err(error);
                    break;
                }
                processed += 1;
                if request_limit.is_some_and(|limit| processed >= limit) {
                    break;
                }
                if let Err(error) = supervisor.poll_events(Duration::ZERO) {
                    service_result = Err(error);
                    break;
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                let now = supervisor.now_seconds();
                if let Err(error) = supervisor.poll_events(supervisor.poll_wait(now)) {
                    service_result = Err(error);
                    break;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }

    acceptor_stop_signal.store(true, Ordering::Release);
    let acceptor_result = acceptor
        .join()
        .map_err(|_| anyhow::anyhow!("apply acceptor thread panicked"))?;
    match (service_result, acceptor_result) {
        (Err(error), Err(acceptor_error)) => {
            Err(error).context(format!("apply acceptor failed also: {acceptor_error:#}"))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn accept_requests<L: Logind>(
    listener: UnixListener,
    authorizer: SessionAuthorizer<L>,
    sender: SyncSender<QueuedRequest>,
    request_limit: Option<usize>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    let mut pending: VecDeque<PendingRequest> = VecDeque::new();
    let mut ready = None;
    let mut sent = 0;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }

        if ready.is_none() {
            if let Some(request) = pending.front_mut() {
                match request.try_request() {
                    Ok(Some(request_value)) => {
                        let request = pending.pop_front().expect("request was present");
                        ready = Some(QueuedRequest {
                            stream: request.stream,
                            request: authorize_request(&authorizer, request.peer, request_value),
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let request = pending.pop_front().expect("request was present");
                        ready = Some(QueuedRequest {
                            stream: request.stream,
                            request: Err(error),
                        });
                    }
                }
            }
        }

        if let Some(request) = ready.take() {
            match sender.try_send(request) {
                Ok(()) => {
                    sent += 1;
                    if request_limit.is_some_and(|limit| sent >= limit) {
                        return Ok(());
                    }
                }
                Err(TrySendError::Full(request)) => ready = Some(request),
                Err(TrySendError::Disconnected(_)) => return Ok(()),
            }
        }

        if ready.is_none() && pending.len() < REQUEST_QUEUE_CAPACITY {
            match listener.accept() {
                Ok((stream, _)) => pending.push_back(PendingRequest::new(stream)?),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => return Err(error).context("accepting apply request"),
            }
        }
        thread::park_timeout(Duration::from_millis(1));
    }
}

fn authorize_request<L: Logind>(
    authorizer: &SessionAuthorizer<L>,
    peer: PeerCredentials,
    request: ConfigSelection,
) -> Result<AuthorizedRequest> {
    let grant = authorizer.authorize(peer)?;
    Ok(AuthorizedRequest {
        request,
        authorization: ApplyAuthorization { peer, grant },
    })
}

#[cfg(test)]
fn read_authorized_request<L: Logind>(
    stream: &mut UnixStream,
    authorizer: &SessionAuthorizer<L>,
) -> Result<AuthorizedRequest> {
    let peer = crate::peer_credentials::read(stream)?;
    let request = crate::apply_ipc::read_request(stream)?;
    authorize_request(authorizer, peer, request)
}

fn serve_queued_request<H: TouchBarHardware, L: Logind>(
    mut queued: QueuedRequest,
    supervisor: &mut Supervisor<H, L>,
) -> Result<()> {
    let result = queued
        .request
        .and_then(|request| supervisor.apply_authorized(request));
    if let Err(error) = &result {
        crate::system_log::supervisor_error(format!("apply request failed: {error:#}"));
    }
    crate::apply_ipc::write_reply(&mut queued.stream, &result).context("sending apply reply")
}

#[cfg(test)]
fn serve_connection<H: TouchBarHardware, L: Logind>(
    stream: &mut UnixStream,
    supervisor: &mut Supervisor<H, L>,
) -> Result<()> {
    let result = read_authorized_request(stream, &supervisor.authorizer)
        .and_then(|request| supervisor.apply_authorized(request));
    crate::apply_ipc::write_reply(stream, &result).context("sending apply reply")
}

impl<H: TouchBarHardware, L: Logind> Drop for Supervisor<H, L> {
    fn drop(&mut self) {
        if self.claimed {
            if let Err(error) = self.release_synthetic_keys() {
                crate::system_log::supervisor_error(format!(
                    "synthetic key cleanup failed during supervisor drop: {error:#}"
                ));
            }
            self.active.take();
            let _ = self.hardware.release();
            self.claimed = false;
        } else {
            self.active.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::rc::Rc;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};

    use crate::default_source;
    use crate::hardware::{
        ConsumerKey, FakeAction, FakeKey, FakeKeyEvent, FakeTouchBar, HardwareCapability,
        HardwareEvent, InputState, KeyboardKey, LogicalFrame, Modifier, ModifierState, OutputKey,
        SyntheticKeyEvent, TouchBarHardware, TouchEvent, TouchPhase,
    };
    use crate::logind::{ActiveSession, FakeLogind, Session};

    use super::{serve_connection, serve_for_test, LuaSource, PreparedPathState, Supervisor};

    fn active_local_logind(session_id: &str) -> (FakeLogind, libc::uid_t) {
        let uid = unsafe { libc::getuid() };
        let pid = std::process::id() as libc::pid_t;
        let logind = FakeLogind::new();
        logind.set_session(
            pid,
            Some(Session {
                id: session_id.into(),
                uid,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: session_id.into(),
                uid,
            }),
        );
        (logind, uid)
    }

    fn frame_contains_rgb(
        frame: &crate::hardware::FrameSnapshot,
        x_range: std::ops::Range<usize>,
        y_range: std::ops::Range<usize>,
        rgb: [u8; 3],
    ) -> bool {
        x_range.into_iter().any(|x| {
            y_range.clone().any(|y| {
                let pixel = frame.rgba_at(x, y);
                pixel[..3]
                    .iter()
                    .zip(rgb)
                    .all(|(actual, expected)| actual.abs_diff(expected) <= 32)
            })
        })
    }

    fn default_bytes_with_battery_root(root: &std::path::Path) -> Vec<u8> {
        String::from_utf8(default_source::bytes().to_vec())
            .expect("canonical default was not UTF-8")
            .replace(
                "/sys/class/power_supply/macsmc-battery",
                &root.to_string_lossy(),
            )
            .into_bytes()
    }

    fn embedded_default_supervisor(
        state_file: std::path::PathBuf,
        session_id: &str,
    ) -> Result<Supervisor<FakeTouchBar, FakeLogind>> {
        let (logind, _) = active_local_logind(session_id);
        Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file,
            logind,
            Some(LuaSource::embedded(default_source::bytes().to_vec())),
        )
    }

    fn battery_cell() -> std::ops::Range<usize> {
        let width = 2008.0 / 11.0;
        let left = (7.0 * width) as usize;
        left..((8.0 * width) as usize)
    }

    fn region_changed(
        before: &crate::hardware::FrameSnapshot,
        after: &crate::hardware::FrameSnapshot,
        x_range: std::ops::Range<usize>,
        y_range: std::ops::Range<usize>,
    ) -> bool {
        x_range.into_iter().any(|x| {
            y_range
                .clone()
                .any(|y| before.rgba_at(x, y) != after.rgba_at(x, y))
        })
    }

    struct FailingPresentHardware {
        inner: FakeTouchBar,
        state_file: std::path::PathBuf,
        fail_next_present: bool,
        fail_present_as_unavailable: bool,
        fail_next_backlight: bool,
        fail_next_key: bool,
        fail_reacquire: bool,
        available: bool,
        state_seen_at_failure: Vec<u8>,
        state_seen_at_key_failure: Vec<u8>,
    }

    impl FailingPresentHardware {
        fn new(state_file: std::path::PathBuf) -> Self {
            Self {
                inner: FakeTouchBar::new(),
                state_file,
                fail_next_present: false,
                fail_present_as_unavailable: false,
                fail_next_backlight: false,
                fail_next_key: false,
                fail_reacquire: false,
                available: true,
                state_seen_at_failure: Vec::new(),
                state_seen_at_key_failure: Vec::new(),
            }
        }
    }

    impl TouchBarHardware for FailingPresentHardware {
        fn claim(&mut self) -> Result<()> {
            let result = self.inner.claim();
            if result.is_ok() {
                self.available = true;
            }
            result
        }

        fn reacquire(&mut self) -> Result<()> {
            if self.fail_reacquire {
                self.available = false;
                bail!("injected hardware reacquisition failure");
            }
            self.inner.reacquire()
        }

        fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
            let result = self.inner.poll(timeout);
            if !self.inner.is_available() {
                self.available = false;
            }
            result
        }

        fn is_available(&self) -> bool {
            self.available && self.inner.is_available()
        }

        fn unavailable_capability(&self) -> Option<HardwareCapability> {
            if !self.available {
                Some(HardwareCapability::Display)
            } else {
                self.inner.unavailable_capability()
            }
        }

        fn input_state(&self) -> InputState {
            self.inner.input_state()
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            if self.fail_next_present {
                self.fail_next_present = false;
                self.available = !self.fail_present_as_unavailable;
                self.state_seen_at_failure = std::fs::read(&self.state_file)?;
                bail!("injected presentation failure");
            }
            self.inner.present(frame)
        }

        fn emit_key_events(&mut self, events: &[crate::hardware::SyntheticKeyEvent]) -> Result<()> {
            if self.fail_next_key {
                self.fail_next_key = false;
                self.state_seen_at_key_failure = std::fs::read(&self.state_file)?;
                bail!("injected synthetic key failure");
            }
            self.inner.emit_key_events(events)
        }

        fn get_backlight(&mut self) -> Result<f64> {
            self.inner.get_backlight()
        }

        fn set_backlight(&mut self, level: f64) -> Result<()> {
            if self.fail_next_backlight {
                self.fail_next_backlight = false;
                bail!("injected backlight failure");
            }
            self.inner.set_backlight(level)
        }

        fn release(&mut self) -> Result<()> {
            self.inner.release()
        }
    }

    struct SessionSwitchingHardware {
        inner: FakeTouchBar,
        logind: FakeLogind,
        uid: libc::uid_t,
        switch_on_present: bool,
    }

    impl SessionSwitchingHardware {
        fn new(logind: FakeLogind, uid: libc::uid_t) -> Self {
            Self {
                inner: FakeTouchBar::new(),
                logind,
                uid,
                switch_on_present: false,
            }
        }
    }

    impl TouchBarHardware for SessionSwitchingHardware {
        fn claim(&mut self) -> Result<()> {
            self.inner.claim()
        }

        fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
            self.inner.poll(timeout)
        }

        fn input_state(&self) -> InputState {
            self.inner.input_state()
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            self.inner.present(frame)?;
            if self.switch_on_present {
                self.switch_on_present = false;
                self.logind.set_active(
                    "seat0",
                    Some(ActiveSession {
                        id: "new-session".into(),
                        uid: self.uid,
                    }),
                );
            }
            Ok(())
        }

        fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
            self.inner.emit_key_events(events)
        }

        fn get_backlight(&mut self) -> Result<f64> {
            self.inner.get_backlight()
        }

        fn set_backlight(&mut self, level: f64) -> Result<()> {
            self.inner.set_backlight(level)
        }

        fn release(&mut self) -> Result<()> {
            self.inner.release()
        }
    }

    #[test]
    fn partial_client_disconnect_does_not_block_shutdown() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _uid) = active_local_logind("seat-session");
        let server = thread::spawn(move || -> Result<()> {
            let mut supervisor =
                Supervisor::new_with_logind(FakeTouchBar::new(), state_file, logind)?;
            serve_for_test(listener, &mut supervisor, 1)
        });

        let mut client = UnixStream::connect(&socket)?;
        client.write_all(&(5u32.to_be_bytes()))?;
        client.write_all(b"x")?;
        client.shutdown(std::net::Shutdown::Write)?;
        let mut reply = Vec::new();
        client.read_to_end(&mut reply)?;

        server.join().expect("supervisor thread panicked")?;
        assert_eq!(reply.first(), Some(&1));
        Ok(())
    }

    #[test]
    fn request_load_does_not_starve_hardware_polling_between_applies() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let timer_marker = directory.path().join("timer-fired");
        let first_marker = directory.path().join("first-started");
        let second_marker = directory.path().join("second-started");
        let second_gate = directory.path().join("second-release");
        let first = directory.path().join("first.lua");
        std::fs::write(
            &first,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local staging = assert(io.open({staging:?}, "w"))
                staging:close()
                local marker = {marker:?}
                sliver.timer.after(0, function()
                    local file = assert(io.open(marker, "w"))
                    file:close()
                end)
                local gate = {gate:?}
                while true do
                    local file = io.open(gate)
                    if file then file:close(); break end
                end
                return {{ api_version = 1, render = function() end }}
                "#,
                staging = first_marker.to_string_lossy(),
                marker = timer_marker.to_string_lossy(),
                gate = directory.path().join("first-release").to_string_lossy(),
            ),
        )?;
        let first_gate = directory.path().join("first-release");
        let second = directory.path().join("second.lua");
        std::fs::write(
            &second,
            format!(
                r#"
                local marker = assert(io.open({marker:?}, "w"))
                marker:close()
                local gate = {gate:?}
                while true do
                    local file = io.open(gate)
                    if file then file:close(); break end
                end
                require("sliver.v1")
                return {{ api_version = 1, render = function() end }}
                "#,
                marker = second_marker.to_string_lossy(),
                gate = second_gate.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _uid) = active_local_logind("seat-session");
        let server_logind = logind.clone();
        let server_state = state_file.clone();
        let server = thread::spawn(move || -> Result<Supervisor<FakeTouchBar, FakeLogind>> {
            let mut supervisor =
                Supervisor::new_with_logind(FakeTouchBar::new(), server_state, server_logind)?;
            serve_for_test(listener, &mut supervisor, 2)?;
            Ok(supervisor)
        });

        let first_socket = socket.clone();
        let first_client =
            thread::spawn(move || crate::apply_ipc::request_apply_at(&first_socket, &first));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !first_marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        anyhow::ensure!(first_marker.exists(), "first request did not enter staging");
        let second_socket = socket.clone();
        let second_client =
            thread::spawn(move || crate::apply_ipc::request_apply_at(&second_socket, &second));
        std::fs::write(&first_gate, "go")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !second_marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        anyhow::ensure!(
            second_marker.exists(),
            "second request did not enter staging"
        );
        assert!(
            timer_marker.exists(),
            "timer polling was starved between requests"
        );
        std::fs::write(&second_gate, "go")?;
        first_client.join().expect("first client panicked")?;
        second_client.join().expect("second client panicked")?;
        let supervisor = server.join().expect("supervisor thread panicked")?;
        supervisor.shutdown()?;
        Ok(())
    }

    struct SharedFakeHardware {
        inner: FakeTouchBar,
        synthetic: Rc<RefCell<Vec<FakeKeyEvent>>>,
        order_file: Option<std::path::PathBuf>,
    }

    impl SharedFakeHardware {
        fn with_order(
            order_file: Option<std::path::PathBuf>,
        ) -> (Self, Rc<RefCell<Vec<FakeKeyEvent>>>) {
            let synthetic = Rc::new(RefCell::new(Vec::new()));
            (
                Self {
                    inner: FakeTouchBar::new(),
                    synthetic: synthetic.clone(),
                    order_file,
                },
                synthetic,
            )
        }
    }

    impl TouchBarHardware for SharedFakeHardware {
        fn claim(&mut self) -> Result<()> {
            self.inner.claim()
        }

        fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
            self.inner.poll(timeout)
        }

        fn input_state(&self) -> InputState {
            self.inner.input_state()
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            self.inner.present(frame)
        }

        fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
            for event in events {
                let key = match event.key {
                    OutputKey::Keyboard(key) => FakeKey::Keyboard(key),
                    OutputKey::Consumer(key) => FakeKey::Consumer(key),
                };
                self.synthetic.borrow_mut().push(FakeKeyEvent {
                    key,
                    active: event.active,
                });
                if !event.active {
                    if let Some(path) = &self.order_file {
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)?;
                        writeln!(file, "key-up")?;
                    }
                }
            }
            self.inner.emit_key_events(events)
        }

        fn get_backlight(&mut self) -> Result<f64> {
            self.inner.get_backlight()
        }

        fn set_backlight(&mut self, level: f64) -> Result<()> {
            self.inner.set_backlight(level)
        }

        fn release(&mut self) -> Result<()> {
            if let Some(path) = &self.order_file {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                writeln!(file, "hardware-release")?;
            }
            self.inner.release()
        }
    }

    #[test]
    fn owner_handoff_cleans_before_logout_and_refreshes_input_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let order_file = directory.path().join("handoff-order");
        let state_log = directory.path().join("handoff-state");
        let old_source = directory.path().join("old-owner.lua");
        let new_source = directory.path().join("new-owner.lua");
        std::fs::write(
            &old_source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local order = {order:?}
                return {{
                    api_version = 1,
                    stop = function(reason)
                        local file = assert(io.open(order, "a"))
                        file:write(reason, "\n")
                        file:close()
                    end,
                    touch = function(event)
                        if event.phase == "down" then
                            sliver.input.key.down(sliver.input.keys.keyboard.f2)
                        end
                    end,
                    render = function() end,
                }}
                "#,
                order = order_file.to_string_lossy(),
            ),
        )?;
        std::fs::write(
            &new_source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local state_log = {state_log:?}
                return {{
                    api_version = 1,
                    start = function()
                        local state = sliver.input.state()
                        local file = assert(io.open(state_log, "w"))
                        file:write(tostring(state.fn), ":", tostring(state.modifiers.left_ctrl))
                        file:close()
                    end,
                    render = function() end,
                }}
                "#,
                state_log = state_log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (hardware, _) = SharedFakeHardware::with_order(Some(order_file.clone()));
        let mut supervisor = Supervisor::new(hardware, state_file)?;
        supervisor.apply(&old_source)?;
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Fn { active: true });
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Modifier {
                modifier: Modifier::LeftCtrl,
                active: true,
            });

        supervisor.handoff_owner()?;
        assert_eq!(std::fs::read_to_string(&order_file)?, "key-up\nlogout\n");
        assert_eq!(supervisor.hardware().inner.virtual_keyboard_creations(), 1);
        assert_eq!(
            supervisor.hardware().inner.virtual_keyboard_name(),
            Some("Sliver Keyboard")
        );

        supervisor.apply(&new_source)?;
        assert_eq!(std::fs::read_to_string(&state_log)?, "true:true");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn failing_logout_still_completes_owner_handoff() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let order_file = directory.path().join("failing-handoff-order");
        let old_source = directory.path().join("failing-old.lua");
        let new_source = directory.path().join("after-logout.lua");
        std::fs::write(
            &old_source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local order = {order:?}
                return {{
                    api_version = 1,
                    stop = function(reason)
                        local file = assert(io.open(order, "a"))
                        file:write(reason, "\n")
                        file:close()
                        error("logout cleanup failed")
                    end,
                    touch = function(event)
                        if event.phase == "down" then
                            sliver.input.key.down(sliver.input.keys.keyboard.f2)
                        end
                    end,
                    render = function() end,
                }}
                "#,
                order = order_file.to_string_lossy(),
            ),
        )?;
        std::fs::write(
            &new_source,
            "require(\"sliver.v1\"); return { api_version = 1, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (hardware, _) = SharedFakeHardware::with_order(Some(order_file.clone()));
        let mut supervisor = Supervisor::new(hardware, state_file)?;
        supervisor.apply(&old_source)?;
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(1.0)?;

        supervisor
            .handoff_owner()
            .expect("failing logout aborted owner handoff");
        assert_eq!(std::fs::read_to_string(&order_file)?, "key-up\nlogout\n");
        supervisor.apply(&new_source)?;
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn held_fn_at_worker_commit_starts_recovery_clock() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("healthy.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let input_state = InputState {
            fn_active: true,
            ..InputState::default()
        };
        let mut supervisor =
            Supervisor::new(FakeTouchBar::with_input_state(input_state), state_file)?;
        supervisor.apply(&source)?;

        supervisor.step_at(1.9)?;
        assert!(supervisor.recovery.is_none());
        supervisor.step_at(2.1)?;
        assert!(supervisor.recovery.is_some());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn reapplying_during_fn_hold_keeps_original_recovery_deadline() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("healthy.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;

        supervisor.origin = Instant::now() - Duration::from_secs(2);
        supervisor.apply(&source)?;

        supervisor.step_at(2.9)?;
        assert!(supervisor.recovery.is_none());
        supervisor.step_at(3.0)?;
        assert!(supervisor.recovery.is_some());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn deadline_takes_recovery_before_due_worker_drive() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("timed.lua");
        let log = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                sliver.timer.after(2.0, function()
                    record("timer")
                    sliver.redraw()
                end)
                return {{
                    api_version = 1,
                    visibility = function(event)
                        record("visibility:" .. tostring(event.visible))
                    end,
                    render = function() record("render") end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.step_at(3.0)?;

        assert_eq!(
            std::fs::read_to_string(&log)?,
            "render\nvisibility:false\ntimer\n"
        );
        assert!(supervisor.recovery.is_some());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn recovery_runs_timers_without_presenting_hidden_lua_frames() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("timed.lua");
        let log = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                sliver.timer.every(1.0, function()
                    record("timer")
                    sliver.redraw()
                    sliver.redraw()
                end)
                return {{
                    api_version = 1,
                    render = function() record("render") end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.step_at(3.0)?;
        assert!(supervisor.recovery.is_some());
        let frames_after_entry = supervisor.hardware().presented_frames().len();
        let count = |name: &str| -> Result<usize> {
            Ok(std::fs::read_to_string(&log)?
                .lines()
                .filter(|line| *line == name)
                .count())
        };
        let renders_before_hidden = count("render")?;
        let timers_before_hidden = count("timer")?;

        supervisor.step_at(6.0)?;

        assert!(count("timer")? > timers_before_hidden);
        assert_eq!(count("render")?, renders_before_hidden);
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            frames_after_entry
        );

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.step_at(7.0)?;
        assert!(supervisor.recovery.is_none());
        assert_eq!(count("render")?, renders_before_hidden + 1);
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            frames_after_entry + 1
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn hidden_timer_brightness_is_restored_after_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("timed-backlight.lua");
        let log = directory.path().join("timer-events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                sliver.timer.after(5.0, function()
                    local file = assert(io.open(log, "w"))
                    file:write("timer")
                    file:close()
                    sliver.backlight.set(0.25)
                end)
                return {{
                    api_version = 1,
                    render = function() end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.step_at(3.0)?;

        assert_eq!(supervisor.hardware().backlight_level(), 0.75);

        supervisor.step_at(6.0)?;
        assert_eq!(std::fs::read_to_string(&log)?, "timer");
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.step_at(7.0)?;

        assert_eq!(supervisor.hardware().backlight_level(), 0.25);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn recovery_cancels_touch_and_releases_lua_held_key() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("keys.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local key = sliver.input.keys.keyboard.f1
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.down(key)
                    elseif event.phase == "cancel" then
                        sliver.input.key.up(key)
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(2.0)?;
        supervisor.step_at(4.0)?;

        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: false,
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn healthy_recovery_exits_before_later_same_batch_touch_up() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("healthy.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.step_at(3.0)?;
        assert!(supervisor.recovery.is_some());

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(3.1)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Up)));
        supervisor.step_at(4.0)?;

        assert!(supervisor.recovery.is_none());
        assert!(supervisor.hardware().synthetic_keys().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn later_render_failure_cancels_contacts_before_fixed_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("failing-render.lua");
        let events = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {events:?}
                local renders = 0
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                return {{
                    api_version = 1,
                    touch = function(event)
                        record(event.phase)
                        if event.phase == "down" then sliver.redraw() end
                    end,
                    render = function()
                        renders = renders + 1
                        if renders > 1 then error("later render failed") end
                    end,
                }}
                "#,
                events = events.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        let error = supervisor
            .step_at(1.0)
            .expect_err("later render failure was not reported");
        assert!(format!("{error:#}").contains("later render failed"));
        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_some());
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        assert!(supervisor.hardware().presented_frames().len() >= 2);
        assert_eq!(std::fs::read_to_string(&events)?, "down\ncancel\n");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn shutdown_and_drop_release_tracked_synthetic_keys() -> Result<()> {
        let exercise = |shutdown: bool| -> Result<Vec<FakeKeyEvent>> {
            let directory = tempfile::tempdir()?;
            let source = directory.path().join("held.lua");
            std::fs::write(
                &source,
                r#"
                local sliver = require("sliver.v1")
                return {
                    api_version = 1,
                    touch = function(event)
                        if event.phase == "down" then
                            sliver.input.key.down(sliver.input.keys.keyboard.f2)
                        end
                    end,
                    render = function() end,
                }
                "#,
            )?;
            let state_file = directory.path().join("state/sliver/config-path");
            let order_file = directory.path().join("shutdown-order");
            let (hardware, synthetic) = SharedFakeHardware::with_order(Some(order_file.clone()));
            let mut supervisor = Supervisor::new(hardware, state_file)?;
            supervisor.apply(&source)?;
            supervisor
                .hardware_mut()
                .inner
                .inject(HardwareEvent::Touch(TouchEvent {
                    phase: TouchPhase::Down,
                    id: 1,
                    time: 0.0,
                    x: 1.0,
                    y: 1.0,
                    modifiers: ModifierState::default(),
                    pressure: None,
                    width: None,
                    height: None,
                }));
            supervisor.step_at(1.0)?;
            if shutdown {
                supervisor.shutdown()?;
            } else {
                drop(supervisor);
            }
            let events = synthetic.borrow().clone();
            assert_eq!(
                std::fs::read_to_string(order_file)?,
                "key-up\nhardware-release\n"
            );
            Ok(events)
        };

        let expected = vec![
            FakeKeyEvent {
                key: FakeKey::Keyboard(KeyboardKey::F2),
                active: true,
            },
            FakeKeyEvent {
                key: FakeKey::Keyboard(KeyboardKey::F2),
                active: false,
            },
        ];
        assert_eq!(exercise(true)?, expected);
        assert_eq!(exercise(false)?, expected);
        Ok(())
    }

    #[test]
    fn key_cleanup_failure_rolls_back_candidate_after_commit_steps() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let old_source = directory.path().join("old.lua");
        let new_source = directory.path().join("new.lua");
        std::fs::write(
            &old_source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.down(sliver.input.keys.keyboard.f2)
                    end
                end,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        std::fs::write(
            &new_source,
            r#"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1)
                end,
            }
            "#,
        )?;
        let mut supervisor = Supervisor::new(
            FailingPresentHardware::new(state_file.clone()),
            state_file.clone(),
        )?;
        supervisor.apply(&old_source)?;
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(1.0)?;
        supervisor.hardware_mut().fail_next_key = true;

        let error = supervisor
            .apply(&new_source)
            .expect_err("key cleanup failure committed a candidate");
        assert!(format!("{error:#}").contains("injected synthetic key failure"));
        assert_eq!(
            supervisor.hardware().state_seen_at_key_failure,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .expect("old frame disappeared")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn presentation_failure_restores_previous_selected_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let old_source = directory.path().join("old.lua");
        let new_source = directory.path().join("new.lua");
        let config = |red: u8, blue: u8| {
            format!(
                "require('sliver.v1'); return {{ api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, {}, 0, {}, 1) end }}",
                f64::from(red) / 255.0,
                f64::from(blue) / 255.0,
            )
        };
        std::fs::write(&old_source, config(255, 0))?;
        std::fs::write(&new_source, config(0, 255))?;
        let hardware = FailingPresentHardware::new(state_file.clone());
        let mut supervisor = Supervisor::new(hardware, state_file.clone())?;
        supervisor.apply(&old_source)?;
        supervisor.hardware_mut().fail_next_present = true;

        let error = supervisor
            .apply(&new_source)
            .expect_err("injected presentation failure was ignored");

        assert!(format!("{error:#}").contains("injected presentation failure"));
        assert_eq!(
            supervisor.hardware().state_seen_at_failure,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(supervisor.hardware().inner.presented_frames().len(), 2);
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .expect("previous frame was not restored")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn rejected_candidate_preserves_active_state_and_replacement_stops_after_commit() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let stop_log = directory.path().join("stop-log");
        let old_source = directory.path().join("old.lua");
        std::fs::write(
            &old_source,
            format!(
                r#"
                require("sliver.v1")
                local state_file = {state_file:?}
                local stop_log = {stop_log:?}
                return {{
                    api_version = 1,
                    stop = function(reason)
                        local selected = assert(io.open(state_file)):read("*a")
                        local log = assert(io.open(stop_log, "w"))
                        log:write(reason, ":", selected)
                        log:close()
                    end,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                    end,
                }}
                "#,
                state_file = state_file.to_string_lossy(),
                stop_log = stop_log.to_string_lossy(),
            ),
        )?;
        let bad_source = directory.path().join("bad.lua");
        let irreversible_marker = directory.path().join("candidate-side-effect");
        std::fs::write(
            &bad_source,
            format!(
                r#"
                require("sliver.v1")
                local marker = assert(io.open({marker:?}, "w"))
                marker:write("kept")
                marker:close()
                return {{
                    api_version = 1,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1)
                        error("candidate failed")
                    end,
                }}
                "#,
                marker = irreversible_marker.to_string_lossy(),
            ),
        )?;
        let new_source = directory.path().join("new.lua");
        std::fs::write(
            &new_source,
            r#"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1)
                end,
            }
            "#,
        )?;
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;
        supervisor.apply(&old_source)?;

        let error = supervisor
            .apply(&bad_source)
            .expect_err("failed candidate was committed");

        assert!(format!("{error:#}").contains("candidate failed"));
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(std::fs::read_to_string(&irreversible_marker)?, "kept");
        assert!(!stop_log.exists());
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .context("old frame disappeared after rejection")?
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );

        supervisor.apply(&new_source)?;

        assert_eq!(
            std::fs::read(&state_file)?,
            new_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            std::fs::read_to_string(&stop_log)?,
            format!("replaced:{}", new_source.display())
        );
        assert_eq!(
            supervisor
                .hardware()
                .actions()
                .iter()
                .filter(|action| matches!(action, FakeAction::Present))
                .count(),
            2
        );
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .context("new frame was not committed")?
                .rgba_at(10, 10),
            [0, 0, 255, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn held_synthetic_keys_are_released_when_a_worker_fails() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("failing-key.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                touch = function(event)
                    if event.x == 1 then
                        sliver.input.key.down(sliver.input.keys.keyboard.f2)
                    else
                        error("worker failed after holding a key")
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        let touch = |id, x| TouchEvent {
            phase: TouchPhase::Down,
            id,
            time: 0.0,
            x,
            y: 1.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(touch(1, 1.0)));
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(touch(2, 2.0)));
        let error = supervisor
            .step_at(2.0)
            .expect_err("worker failure was swallowed");
        assert!(format!("{error:#}").contains("worker failed after holding a key"));
        assert_eq!(
            supervisor.hardware().synthetic_transactions()[1],
            vec![FakeKeyEvent {
                key: FakeKey::Keyboard(KeyboardKey::F2),
                active: false,
            }]
        );
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        let frames_after_failure = supervisor.hardware().presented_frames().len();
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(touch(3, 1.0)));
        supervisor.step_at(3.0)?;
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            frames_after_failure + 1
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn invalid_key_request_from_active_callback_enters_fixed_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("invalid-active-key.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local key = sliver.input.keys.keyboard.f1
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then sliver.input.key.up(key) end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        let error = supervisor
            .step_at(1.0)
            .expect_err("invalid active key request was accepted");
        assert!(format!("{error:#}").contains("synthetic key is not held"));
        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        assert!(supervisor.active.is_none());
        assert!(supervisor
            .recovery
            .as_ref()
            .is_some_and(|recovery| !recovery.owner_is_healthy()));
        assert!(supervisor.next_worker_deadline.is_none());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn invalid_key_request_from_hidden_timer_enters_fixed_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("invalid-hidden-key.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local key = sliver.input.keys.keyboard.f1
            sliver.timer.after(5.0, function() sliver.input.key.up(key) end)
            return { api_version = 1, render = function() end }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.step_at(3.0)?;
        assert!(supervisor
            .recovery
            .as_ref()
            .is_some_and(|recovery| recovery.owner_is_healthy()));

        let error = supervisor
            .step_at(6.0)
            .expect_err("invalid hidden key request was accepted");
        assert!(format!("{error:#}").contains("synthetic key is not held"));
        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        assert!(supervisor.active.is_none());
        assert!(supervisor
            .recovery
            .as_ref()
            .is_some_and(|recovery| !recovery.owner_is_healthy()));
        assert!(supervisor.next_worker_deadline.is_none());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn synthetic_key_requests_are_rejected_during_staging() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("staged-key.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            sliver.input.key.tap(sliver.input.keys.keyboard.escape)
            return { api_version = 1, render = function() end }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        let error = supervisor
            .apply(&source)
            .expect_err("staged synthetic key output was accepted");
        assert!(format!("{error:#}").contains("unavailable while staging"));
        assert!(supervisor.hardware().synthetic_keys().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn synthetic_key_holds_are_released_before_worker_replacement() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old-key.lua");
        let new_source = directory.path().join("new-key.lua");
        std::fs::write(
            &old_source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.down(sliver.input.keys.keyboard.f2)
                    elseif event.phase == "up" then
                        sliver.input.key.up(sliver.input.keys.keyboard.f2)
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        std::fs::write(
            &new_source,
            r#"
            require("sliver.v1")
            return { api_version = 1, render = function() end }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut hardware = FakeTouchBar::new();
        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: true,
        });
        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftAlt,
            active: true,
        });
        let mut supervisor = Supervisor::new(hardware, state_file)?;
        assert_eq!(
            supervisor.hardware().virtual_keyboard_name(),
            Some("Sliver Keyboard")
        );
        assert_eq!(supervisor.hardware().virtual_keyboard_creations(), 1);
        supervisor.apply(&old_source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(1.0)?;
        assert_eq!(supervisor.hardware().synthetic_transactions().len(), 1);

        supervisor.apply(&new_source)?;
        assert_eq!(supervisor.hardware().virtual_keyboard_creations(), 1);
        assert_eq!(
            supervisor.hardware().synthetic_transactions()[1],
            vec![
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftAlt),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false,
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn touch_can_emit_keyboard_and_consumer_taps() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("keys.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.tap(sliver.input.keys.keyboard.escape)
                        sliver.input.key.tap(sliver.input.keys.consumer.play_pause)
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(1.0)?;

        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::PlayPause),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::PlayPause),
                    active: false,
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn overlapping_held_keys_reference_count_inherited_and_explicit_modifiers() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("overlapping-keys.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local keys = sliver.input.keys
            local explicit = { modifiers = { keys.keyboard.right_ctrl } }
            return {
                api_version = 1,
                touch = function(event)
                    local key = ({
                        [1] = keys.keyboard.f2,
                        [2] = keys.keyboard.f3,
                        [3] = keys.keyboard.f4,
                        [4] = keys.keyboard.f5,
                    })[event.id]
                    local options = event.id >= 3 and explicit or nil
                    if event.phase == "down" then
                        sliver.input.key.down(key, options)
                    elseif event.phase == "up" then
                        sliver.input.key.up(key)
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut hardware = FakeTouchBar::new();
        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: true,
        });
        let mut supervisor = Supervisor::new(hardware, state_file)?;
        supervisor.apply(&source)?;
        for id in 1..=4 {
            supervisor
                .hardware_mut()
                .inject(HardwareEvent::Touch(overlap_touch(id, TouchPhase::Down)));
        }
        for id in 1..=4 {
            supervisor
                .hardware_mut()
                .inject(HardwareEvent::Touch(overlap_touch(id, TouchPhase::Up)));
        }
        supervisor.step_at(1.0)?;

        assert_eq!(
            supervisor.hardware().synthetic_transactions(),
            &[vec![
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F3),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::RightCtrl),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F4),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F5),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F3),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F4),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F5),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::RightCtrl),
                    active: false,
                },
            ]]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    fn overlap_touch(id: u32, phase: TouchPhase) -> TouchEvent {
        TouchEvent {
            phase,
            id,
            time: 0.0,
            x: f64::from(id),
            y: 1.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn tap_rejects_a_primary_key_held_by_down_before_up() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("held-primary.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local key = sliver.input.keys.keyboard.f2
            return {
                api_version = 1,
                touch = function(event)
                    if event.id == 1 and event.phase == "down" then
                        sliver.input.key.down(key, { modifiers = false })
                    elseif event.id == 2 and event.phase == "down" then
                        sliver.input.key.tap(key, { modifiers = false })
                    elseif event.id == 1 and event.phase == "up" then
                        sliver.input.key.up(key)
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        for (id, phase) in [
            (1, TouchPhase::Down),
            (2, TouchPhase::Down),
            (1, TouchPhase::Up),
        ] {
            supervisor
                .hardware_mut()
                .inject(HardwareEvent::Touch(overlap_touch(id, phase)));
        }
        let error = supervisor
            .step_at(1.0)
            .expect_err("tap of an already-held key was accepted");
        assert!(format!("{error:#}").contains("synthetic key is already held"));
        assert!(supervisor.hardware().synthetic_transactions().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn tap_rejects_a_modifier_owned_standalone_or_by_a_chord() -> Result<()> {
        let run = |chord: bool| -> Result<()> {
            let directory = tempfile::tempdir()?;
            let source = directory.path().join("held-modifier.lua");
            std::fs::write(
                &source,
                if chord {
                    r#"
                    local sliver = require("sliver.v1")
                    local keys = sliver.input.keys.keyboard
                    return {
                        api_version = 1,
                        touch = function(event)
                            if event.id == 1 and event.phase == "down" then
                                sliver.input.key.down(keys.f2, {
                                    modifiers = { keys.left_ctrl },
                                })
                            elseif event.id == 2 and event.phase == "down" then
                                sliver.input.key.tap(keys.left_ctrl, { modifiers = false })
                            end
                        end,
                        render = function() end,
                    }
                    "#
                } else {
                    r#"
                    local sliver = require("sliver.v1")
                    local key = sliver.input.keys.keyboard.left_ctrl
                    return {
                        api_version = 1,
                        touch = function(event)
                            if event.id == 1 and event.phase == "down" then
                                sliver.input.key.down(key, { modifiers = false })
                            elseif event.id == 2 and event.phase == "down" then
                                sliver.input.key.tap(key, { modifiers = false })
                            end
                        end,
                        render = function() end,
                    }
                    "#
                },
            )?;
            let state_file = directory.path().join("state/sliver/config-path");
            let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
            supervisor.apply(&source)?;
            for id in 1..=2 {
                supervisor
                    .hardware_mut()
                    .inject(HardwareEvent::Touch(overlap_touch(id, TouchPhase::Down)));
            }
            let error = supervisor
                .step_at(1.0)
                .expect_err("tap of an owned modifier was accepted");
            assert!(format!("{error:#}").contains("synthetic modifier is already held"));
            assert!(supervisor.hardware().synthetic_transactions().is_empty());
            supervisor.shutdown()?;
            Ok(())
        };

        run(false)?;
        run(true)
    }

    #[test]
    fn explicit_modifier_lists_reject_nonadjacent_duplicates() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("duplicate-modifiers.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local keys = sliver.input.keys
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.tap(keys.keyboard.f2, {
                            modifiers = {
                                keys.keyboard.left_ctrl,
                                keys.keyboard.left_alt,
                                keys.keyboard.left_ctrl,
                            },
                        })
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        let error = supervisor
            .step_at(1.0)
            .expect_err("nonadjacent duplicate modifier was accepted");
        assert!(format!("{error:#}").contains("must not contain duplicates"));
        assert!(supervisor.hardware().synthetic_transactions().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn standalone_modifier_shares_ownership_with_mirrored_modifier() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("modifier-owner.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local keys = sliver.input.keys.keyboard
            return {
                api_version = 1,
                touch = function(event)
                    if event.id == 1 and event.phase == "down" then
                        sliver.input.key.down(keys.left_ctrl, { modifiers = false })
                    elseif event.id == 2 and event.phase == "down" then
                        sliver.input.key.down(keys.f2, {
                            modifiers = { keys.left_ctrl },
                        })
                    elseif event.id == 1 and event.phase == "up" then
                        sliver.input.key.up(keys.left_ctrl)
                    elseif event.id == 2 and event.phase == "up" then
                        sliver.input.key.up(keys.f2)
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        for (id, phase) in [
            (1, TouchPhase::Down),
            (2, TouchPhase::Down),
            (1, TouchPhase::Up),
            (2, TouchPhase::Up),
        ] {
            supervisor
                .hardware_mut()
                .inject(HardwareEvent::Touch(overlap_touch(id, phase)));
        }
        supervisor.step_at(1.0)?;

        assert_eq!(
            supervisor.hardware().synthetic_transactions()[0],
            vec![
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false,
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn overlapping_held_keys_cleanup_releases_each_modifier_once() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("held-old.lua");
        let new_source = directory.path().join("held-new.lua");
        std::fs::write(
            &old_source,
            r#"
            local sliver = require("sliver.v1")
            local keys = sliver.input.keys.keyboard
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.down(event.id == 1 and keys.f2 or keys.f3)
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        std::fs::write(
            &new_source,
            "require(\"sliver.v1\"); return { api_version = 1, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut hardware = FakeTouchBar::new();
        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: true,
        });
        let mut supervisor = Supervisor::new(hardware, state_file)?;
        supervisor.apply(&old_source)?;
        for id in 1..=2 {
            supervisor
                .hardware_mut()
                .inject(HardwareEvent::Touch(overlap_touch(id, TouchPhase::Down)));
        }
        supervisor.step_at(1.0)?;
        supervisor.apply(&new_source)?;

        assert_eq!(
            supervisor.hardware().synthetic_transactions()[1],
            vec![
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F3),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false,
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn key_taps_bridge_inherited_suppressed_and_explicit_modifiers_in_one_transaction() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("modifier-keys.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.tap(sliver.input.keys.keyboard.f2)
                        sliver.input.key.tap(sliver.input.keys.keyboard.escape, { modifiers = false })
                        sliver.input.key.tap(sliver.input.keys.keyboard.f3, {
                            modifiers = { sliver.input.keys.keyboard.right_shift },
                        })
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut hardware = FakeTouchBar::new();
        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: true,
        });
        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftAlt,
            active: true,
        });
        let mut supervisor = Supervisor::new(hardware, state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(1.0)?;

        assert_eq!(
            supervisor.hardware().synthetic_transactions(),
            &[vec![
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftAlt),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftAlt),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::RightShift),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F3),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F3),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::RightShift),
                    active: false,
                },
            ]]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn deferred_input_updates_candidate_render_and_survives_replacement() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old-input.lua");
        let candidate_source = directory.path().join("candidate-input.lua");
        let old_log = directory.path().join("old-input-events");
        let candidate_log = directory.path().join("candidate-input-events");
        let render_log = directory.path().join("candidate-render-state");
        let old_config = format!(
            r#"
            local sliver = require("sliver.v1")
            local log = {log:?}
            local function record(event)
                local file = assert(io.open(log, "a"))
                file:write(event.key, ":", event.phase, "\n")
                file:close()
            end
            return {{
                api_version = 1,
                key = record,
                render = function() end,
            }}
            "#,
            log = old_log.to_string_lossy(),
        );
        let candidate_config = format!(
            r#"
            local sliver = require("sliver.v1")
            local event_log = {event_log:?}
            local render_log = {render_log:?}
            local function record(event)
                local file = assert(io.open(event_log, "a"))
                file:write(event.key, ":", event.phase, "\n")
                file:close()
            end
            return {{
                api_version = 1,
                key = record,
                render = function()
                    local state = sliver.input.state()
                    assert(state.fn)
                    assert(state.modifiers.left_ctrl)
                    local file = assert(io.open(render_log, "w"))
                    file:write("fresh")
                    file:close()
                end,
            }}
            "#,
            event_log = candidate_log.to_string_lossy(),
            render_log = render_log.to_string_lossy(),
        );
        std::fs::write(&old_source, old_config)?;
        std::fs::write(&candidate_source, candidate_config)?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&old_source)?;

        // The next apply polls once before staging and once after staging. The
        // changes arrive on that deferred poll, after the candidate's snapshot.
        supervisor
            .hardware_mut()
            .inject_on_poll(4, HardwareEvent::Fn { active: true });
        supervisor.hardware_mut().inject_on_poll(
            4,
            HardwareEvent::Modifier {
                modifier: Modifier::LeftCtrl,
                active: true,
            },
        );
        supervisor.apply(&candidate_source)?;

        assert_eq!(std::fs::read_to_string(render_log)?, "fresh");
        assert_eq!(
            std::fs::read_to_string(old_log)?,
            "fn:down\nleft_ctrl:down\n"
        );

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.hardware_mut().inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: false,
        });
        supervisor.step_at(1.0)?;
        assert_eq!(
            std::fs::read_to_string(candidate_log)?,
            "fn:up\nleft_ctrl:up\n"
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn due_fn_hold_during_staging_enters_fixed_recovery_before_deferred_input() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old.lua");
        let candidate_source = directory.path().join("candidate.lua");
        let old_log = directory.path().join("old-events");
        let candidate_log = directory.path().join("candidate-events");
        let marker = directory.path().join("staging");
        let gate = directory.path().join("release");
        std::fs::write(
            &old_source,
            format!(
                "require('sliver.v1'); return {{ api_version = 1, touch = function(event) local file = assert(io.open({old_log:?}, 'a')); file:write(event.phase, '\\n'); file:close() end, render = function() end }}",
                old_log = old_log.to_string_lossy(),
            ),
        )?;
        std::fs::write(
            &candidate_source,
            format!(
                r#"
                local marker = assert(io.open({marker:?}, "w"))
                marker:close()
                while true do
                    local file = io.open({gate:?})
                    if file then file:close(); break end
                end
                local sliver = require("sliver.v1")
                return {{
                    api_version = 1,
                    touch = function(event)
                        local file = assert(io.open({candidate_log:?}, "a"))
                        file:write(event.phase, "\n")
                        file:close()
                    end,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                    end,
                }}
                "#,
                marker = marker.to_string_lossy(),
                gate = gate.to_string_lossy(),
                candidate_log = candidate_log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;
        supervisor.apply(&old_source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.origin = Instant::now() - Duration::from_secs_f64(2.7);
        supervisor
            .hardware_mut()
            .inject_on_poll(5, HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));

        let marker_seen = marker.clone();
        let gate_to_open = gate.clone();
        let releaser = thread::spawn(move || -> Result<bool> {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !marker_seen.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            let staged = marker_seen.exists();
            thread::sleep(Duration::from_millis(700));
            std::fs::write(gate_to_open, "go")?;
            Ok(staged)
        });
        let apply_result = supervisor.apply(&candidate_source);
        let staged = releaser.join().expect("staging releaser panicked")?;
        anyhow::ensure!(staged, "candidate never entered staging");
        apply_result?;

        assert_eq!(
            std::fs::read(&state_file)?,
            candidate_source.as_os_str().as_encoded_bytes()
        );
        assert!(supervisor
            .recovery
            .as_ref()
            .is_some_and(|recovery| recovery.owner_is_healthy()));
        assert_eq!(std::fs::read_to_string(&old_log)?, "cancel\n");
        assert!(!candidate_log.exists());
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .unwrap()
                .rgba_at(10, 30),
            [0, 0, 0, 255]
        );

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Up)));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(2, TouchPhase::Down)));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(2, TouchPhase::Up)));
        supervisor.step_at(5.0)?;
        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: false
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn deferred_input_survives_a_failed_candidate_until_the_old_worker_drives() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old-input.lua");
        let bad_source = directory.path().join("bad-input.lua");
        let log = directory.path().join("old-input-events");
        std::fs::write(
            &old_source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                return {{
                    api_version = 1,
                    key = function(event)
                        local file = assert(io.open(log, "a"))
                        file:write(event.key, ":", event.phase, "\n")
                        file:close()
                    end,
                    render = function() end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        std::fs::write(
            &bad_source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                render = function()
                    assert(sliver.input.state().fn)
                    error("candidate failed")
                end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&old_source)?;
        supervisor
            .hardware_mut()
            .inject_on_poll(4, HardwareEvent::Fn { active: true });

        let error = supervisor
            .apply(&bad_source)
            .expect_err("failed candidate unexpectedly committed");
        assert!(format!("{error:#}").contains("candidate failed"));

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.step_at(1.0)?;
        assert_eq!(std::fs::read_to_string(log)?, "fn:down\nfn:up\n");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn lua_receives_fn_and_modifier_transitions_and_snapshots() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("input.lua");
        let log = directory.path().join("input-events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(line)
                    local file = assert(io.open(log, "a"))
                    file:write(line, "\n")
                    file:close()
                end
                return {{
                    api_version = 1,
                    start = function()
                        local state = sliver.input.state()
                        assert(state.fn)
                        assert(state.modifiers.left_ctrl)
                        record("start:" .. tostring(state.fn) .. ":" .. tostring(state.modifiers.left_ctrl))
                    end,
                    key = function(event)
                        local state = sliver.input.state()
                        assert(event.state.fn == state.fn)
                        assert(event.state.modifiers.right_alt == state.modifiers.right_alt)
                        record(event.key .. ":" .. event.phase .. ":" .. tostring(state.fn) .. ":" .. tostring(state.modifiers.left_ctrl) .. ":" .. tostring(state.modifiers.right_alt))
                    end,
                    render = function() end,
                }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut hardware = FakeTouchBar::new();
        hardware.inject(HardwareEvent::Fn { active: true });
        hardware.inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: true,
        });
        let mut supervisor = Supervisor::new(hardware, state_file)?;
        supervisor.apply(&source)?;
        assert_eq!(std::fs::read_to_string(&log)?, "start:true:true\n");

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.hardware_mut().inject(HardwareEvent::Modifier {
            modifier: Modifier::RightAlt,
            active: true,
        });
        supervisor.hardware_mut().inject(HardwareEvent::Modifier {
            modifier: Modifier::RightAlt,
            active: true,
        });
        supervisor.step_at(1.0)?;

        assert_eq!(
            std::fs::read_to_string(&log)?,
            "start:true:true\nfn:up:false:true:false\nright_alt:down:false:true:true\n"
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn touch_drives_frame_and_backlight_with_normalized_fields() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("touch.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            local active = false
            return {
                api_version = 1,
                touch = function(event)
                    assert(event.phase == "down")
                    assert(event.id == 7)
                    assert(event.x == 100)
                    assert(event.y == 20)
                    assert(event.time == 0.25)
                    assert(event.modifiers.left_ctrl)
                    assert(event.pressure == 0.5)
                    assert(event.width == 0.25)
                    assert(event.height == nil)
                    active = true
                    sliver.backlight.set(0.75)
                    sliver.redraw()
                end,
                render = function(canvas)
                    if active then
                        canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                    end
                end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;

        let mut modifiers = ModifierState::default();
        modifiers.set(Modifier::LeftCtrl, true);
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 7,
                time: 0.25,
                x: 100.0,
                y: 20.0,
                modifiers,
                pressure: Some(0.5),
                width: Some(0.25),
                height: None,
            }));
        supervisor.step_at(1.0)?;

        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        assert!(supervisor
            .hardware()
            .actions()
            .contains(&FakeAction::Backlight(0.75)));
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .context("touch did not produce a frame")?
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn coalesces_moves_without_reordering_transitions() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("touch-order.lua");
        let log = directory.path().join("touch-events");
        std::fs::write(
            &source,
            format!(
                r#"
                require("sliver.v1")
                local log = {log:?}
                return {{
                    api_version = 1,
                    touch = function(event)
                        local file = assert(io.open(log, "a"))
                        file:write(event.phase, ":", event.id, ":", math.floor(event.x), "\n")
                        file:close()
                    end,
                    render = function() end,
                }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;

        let touch = |phase, id, x| TouchEvent {
            phase,
            id,
            time: 0.0,
            x,
            y: 10.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        for event in [
            touch(TouchPhase::Down, 1, 10.0),
            touch(TouchPhase::Move, 1, 20.0),
            touch(TouchPhase::Down, 2, 5.0),
            touch(TouchPhase::Move, 1, 30.0),
            touch(TouchPhase::Move, 2, 40.0),
            touch(TouchPhase::Up, 1, 30.0),
            touch(TouchPhase::Cancel, 2, 40.0),
        ] {
            supervisor
                .hardware_mut()
                .inject(HardwareEvent::Touch(event));
        }
        supervisor.step_at(1.0)?;

        assert_eq!(
            std::fs::read_to_string(log)?,
            "down:1:10\ndown:2:5\nmove:1:30\nmove:2:40\nup:1:30\ncancel:2:40\n"
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn zero_delay_timer_waits_until_after_worker_commit() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("zero-timer.lua");
        let log = directory.path().join("timer-events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                sliver.timer.after(0, function()
                    local file = assert(io.open(log, "w"))
                    file:write("fired")
                    file:close()
                end)
                return {{ api_version = 1, render = function() end }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;

        supervisor.apply(&source)?;

        assert!(!log.exists(), "staged timer fired before commit");
        supervisor.step_at(supervisor.now_seconds())?;
        assert_eq!(std::fs::read_to_string(&log)?, "fired");
        supervisor.step_at(supervisor.now_seconds() + 1.0)?;
        assert_eq!(std::fs::read_to_string(&log)?, "fired");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn timers_skip_missed_repeats_and_cancel_idempotently() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("timers.lua");
        let log = directory.path().join("timer-events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(name)
                    local file = assert(io.open(log, "a"))
                    file:write(name, "\n")
                    file:close()
                end
                local canceled = sliver.timer.after(9, function() record("canceled") end)
                canceled:cancel()
                canceled:cancel()
                local active = false
                sliver.timer.after(0.25, function()
                    record("once")
                    active = true
                    sliver.backlight.set(0.5)
                    sliver.redraw()
                end)
                sliver.timer.every(0.5, function() record("repeat") end)
                return {{
                    api_version = 1,
                    render = function(canvas)
                        if active then canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end
                    end,
                }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        let committed_at = supervisor.now_seconds();

        supervisor.step_at(committed_at + 0.5)?;
        supervisor.step_at(committed_at + 2.75)?;

        assert_eq!(std::fs::read_to_string(log)?, "once\nrepeat\nrepeat\n");
        assert_eq!(supervisor.hardware().backlight_level(), 0.5);
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .context("timer did not produce a frame")?
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn slow_repeating_callbacks_schedule_the_next_interval_in_the_future() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("slow-timer.lua");
        let log = directory.path().join("timer-events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                sliver.timer.every(0.000001, function()
                    local total = 0
                    for i = 1, 1000000 do total = total + i end
                    local file = assert(io.open(log, "a"))
                    file:write(total, "\n")
                    file:close()
                end)
                return {{ api_version = 1, render = function() end }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;

        let drive_now = supervisor.now_seconds() + 1.0;
        supervisor.step_at(drive_now)?;
        supervisor.step_at(drive_now + 0.000002)?;

        let events = std::fs::read_to_string(log)?;
        assert_eq!(events.lines().count(), 1, "slow timer fired repeatedly");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn redraw_coalesces_and_a_render_request_schedules_one_follow_up() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("redraw.lua");
        let log = directory.path().join("renders");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local renders = 0
                sliver.timer.after(10, function() end)
                return {{
                    api_version = 1,
                    touch = function()
                        sliver.redraw()
                        sliver.redraw()
                    end,
                    render = function()
                        renders = renders + 1
                        local file = assert(io.open(log, "a"))
                        file:write(renders, "\n")
                        file:close()
                        if renders == 2 then sliver.redraw() end
                    end,
                }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        assert_eq!(std::fs::read_to_string(&log)?, "1\n");

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(1.0)?;
        assert_eq!(std::fs::read_to_string(&log)?, "1\n2\n");

        supervisor.step_at(2.0)?;
        assert_eq!(std::fs::read_to_string(&log)?, "1\n2\n3\n");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn rendered_frames_receive_intended_time_and_previous_presented_delta() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("frame-time.lua");
        let log = directory.path().join("frame-times");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                sliver.timer.after(0.5, function() sliver.redraw() end)
                return {{
                    api_version = 1,
                    render = function(_, time, delta)
                        assert(type(time) == "number")
                        assert(type(delta) == "number")
                        local file = assert(io.open(log, "a"))
                        file:write(time, " ", delta, "\n")
                        file:close()
                    end,
                }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        let committed_at = supervisor.now_seconds();
        supervisor.step_at(committed_at + 1.0)?;

        let lines: Vec<_> = std::fs::read_to_string(log)?
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .map(|value| value.parse::<f64>().expect("frame timing was numeric"))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][1], 0.0);
        assert!(lines[1][0] > lines[0][0]);
        assert!((lines[1][1] - 1.0).abs() < 0.002);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn slow_replacement_retimes_candidate_after_old_frame_presentation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old-present.lua");
        let candidate_source = directory.path().join("candidate-present.lua");
        let old_log = directory.path().join("old-present-times");
        let candidate_log = directory.path().join("candidate-present-times");
        let candidate_counts = directory.path().join("candidate-counts");
        let old_config = format!(
            r#"
            local sliver = require("sliver.v1")
            local log = {log:?}
            return {{
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then sliver.redraw() end
                end,
                render = function(_, time, delta)
                    local file = assert(io.open(log, "a"))
                    file:write(time, " ", delta, "\n")
                    file:close()
                end,
            }}
            "#,
            log = old_log.to_string_lossy(),
        );
        let candidate_config = format!(
            r#"
            local sliver = require("sliver.v1")
            local log = {log:?}
            local counts = {counts:?}
            local renders = 0
            local registrations = 0
            local function register_timer()
                registrations = registrations + 1
                sliver.timer.after(100, function() end)
            end
            return {{
                api_version = 1,
                start = function()
                    register_timer()
                    local deadline = os.clock() + 0.08
                    while os.clock() < deadline do end
                end,
                render = function(_, time, delta)
                    renders = renders + 1
                    register_timer()
                    sliver.backlight.set(0.75)
                    local file = assert(io.open(log, "a"))
                    file:write(time, " ", delta, "\n")
                    file:close()
                    local count_file = assert(io.open(counts, "a"))
                    count_file:write(renders, " ", registrations, "\n")
                    count_file:close()
                end,
            }}
            "#,
            log = candidate_log.to_string_lossy(),
            counts = candidate_counts.to_string_lossy(),
        );
        std::fs::write(&old_source, old_config)?;
        std::fs::write(&candidate_source, candidate_config)?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&old_source)?;
        supervisor.hardware_mut().inject_on_poll(
            4,
            HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }),
        );
        supervisor.apply(&candidate_source)?;

        let parse = |path: &std::path::Path| -> Result<Vec<[f64; 2]>> {
            std::fs::read_to_string(path)?
                .lines()
                .map(|line| {
                    let values: Vec<_> = line
                        .split_whitespace()
                        .map(|value| value.parse::<f64>())
                        .collect::<std::result::Result<_, _>>()?;
                    anyhow::ensure!(values.len() == 2, "frame timing line had the wrong shape");
                    Ok([values[0], values[1]])
                })
                .collect()
        };
        let old_frames = parse(&old_log)?;
        let candidate_frames = parse(&candidate_log)?;
        let counts = std::fs::read_to_string(candidate_counts)?;
        assert_eq!(old_frames.len(), 2);
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            2,
            "old worker presented during candidate staging"
        );
        assert_eq!(
            candidate_frames.len(),
            1,
            "candidate rendered more than once"
        );
        assert_eq!(candidate_frames[0][1], 0.0);
        assert!(candidate_frames[0][0] >= old_frames[0][0]);
        assert_eq!(counts, "1 2\n");
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn reapplying_after_a_later_frame_keeps_timestamps_monotonic() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old-timing.lua");
        let new_source = directory.path().join("new-timing.lua");
        let old_log = directory.path().join("old-timing");
        let new_log = directory.path().join("new-timing");
        let config = |log: &std::path::Path, timer: bool| {
            let timer = if timer {
                "sliver.timer.after(0.01, function() sliver.redraw() end)"
            } else {
                ""
            };
            format!(
                r#"
                local sliver = require("sliver.v1")
                local file_name = {log:?}
                {timer}
                return {{
                    api_version = 1,
                    render = function(_, time, delta)
                        local file = assert(io.open(file_name, "a"))
                        file:write(time, " ", delta, "\n")
                        file:close()
                    end,
                }}
                "#,
                log = log.to_string_lossy(),
                timer = timer,
            )
        };
        std::fs::write(&old_source, config(&old_log, true))?;
        std::fs::write(&new_source, config(&new_log, false))?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&old_source)?;
        std::thread::sleep(Duration::from_millis(40));
        let now = supervisor.now_seconds();
        supervisor.step_at(now)?;
        supervisor.apply(&new_source)?;

        let parse = |path: &std::path::Path| -> Result<Vec<[f64; 2]>> {
            std::fs::read_to_string(path)?
                .lines()
                .map(|line| {
                    let values: Vec<_> = line
                        .split_whitespace()
                        .map(|value| value.parse::<f64>())
                        .collect::<std::result::Result<_, _>>()?;
                    anyhow::ensure!(values.len() == 2, "frame timing line had the wrong shape");
                    Ok([values[0], values[1]])
                })
                .collect()
        };
        let old_frames = parse(&old_log)?;
        let new_frames = parse(&new_log)?;
        assert_eq!(old_frames.len(), 2);
        assert_eq!(new_frames.len(), 1);
        assert!(old_frames[1][0] >= old_frames[0][0]);
        assert!(new_frames[0][0] >= old_frames[1][0]);
        assert_eq!(new_frames[0][1], 0.0);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn slow_staging_does_not_charge_staging_time_to_the_next_frame_delta() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("slow-stage.lua");
        let log = directory.path().join("slow-stage-times");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                sliver.timer.after(0.01, function() sliver.redraw() end)
                return {{
                    api_version = 1,
                    start = function()
                        local deadline = os.clock() + 0.08
                        while os.clock() < deadline do end
                    end,
                    render = function(_, time, delta)
                        local file = assert(io.open(log, "a"))
                        file:write(time, " ", delta, "\n")
                        file:close()
                    end,
                }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        let committed_at = supervisor.now_seconds();
        let next_frame_at = committed_at + 0.05;
        supervisor.step_at(next_frame_at)?;

        let lines: Vec<_> = std::fs::read_to_string(log)?
            .lines()
            .map(|line| {
                line.split_whitespace()
                    .map(|value| value.parse::<f64>().expect("frame timing was numeric"))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][1], 0.0);
        assert!((lines[1][1] - 0.05).abs() < 0.02);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn non_droppable_touch_overflow_enters_fixed_recovery_without_growing_forever() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("queue.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, touch = function() end, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        for id in 0..=super::TOUCH_QUEUE_CAPACITY as u32 {
            supervisor
                .hardware_mut()
                .inject(HardwareEvent::Touch(overlap_touch(id, TouchPhase::Down)));
        }

        let error = supervisor
            .step_at(1.0)
            .expect_err("non-droppable input overflow was accepted");
        assert!(format!("{error:#}").contains("input queue overflow"));
        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_some());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn non_droppable_input_transition_overflow_enters_fixed_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("transition-queue.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        for index in 0..=super::TOUCH_QUEUE_CAPACITY {
            supervisor.hardware_mut().inject(HardwareEvent::Fn {
                active: index % 2 == 0,
            });
        }

        let error = supervisor
            .step_at(1.0)
            .expect_err("input transition overflow was accepted");
        assert!(format!("{error:#}").contains("transition queue overflow"));
        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_some());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn a_full_touch_queue_coalesces_the_latest_move_without_growing() -> Result<()> {
        let mut queue = super::TouchQueue::new();
        for id in 0..super::TOUCH_QUEUE_CAPACITY as u32 {
            queue.push(overlap_touch(id, TouchPhase::Move))?;
        }
        let mut latest = overlap_touch(0, TouchPhase::Move);
        latest.x = 999.0;
        queue.push(latest)?;
        let events = queue.drain();
        assert_eq!(events.len(), super::TOUCH_QUEUE_CAPACITY);
        assert_eq!(events.iter().find(|event| event.id == 0).unwrap().x, 999.0);
        Ok(())
    }

    #[test]
    fn repeated_moves_do_not_consume_queue_capacity_before_their_end() -> Result<()> {
        let mut queue = super::TouchQueue::new();
        queue.push(overlap_touch(1, TouchPhase::Down))?;
        for index in 0..=super::TOUCH_QUEUE_CAPACITY {
            let mut move_event = overlap_touch(1, TouchPhase::Move);
            move_event.x = index as f64;
            queue.push(move_event)?;
        }
        queue.push(overlap_touch(1, TouchPhase::Up))?;
        let events = queue.drain();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].phase, TouchPhase::Move);
        assert_eq!(events[1].x, super::TOUCH_QUEUE_CAPACITY as f64);
        assert_eq!(events[2].phase, TouchPhase::Up);
        Ok(())
    }

    #[test]
    fn excessive_synthetic_key_output_enters_fixed_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("key-queue.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        for _ = 1, 5000 do
                            sliver.input.key.tap(sliver.input.keys.keyboard.f1)
                        end
                    end
                end,
                render = function() end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));

        let error = supervisor
            .step_at(1.0)
            .expect_err("unbounded synthetic key output was accepted");
        assert!(format!("{error:#}").contains("key request limit exceeded"));
        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_some());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn static_worker_does_not_render_without_a_request() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("static.lua");
        let log = directory.path().join("renders");
        std::fs::write(
            &source,
            format!(
                r#"
                require("sliver.v1")
                local log = {log:?}
                return {{
                    api_version = 1,
                    render = function()
                        local file = assert(io.open(log, "a"))
                        file:write("render\n")
                        file:close()
                    end,
                }}
                "#,
                log = log.to_string_lossy()
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        assert_eq!(std::fs::read_to_string(&log)?, "render\n");

        let now = supervisor.now_seconds();
        supervisor.step_at(now + 1.0)?;
        supervisor.step_at(now + 2.0)?;
        assert_eq!(std::fs::read_to_string(&log)?, "render\n");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn replacement_cancels_old_contacts_and_ignores_until_up() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old-touch.lua");
        let new_source = directory.path().join("new-touch.lua");
        let old_log = directory.path().join("old-events");
        let new_log = directory.path().join("new-events");
        let config = |log: &std::path::Path| {
            format!(
                r#"
                require("sliver.v1")
                local log = {log:?}
                return {{
                    api_version = 1,
                    touch = function(event)
                        local file = assert(io.open(log, "a"))
                        file:write(event.phase, "\n")
                        file:close()
                    end,
                    render = function() end,
                }}
                "#,
                log = log.to_string_lossy()
            )
        };
        std::fs::write(&old_source, config(&old_log))?;
        std::fs::write(&new_source, config(&new_log))?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&old_source)?;

        let event = |phase| TouchEvent {
            phase,
            id: 9,
            time: 0.0,
            x: 1.0,
            y: 1.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        supervisor
            .hardware_mut()
            .inject_on_poll(4, HardwareEvent::Touch(event(TouchPhase::Down)));
        supervisor.apply(&new_source)?;

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(TouchPhase::Move)));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(TouchPhase::Up)));
        supervisor.step_at(2.0)?;
        assert!(!new_log.exists());

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(TouchPhase::Down)));
        supervisor.step_at(3.0)?;
        assert_eq!(std::fs::read_to_string(old_log)?, "down\ncancel\n");
        assert_eq!(std::fs::read_to_string(new_log)?, "down\n");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn staged_backlight_failure_restores_path_frame_and_level() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let old_source = directory.path().join("old.lua");
        let new_source = directory.path().join("new.lua");
        std::fs::write(
            &old_source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        std::fs::write(
            &new_source,
            "require('sliver.v1'); local sliver = require('sliver.v1'); return { api_version = 1, start = function() sliver.backlight.set(0.75) end, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1) end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(
            FailingPresentHardware::new(state_file.clone()),
            state_file.clone(),
        )?;
        supervisor.apply(&old_source)?;
        supervisor.hardware_mut().fail_next_backlight = true;

        let error = supervisor
            .apply(&new_source)
            .expect_err("staged backlight failure was accepted");

        assert!(format!("{error:#}").contains("injected backlight failure"));
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(supervisor.hardware().inner.backlight_level(), 0.0);
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .unwrap()
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn committed_backlight_failure_rolls_back_worker_and_hardware_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("backlight.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.backlight.set(0.75)
                        sliver.redraw()
                    end
                end,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor =
            Supervisor::new(FailingPresentHardware::new(state_file.clone()), state_file)?;
        supervisor.apply(&source)?;
        supervisor.hardware_mut().fail_next_backlight = true;
        let event = |phase| TouchEvent {
            phase,
            id: 1,
            time: 0.0,
            x: 1.0,
            y: 1.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };

        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(event(TouchPhase::Down)));
        let error = supervisor
            .step_at(1.0)
            .expect_err("committed backlight failure was accepted");
        assert!(format!("{error:#}").contains("injected backlight failure"));
        assert_eq!(supervisor.hardware().inner.backlight_level(), 0.0);
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .expect("previous frame was lost")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );

        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(event(TouchPhase::Up)));
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(event(TouchPhase::Down)));
        supervisor.step_at(2.0)?;
        assert_eq!(supervisor.hardware().inner.backlight_level(), 0.75);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn selected_path_normalization_preserves_the_final_symlink() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("target");
        std::fs::write(
            &target,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let symlink = directory.path().join("selected");
        std::os::unix::fs::symlink(&target, &symlink)?;
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested)?;
        let requested = nested.join("..").join("selected");
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;

        supervisor.apply(&requested)?;

        assert_eq!(
            std::fs::read(&state_file)?,
            symlink.as_os_str().as_encoded_bytes()
        );
        assert_ne!(
            std::fs::read(&state_file)?,
            target.as_os_str().as_encoded_bytes()
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn source_changes_wait_for_an_explicit_fresh_reapply() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("config");
        let state_file = directory.path().join("state/sliver/config-path");
        let config = |red: u8, blue: u8| {
            format!(
                "require('sliver.v1'); return {{ api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, {}, 0, {}, 1) end }}",
                f64::from(red) / 255.0,
                f64::from(blue) / 255.0,
            )
        };
        std::fs::write(&source, config(255, 0))?;
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;

        std::fs::write(&source, config(0, 255))?;

        assert_eq!(supervisor.hardware().presented_frames().len(), 1);
        assert_eq!(
            supervisor.hardware().presented_frames()[0].rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.apply(&source)?;
        assert_eq!(supervisor.hardware().presented_frames().len(), 2);
        assert_eq!(
            supervisor.hardware().presented_frames()[1].rgba_at(10, 10),
            [0, 0, 255, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn concurrent_apply_requests_are_processed_in_arrival_order() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let state_file = directory.path().join("state/sliver/config-path");
        let gate = directory.path().join("gate");
        let staging = directory.path().join("staging");
        let first = directory.path().join("first.lua");
        std::fs::write(
            &first,
            format!(
                r#"
                local staging = assert(io.open({staging:?}, "w"))
                staging:write("ready")
                staging:close()
                while true do
                    local gate = io.open({gate:?})
                    if gate then gate:close(); break end
                end
                require("sliver.v1")
                return {{
                    api_version = 1,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                    end,
                }}
                "#,
                staging = staging.to_string_lossy(),
                gate = gate.to_string_lossy(),
            ),
        )?;
        let second = directory.path().join("second.lua");
        std::fs::write(
            &second,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1) end }",
        )?;
        let invalid = directory.path().join("invalid.lua");
        std::fs::write(
            &invalid,
            "require('sliver.v1'); return { api_version = 1, render = function() error('queued failure') end }",
        )?;
        let (logind, _uid) = active_local_logind("seat-session");
        let server_logind = logind.clone();
        let server = thread::spawn(move || -> Result<Supervisor<FakeTouchBar, FakeLogind>> {
            let mut supervisor =
                Supervisor::new_with_logind(FakeTouchBar::new(), state_file, server_logind)?;
            for _ in 0..3 {
                let (mut stream, _) = listener.accept()?;
                serve_connection(&mut stream, &mut supervisor)?;
            }
            Ok(supervisor)
        });

        let first_socket = socket.clone();
        let first_path = first.clone();
        let first_client =
            thread::spawn(move || crate::apply_ipc::request_apply_at(&first_socket, &first_path));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !staging.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        anyhow::ensure!(staging.exists(), "first candidate never entered staging");
        let second_socket = socket.clone();
        let second_path = second.clone();
        let second_client =
            thread::spawn(move || crate::apply_ipc::request_apply_at(&second_socket, &second_path));
        let invalid_socket = socket.clone();
        let invalid_client =
            thread::spawn(move || crate::apply_ipc::request_apply_at(&invalid_socket, &invalid));
        thread::sleep(Duration::from_millis(50));
        std::fs::write(&gate, "go")?;

        first_client.join().expect("first apply client panicked")?;
        second_client
            .join()
            .expect("second apply client panicked")?;
        let invalid_error = invalid_client
            .join()
            .expect("invalid apply client panicked")
            .expect_err("invalid queued candidate was accepted");
        assert!(format!("{invalid_error:#}").contains("queued failure"));
        let supervisor = server.join().expect("supervisor server panicked")?;

        assert_eq!(supervisor.hardware().presented_frames().len(), 2);
        assert_eq!(
            supervisor.hardware().presented_frames()[0].rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        assert_eq!(
            supervisor.hardware().presented_frames()[1].rgba_at(10, 10),
            [0, 0, 255, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn active_local_session_can_reset_to_the_embedded_default_through_the_unix_request_path(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let old_source = directory.path().join("old.lua");
        std::fs::write(
            &old_source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("default-request-session");
        let mut supervisor =
            Supervisor::new_with_logind(FakeTouchBar::new(), state_file.clone(), logind)?;
        supervisor.apply(&old_source)?;

        let client_socket = socket.clone();
        let client = thread::spawn(move || crate::apply_ipc::request_default_at(&client_socket));
        let (mut stream, _) = listener.accept()?;
        serve_connection(&mut stream, &mut supervisor)?;
        client.join().expect("default client panicked")?;

        assert!(!state_file.exists());
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("default frame was not presented")
                .rgba_at(0, 0),
            [0, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn active_local_session_can_apply_through_the_unix_request_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let source = directory.path().join("config.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _uid) = active_local_logind("seat-session");
        let mut supervisor =
            Supervisor::new_with_logind(FakeTouchBar::new(), state_file.clone(), logind)?;
        let client_socket = socket.clone();
        let client_source = source.clone();
        let client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&client_socket, &client_source)
        });

        let (mut stream, _) = listener.accept()?;
        serve_connection(&mut stream, &mut supervisor)?;
        client.join().expect("apply client panicked")?;

        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(supervisor.hardware().presented_frames().len(), 1);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn session_switch_in_the_precommit_window_is_rejected() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let staged = directory.path().join("staged");
        let source = directory.path().join("config.lua");
        std::fs::write(
            &source,
            format!(
                r#"
                require("sliver.v1")
                return {{
                    api_version = 1,
                    start = function()
                        local marker = assert(io.open({staged:?}, "w"))
                        marker:close()
                    end,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                    end,
                }}
                "#,
                staged = staged.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, uid) = active_local_logind("old-session");
        logind.switch_active_on_generation_read(
            6,
            "seat0",
            ActiveSession {
                id: "new-session".into(),
                uid,
            },
        );
        let mut supervisor =
            Supervisor::new_with_logind(FakeTouchBar::new(), state_file.clone(), logind)?;
        let client_socket = socket.clone();
        let client_source = source.clone();
        let client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&client_socket, &client_source)
        });

        let (mut stream, _) = listener.accept()?;
        serve_connection(&mut stream, &mut supervisor)?;
        let error = client
            .join()
            .expect("apply client panicked")
            .expect_err("session switched during precommit but apply succeeded");
        assert!(staged.exists());
        assert!(format!("{error:#}").contains("session changed while checking authorization"));
        assert!(!state_file.exists());
        assert!(supervisor.hardware().presented_frames().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn fallback_rejects_a_session_that_activates_after_presentation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        let mut supervisor = Supervisor::new_fallback_with_logind(
            SessionSwitchingHardware::new(logind.clone(), uid),
            directory.path().join("state/config-path"),
            logind,
            Some(LuaSource::embedded(
                b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
            )),
        )?;
        supervisor.hardware_mut().switch_on_present = true;

        supervisor.start_fallback()?;

        assert!(!supervisor.has_active_worker());
        assert!(supervisor.has_recovery());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn session_change_after_candidate_presentation_rolls_back_selected_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let old_source = directory.path().join("old.lua");
        let new_source = directory.path().join("new.lua");
        std::fs::write(
            &old_source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        std::fs::write(
            &new_source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                start = function()
                    sliver.backlight.set(0.75)
                end,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1)
                end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, uid) = active_local_logind("old-session");
        let mut supervisor = Supervisor::new_with_logind(
            SessionSwitchingHardware::new(logind.clone(), uid),
            state_file.clone(),
            logind,
        )?;
        supervisor.apply(&old_source)?;
        supervisor.hardware_mut().switch_on_present = true;

        let client_socket = socket.clone();
        let client_source = new_source.clone();
        let client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&client_socket, &client_source)
        });
        let (mut stream, _) = listener.accept()?;
        serve_connection(&mut stream, &mut supervisor)?;

        let error = client
            .join()
            .expect("apply client panicked")
            .expect_err("session changed after presentation but apply succeeded");
        assert!(format!("{error:#}").contains("not the active session"));
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(supervisor.hardware().inner.backlight_level(), 0.0);
        assert_eq!(supervisor.hardware().inner.presented_frames().len(), 3);
        assert_eq!(
            supervisor.hardware().inner.presented_frames()[1].rgba_at(10, 10),
            [0, 0, 255, 255]
        );
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .expect("previous frame was not restored")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn session_change_after_key_release_restores_the_old_worker_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let old_source = directory.path().join("old.lua");
        let new_source = directory.path().join("new.lua");
        std::fs::write(
            &old_source,
            r#"
            local sliver = require("sliver.v1")
            local key = sliver.input.keys.keyboard.f2
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == "down" then
                        sliver.input.key.down(key)
                    elseif event.phase == "up" then
                        sliver.input.key.up(key)
                    end
                end,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        std::fs::write(
            &new_source,
            r#"
            local sliver = require("sliver.v1")
            return {
                api_version = 1,
                start = function()
                    sliver.backlight.set(0.75)
                end,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1)
                end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, uid) = active_local_logind("old-session");
        let mut supervisor = Supervisor::new_with_logind(
            SessionSwitchingHardware::new(logind.clone(), uid),
            state_file.clone(),
            logind,
        )?;
        supervisor.apply(&old_source)?;
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(1.0)?;
        supervisor.hardware_mut().switch_on_present = true;

        let client_socket = socket.clone();
        let client_source = new_source.clone();
        let client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&client_socket, &client_source)
        });
        let (mut stream, _) = listener.accept()?;
        serve_connection(&mut stream, &mut supervisor)?;
        let error = client
            .join()
            .expect("apply client panicked")
            .expect_err("session changed after key release but apply succeeded");
        assert!(format!("{error:#}").contains("not the active session"));
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .expect("old frame was not restored")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );

        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Up)));
        supervisor.step_at(2.0)?;

        assert_eq!(
            supervisor.hardware().inner.synthetic_transactions(),
            &[
                vec![FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                }],
                vec![FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                }],
                vec![FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                }],
                vec![FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                }],
            ]
        );
        assert_eq!(
            supervisor.hardware().inner.actions(),
            &[
                FakeAction::Grab,
                FakeAction::Present,
                FakeAction::SyntheticKey(FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                }),
                FakeAction::Backlight(0.75),
                FakeAction::Present,
                FakeAction::SyntheticKey(FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                }),
                FakeAction::Present,
                FakeAction::Backlight(0.0),
                FakeAction::SyntheticKey(FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: true,
                }),
                FakeAction::SyntheticKey(FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F2),
                    active: false,
                }),
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn final_authorization_failure_does_not_select_failed_path_without_worker() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let started = directory.path().join("started");
        let release = directory.path().join("release");
        let source = directory.path().join("config.lua");
        std::fs::write(
            &source,
            format!(
                r#"
                local marker = assert(io.open({started:?}, "w"))
                marker:close()
                while true do
                    local gate = io.open({release:?})
                    if gate then gate:close(); break end
                end
                require("sliver.v1")
                return {{ api_version = 1, render = function() end }}
                "#,
                started = started.to_string_lossy(),
                release = release.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, uid) = active_local_logind("old-session");
        let mut supervisor =
            Supervisor::new_with_logind(FakeTouchBar::new(), state_file.clone(), logind.clone())?;
        let client_socket = socket.clone();
        let client_source = source.clone();
        let client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&client_socket, &client_source)
        });

        let (mut stream, _) = listener.accept()?;
        let request = super::read_authorized_request(&mut stream, &supervisor.authorizer)?;
        let flip_logind = logind.clone();
        let flip_started = started.clone();
        let flip_release = release.clone();
        let flip = thread::spawn(move || {
            while !flip_started.exists() {
                thread::sleep(Duration::from_millis(1));
            }
            flip_logind.set_session(
                std::process::id() as libc::pid_t,
                Some(Session {
                    id: "old-session".into(),
                    uid,
                    seat: Some("seat0".into()),
                    remote: false,
                    active: false,
                }),
            );
            std::fs::write(flip_release, "continue").expect("release candidate staging");
        });
        let result = supervisor.apply_authorized(request);
        flip.join().expect("authorization flip panicked");
        crate::apply_ipc::write_reply(&mut stream, &result)?;
        let error = client
            .join()
            .expect("apply client panicked")
            .expect_err("inactive final authorization check was accepted");
        assert!(format!("{error:#}").contains("session is inactive"));
        assert!(!state_file.exists());
        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_none());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn failed_candidate_rechecks_authorization_before_preserving_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let started = directory.path().join("started");
        let release = directory.path().join("release");
        let source = directory.path().join("config.lua");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local started = {started:?}
                local release = {release:?}
                return {{
                    api_version = 1,
                    render = function()
                        local marker = assert(io.open(started, "w"))
                        marker:close()
                        while true do
                            local gate = io.open(release)
                            if gate then gate:close(); break end
                        end
                        error("candidate failed during staging")
                    end,
                }}
                "#,
                started = started.to_string_lossy(),
                release = release.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("old-session");
        let mut supervisor =
            Supervisor::new_with_logind(FakeTouchBar::new(), state_file.clone(), logind.clone())?;
        let client_socket = socket.clone();
        let client_source = source.clone();
        let client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&client_socket, &client_source)
        });

        let (mut stream, _) = listener.accept()?;
        let request = super::read_authorized_request(&mut stream, &supervisor.authorizer)?;
        let flip_logind = logind.clone();
        let flip_started = started.clone();
        let flip_release = release.clone();
        let flip = thread::spawn(move || {
            while !flip_started.exists() {
                thread::sleep(Duration::from_millis(1));
            }
            flip_logind.bump_generation();
            std::fs::write(flip_release, "continue").expect("release candidate staging");
        });
        let result = supervisor.apply_authorized(request);
        flip.join().expect("authorization flip panicked");
        crate::apply_ipc::write_reply(&mut stream, &result)?;
        let error = client
            .join()
            .expect("apply client panicked")
            .expect_err("failed candidate was accepted after its session changed");
        assert!(format!("{error:#}").contains("changed during config apply"));
        assert!(!state_file.exists());
        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_none());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn session_change_during_staging_cancels_candidate_before_commit() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let marker = directory.path().join("staging");
        let gate = directory.path().join("release");
        let source = directory.path().join("config.lua");
        std::fs::write(
            &source,
            format!(
                r#"
                local marker = assert(io.open({marker:?}, "w"))
                marker:close()
                while true do
                    local gate = io.open({gate:?})
                    if gate then gate:close(); break end
                end
                require("sliver.v1")
                return {{
                    api_version = 1,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                    end,
                }}
                "#,
                marker = marker.to_string_lossy(),
                gate = gate.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, uid) = active_local_logind("old-session");
        let server_logind = logind.clone();
        let server_socket = socket.clone();
        let server_state = state_file.clone();
        let server = thread::spawn(move || -> Result<Supervisor<FakeTouchBar, FakeLogind>> {
            let mut supervisor =
                Supervisor::new_with_logind(FakeTouchBar::new(), server_state, server_logind)?;
            let (mut stream, _) = listener.accept()?;
            serve_connection(&mut stream, &mut supervisor)?;
            Ok(supervisor)
        });
        let client_source = source.clone();
        let client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&server_socket, &client_source)
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        anyhow::ensure!(marker.exists(), "candidate never entered staging");
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "new-session".into(),
                uid,
            }),
        );
        std::fs::write(&gate, "continue")?;

        let error = client
            .join()
            .expect("apply client panicked")
            .expect_err("candidate committed after the active session changed");
        assert!(format!("{error:#}").contains("not the active session"));
        let supervisor = server.join().expect("supervisor thread panicked")?;
        assert!(!state_file.exists());
        assert!(supervisor.hardware().presented_frames().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn queued_apply_is_cancelled_after_a_session_changes_away_and_back() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let first_marker = directory.path().join("first-staging");
        let first_gate = directory.path().join("first-release");
        let second_marker = directory.path().join("second-staging");
        let first = directory.path().join("first.lua");
        std::fs::write(
            &first,
            format!(
                r#"
                local marker = assert(io.open({marker:?}, "w"))
                marker:close()
                while true do
                    local gate = io.open({gate:?})
                    if gate then gate:close(); break end
                end
                require("sliver.v1")
                return {{ api_version = 1, render = function() end }}
                "#,
                marker = first_marker.to_string_lossy(),
                gate = first_gate.to_string_lossy(),
            ),
        )?;
        let second = directory.path().join("second.lua");
        std::fs::write(
            &second,
            format!(
                r#"
                require("sliver.v1")
                return {{
                    api_version = 1,
                    start = function()
                        local marker = assert(io.open({marker:?}, "w"))
                        marker:close()
                    end,
                    render = function() end,
                }}
                "#,
                marker = second_marker.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, uid) = active_local_logind("old-session");
        let server_logind = logind.clone();
        let server_state = state_file.clone();
        let server = thread::spawn(move || -> Result<Supervisor<FakeTouchBar, FakeLogind>> {
            let mut supervisor =
                Supervisor::new_with_logind(FakeTouchBar::new(), server_state, server_logind)?;
            serve_for_test(listener, &mut supervisor, 2)?;
            Ok(supervisor)
        });
        let first_socket = socket.clone();
        let first_client_source = first.clone();
        let first_client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&first_socket, &first_client_source)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !first_marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        anyhow::ensure!(
            first_marker.exists(),
            "first candidate never entered staging"
        );

        let second_socket = socket.clone();
        let second_client_source = second.clone();
        let second_client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&second_socket, &second_client_source)
        });
        anyhow::ensure!(
            logind.wait_for_generation_reads(6, Duration::from_secs(2)),
            "second request was not authorized while the first candidate was staged"
        );
        assert!(
            !second_marker.exists(),
            "queued request started staging early"
        );

        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "new-session".into(),
                uid,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "old-session".into(),
                uid,
            }),
        );
        std::fs::write(&first_gate, "continue")?;

        let first_error = first_client
            .join()
            .expect("first apply client panicked")
            .expect_err("staged apply committed after session changed");
        let second_error = second_client
            .join()
            .expect("second apply client panicked")
            .expect_err("queued apply committed after session changed");
        assert!(format!("{first_error:#}").contains("changed during config apply"));
        assert!(format!("{second_error:#}").contains("changed during config apply"));
        assert!(!second_marker.exists());
        let supervisor = server.join().expect("supervisor thread panicked")?;
        assert!(!state_file.exists());
        assert!(supervisor.hardware().presented_frames().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn rapid_active_session_changes_do_not_leave_stale_apply_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let old_source = directory.path().join("old.lua");
        let new_source = directory.path().join("new.lua");
        let config = |red: f64, blue: f64| {
            format!(
                "require('sliver.v1'); return {{ api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, {red}, 0, {blue}, 1) end }}"
            )
        };
        std::fs::write(&old_source, config(1.0, 0.0))?;
        std::fs::write(&new_source, config(0.0, 1.0))?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, uid) = active_local_logind("old-session");
        let server_logind = logind.clone();
        let server_state = state_file.clone();
        let server = thread::spawn(move || -> Result<Supervisor<FakeTouchBar, FakeLogind>> {
            let mut supervisor =
                Supervisor::new_with_logind(FakeTouchBar::new(), server_state, server_logind)?;
            for _ in 0..3 {
                let (mut stream, _) = listener.accept()?;
                serve_connection(&mut stream, &mut supervisor)?;
            }
            Ok(supervisor)
        });
        let apply = |path: &std::path::Path| {
            let socket = socket.clone();
            let path = path.to_path_buf();
            thread::spawn(move || crate::apply_ipc::request_apply_at(&socket, &path))
                .join()
                .expect("apply client panicked")
        };
        apply(&old_source)?;
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "new-session".into(),
                uid,
            }),
        );
        let rejected = apply(&new_source).expect_err("inactive old session was accepted");
        assert!(
            format!("{rejected:#}").contains("not the active session"),
            "{rejected:#}"
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "old-session".into(),
                uid,
            }),
        );
        apply(&new_source)?;
        let supervisor = server.join().expect("supervisor thread panicked")?;
        assert_eq!(
            std::fs::read(&state_file)?,
            new_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(supervisor.hardware().presented_frames().len(), 2);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn absent_state_starts_the_canonical_embedded_default_without_selecting_a_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let supervisor =
            embedded_default_supervisor(state_file.clone(), "canonical-default-session")?;

        assert!(!state_file.exists());
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        let frame = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("canonical default did not present a frame");
        assert_eq!(frame.dimensions(), (2008, 60));
        assert!(frame_contains_rgb(frame, 0..2008, 0..60, [255, 255, 255]));
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn embedded_default_stays_healthy_through_systemd_worker_polling() -> Result<()> {
        let _systemd_tests = crate::lock_systemd_tests();
        let available = std::process::Command::new("systemd-run")
            .args(["--user", "--wait", "--quiet", "true"])
            .status();
        anyhow::ensure!(
            available.is_ok_and(|status| status.success()),
            "systemd user manager is required for this worker lifecycle test"
        );

        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("systemd-default-session");
        let mut supervisor = Supervisor::new_with_startup_candidate_systemd(
            FakeTouchBar::new(),
            state_file,
            logind,
            Some(LuaSource::embedded(default_source::bytes().to_vec())),
        )?;
        assert!(supervisor.has_active_worker());
        assert!(!supervisor.has_recovery());

        let deadline = Instant::now() + Duration::from_millis(2500);
        while Instant::now() < deadline {
            let now = supervisor.now_seconds();
            supervisor.step_at(now)?;
            thread::sleep(Duration::from_millis(20));
        }

        assert!(supervisor.has_active_worker());
        assert!(!supervisor.has_recovery());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn default_first_frame_style_and_normal_controls_cross_the_fake_touchbar() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = embedded_default_supervisor(state_file, "default-controls-session")?;
        let first = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("default did not present its first frame");
        assert_eq!(first.rgba_at(0, 0), [0, 0, 0, 255]);
        assert!(frame_contains_rgb(first, 0..2008, 0..60, [255, 255, 255]));
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 0.0,
                x: 91.0,
                y: 30.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(1.0)?;
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("pressed default frame was not presented")
                .rgba_at(91, 30),
            [56, 56, 56, 255]
        );
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Up,
                id: 1,
                time: 1.0,
                x: 91.0,
                y: 30.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(2.0)?;

        let touch = |id: u32, phase: TouchPhase, x: f64| TouchEvent {
            phase,
            id,
            time: 2.0,
            x,
            y: 30.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        for index in 0..11 {
            let x = (f64::from(index) + 0.5) * 2008.0 / 11.0;
            supervisor.hardware_mut().inject(HardwareEvent::Touch(touch(
                index as u32 + 2,
                TouchPhase::Down,
                x,
            )));
            supervisor.hardware_mut().inject(HardwareEvent::Touch(touch(
                index as u32 + 2,
                TouchPhase::Up,
                x,
            )));
        }
        supervisor.step_at(3.0)?;
        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::BrightnessDown),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::BrightnessDown),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::BrightnessUp),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::BrightnessUp),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::Previous),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::Previous),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::PlayPause),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::PlayPause),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::Next),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::Next),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::Mute),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::Mute),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::VolumeDown),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::VolumeDown),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::VolumeUp),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::VolumeUp),
                    active: false
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn default_clock_requests_a_new_frame_at_the_next_minute_boundary() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let prefix = r#"
            local real_date = os.date
            local minute = 0
            os.date = function(format)
                if format == "%S" then return "59" end
                if format == "%H:%M" then
                    minute = minute + 1
                    return string.format("00:%02d", minute)
                end
                return real_date(format)
            end
        "#;
        let source = format!(
            "{}{}",
            prefix,
            String::from_utf8(default_source::bytes().to_vec())?
        );
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("clock-session");
        let mut supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file,
            logind,
            Some(LuaSource::embedded(source.into_bytes())),
        )?;
        let before = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("clock default did not present")
            .clone();
        let committed = supervisor.now_seconds();
        supervisor.step_at(committed + 0.5)?;
        assert_eq!(supervisor.hardware().presented_frames().len(), 2);
        supervisor.step_at(committed + 1.1)?;
        let after = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("minute timer did not present");
        let width = 2008.0 / 11.0;
        assert!(region_changed(
            &before,
            after,
            (6.0 * width) as usize..(7.0 * width) as usize,
            0..60,
        ));
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn default_clock_refreshes_after_staging_crosses_a_minute_before_commit() -> Result<()> {
        let prefix = r#"
            local real_date = os.date
            local minute_crossed = false
            os.date = function(format)
                if format == "%S" then
                    return minute_crossed and "00" or "59"
                end
                if format == "%H:%M" then
                    return minute_crossed and "00:01" or "00:00"
                end
                return real_date(format)
            end
        "#;
        let original_start = "    start = function()\n        sliver.backlight.set(0.75)\n    end,";
        let delayed_start = r#"    start = function()
        local deadline = os.clock() + 0.05
        while os.clock() < deadline do end
        minute_crossed = true
        sliver.backlight.set(0.75)
    end,"#;
        let mut source = format!(
            "{}{}",
            prefix,
            String::from_utf8(default_source::bytes().to_vec())?
        );
        assert!(source.contains(original_start));
        source = source.replacen(original_start, delayed_start, 1);

        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("clock-staging-session");
        let mut supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file,
            logind,
            Some(LuaSource::embedded(source.into_bytes())),
        )?;
        let before = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("clock default did not present")
            .clone();
        let committed = supervisor.now_seconds();

        supervisor.step_at(committed)?;

        let after = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("clock refresh did not present");
        let width = 2008.0 / 11.0;
        assert!(
            region_changed(
                &before,
                after,
                (6.0 * width) as usize..(7.0 * width) as usize,
                0..60,
            ),
            "clock stayed on the staging-time minute after commit"
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn default_battery_reads_asahi_files_and_uses_the_required_color_precedence() -> Result<()> {
        let cases = [
            ("80", "Discharging", [255, 255, 255]),
            ("30", "Discharging", [255, 191, 0]),
            ("15", "Discharging", [255, 59, 48]),
            ("10", "Charging", [52, 199, 89]),
            ("not-a-capacity", "Charging", [255, 255, 255]),
            ("50", "not-a-status", [255, 255, 255]),
        ];
        for (capacity, status, color) in cases {
            let directory = tempfile::tempdir()?;
            let battery = directory.path().join("macsmc-battery");
            std::fs::create_dir(&battery)?;
            std::fs::write(battery.join("capacity"), format!("{capacity}\n"))?;
            std::fs::write(battery.join("status"), format!("{status}\n"))?;
            let state_file = directory.path().join("state/sliver/config-path");
            let (logind, _) = active_local_logind("battery-session");
            let supervisor = Supervisor::new_with_startup_candidate(
                FakeTouchBar::new(),
                state_file,
                logind,
                Some(LuaSource::embedded(default_bytes_with_battery_root(
                    &battery,
                ))),
            )?;
            let frame = supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("battery default did not present a frame");
            let cell = battery_cell();
            assert!(
                frame_contains_rgb(frame, cell, 40..60, color),
                "{capacity} / {status} did not use RGB {color:?}"
            );
            supervisor.shutdown()?;
        }
        Ok(())
    }

    #[test]
    fn default_battery_timer_refreshes_after_thirty_seconds() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let battery = directory.path().join("macsmc-battery");
        std::fs::create_dir(&battery)?;
        std::fs::write(battery.join("capacity"), "50\n")?;
        std::fs::write(battery.join("status"), "Discharging\n")?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("battery-timer-session");
        let mut supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file,
            logind,
            Some(LuaSource::embedded(default_bytes_with_battery_root(
                &battery,
            ))),
        )?;
        let committed = supervisor.now_seconds();
        std::fs::write(battery.join("capacity"), "15\n")?;
        supervisor.step_at(committed + 29.0)?;
        let before = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("battery frame disappeared")
            .clone();
        assert!(!frame_contains_rgb(
            &before,
            battery_cell(),
            40..60,
            [255, 59, 48]
        ));
        supervisor.step_at(committed + 31.0)?;
        let after = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("battery timer did not present a frame");
        assert!(frame_contains_rgb(
            after,
            battery_cell(),
            40..60,
            [255, 59, 48]
        ));
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn default_touch_contacts_highlight_cancel_activate_and_remain_independent() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = embedded_default_supervisor(state_file, "default-touch-session")?;
        let event = |id, phase, x| TouchEvent {
            phase,
            id,
            time: 0.0,
            x,
            y: 30.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        };
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(1, TouchPhase::Down, 91.0)));
        supervisor.step_at(1.0)?;
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("down did not present highlight")
                .rgba_at(91, 30),
            [56, 56, 56, 255]
        );
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(1, TouchPhase::Move, 300.0)));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(1, TouchPhase::Up, 300.0)));
        supervisor.step_at(2.0)?;
        assert!(supervisor.hardware().synthetic_keys().is_empty());
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("cancel did not repaint")
                .rgba_at(91, 30),
            [0, 0, 0, 255]
        );

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(1, TouchPhase::Down, 91.0)));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(2, TouchPhase::Down, 273.0)));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(1, TouchPhase::Up, 91.0)));
        supervisor.step_at(3.0)?;
        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: false
                },
            ]
        );
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("second contact highlight was lost")
                .rgba_at(273, 50),
            [56, 56, 56, 255]
        );
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(event(2, TouchPhase::Up, 273.0)));
        supervisor.step_at(4.0)?;
        assert_eq!(
            supervisor.hardware().synthetic_keys().len(),
            4,
            "the second same-contact up did not activate its control"
        );
        assert_eq!(
            &supervisor.hardware().synthetic_keys()[2..],
            &[
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::BrightnessDown),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Consumer(ConsumerKey::BrightnessDown),
                    active: false
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn default_fn_down_immediately_selects_the_lua_function_layer() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = embedded_default_supervisor(state_file, "default-fn-session")?;
        let before = supervisor.hardware().presented_frames().len();
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            before + 1,
            "Fn down did not repaint the Lua layer immediately"
        );
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 1.0,
                x: 83.0,
                y: 30.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Up,
                id: 1,
                time: 1.0,
                x: 83.0,
                y: 30.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(2.0)?;
        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: false
                },
            ]
        );
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.step_at(3.0)?;
        assert!(supervisor.hardware().presented_frames().len() >= 3);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn embedded_source_has_the_ordinary_runtime_without_source_metadata() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let marker = directory.path().join("embedded-metadata");
        let source = format!(
            r#"
            local sliver = require("sliver.v1")
            local info = debug.getinfo(1, "S")
            local file = assert(io.open({marker:?}, "w"))
            file:write(info.source, "|", tostring(sliver.source), "|", tostring(sliver["is" .. "_default"]), "|", type(io.open), "|", type(os.date))
            file:close()
            return {{ api_version = 1, render = function() end }}
            "#,
            marker = marker.to_string_lossy(),
        );
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("embedded-metadata-session");
        let supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file,
            logind,
            Some(LuaSource::embedded(source.into_bytes())),
        )?;

        let metadata = std::fs::read_to_string(marker)?;
        assert_eq!(metadata, "=sliver|nil|nil|function|function");
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn explicit_source_exposes_its_path_and_directory_metadata() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("config.lua");
        let marker = directory.path().join("source-metadata");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local file = assert(io.open({marker:?}, "w"))
                file:write(sliver.source.path, "|", sliver.source.directory)
                file:close()
                return {{ api_version = 1, render = function() end }}
                "#,
                marker = marker.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;

        assert_eq!(
            std::fs::read_to_string(marker)?,
            format!("{}|{}", source.display(), directory.path().display())
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn failed_default_reset_keeps_the_previous_worker_and_selected_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let old_source = directory.path().join("old.lua");
        std::fs::write(
            &old_source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;
        supervisor.apply(&old_source)?;
        supervisor.set_default_source_for_test(
            b"require('sliver.v1'); error('embedded reset failed')".to_vec(),
        );

        let error = supervisor
            .apply_default()
            .expect_err("failed embedded reset was committed");

        assert!(format!("{error:#}").contains("embedded reset failed"));
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("previous worker frame disappeared")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn failed_default_reset_during_presentation_rolls_back_without_clearing_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let old_source = directory.path().join("old.lua");
        std::fs::write(
            &old_source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let mut supervisor = Supervisor::new(
            FailingPresentHardware::new(state_file.clone()),
            state_file.clone(),
        )?;
        supervisor.apply(&old_source)?;
        supervisor.set_default_source_for_test(default_source::bytes().to_vec());
        supervisor.hardware_mut().fail_next_present = true;

        let error = supervisor
            .apply_default()
            .expect_err("presentation failure committed embedded reset");

        assert!(format!("{error:#}").contains("injected presentation failure"));
        assert_eq!(
            std::fs::read(&state_file)?,
            old_source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .expect("old frame disappeared")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn changed_embedded_bytes_wait_for_the_next_default_worker_start() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor =
            embedded_default_supervisor(state_file.clone(), "default-upgrade-session")?;
        let original = supervisor
            .hardware()
            .presented_frames()
            .last()
            .expect("old default did not present")
            .rgba_at(0, 0);
        let changed = String::from_utf8(default_source::bytes().to_vec())?
            .replacen("#000000", "#010203", 1)
            .into_bytes();
        supervisor.set_default_source_for_test(changed);

        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("running default disappeared")
                .rgba_at(0, 0),
            original
        );
        supervisor.apply_default()?;
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("new default did not present")
                .rgba_at(0, 0),
            [1, 2, 3, 255]
        );
        assert!(!state_file.exists());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn failed_default_reset_without_a_worker_enters_recovery_without_selecting_a_path() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("failed-default-reset-session");
        let mut supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            logind,
            Some(LuaSource::embedded(
                b"require('sliver.v1'); return { api_version = 1, render = function() error('bad default') end }".to_vec(),
            )),
        )?;
        assert!(!state_file.exists());

        let error = supervisor
            .apply_default()
            .expect_err("failed default reset without a worker was accepted");

        assert!(format!("{error:#}").contains("bad default"));
        assert!(!state_file.exists());
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        assert!(!supervisor.hardware().presented_frames().is_empty());
        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_some());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn saved_startup_failure_keeps_path_and_starts_the_default() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let missing = directory.path().join("missing.lua");
        PreparedPathState::prepare(&state_file, &missing)?.commit()?;
        let (logind, _) = active_local_logind("startup-session");
        let supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            logind,
            None,
        )?;

        assert_eq!(
            std::fs::read(&state_file)?,
            missing.as_os_str().as_encoded_bytes()
        );
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        assert!(!supervisor.hardware().presented_frames().is_empty());
        assert!(supervisor.active.is_some());
        assert!(supervisor.recovery.is_none());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn rejected_default_reset_after_saved_startup_failure_restores_default_frame() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let state_file = directory.path().join("state/sliver/config-path");
        let missing = directory.path().join("missing.lua");
        PreparedPathState::prepare(&state_file, &missing)?.commit()?;
        let (logind, uid) = active_local_logind("saved-startup-session");
        let mut supervisor = Supervisor::new_with_startup_candidate(
            SessionSwitchingHardware::new(logind.clone(), uid),
            state_file.clone(),
            logind,
            Some(LuaSource::embedded(default_source::bytes().to_vec())),
        )?;
        assert!(supervisor.active.is_some());
        assert!(supervisor.recovery.is_none());
        let default_frame = supervisor
            .hardware()
            .inner
            .presented_frames()
            .last()
            .cloned()
            .expect("saved startup failure did not present recovery");
        supervisor.hardware_mut().switch_on_present = true;

        let client_socket = socket.clone();
        let client = thread::spawn(move || crate::apply_ipc::request_default_at(&client_socket));
        let (mut stream, _) = listener.accept()?;
        serve_connection(&mut stream, &mut supervisor)?;

        let error = client
            .join()
            .expect("default client panicked")
            .expect_err("session-changed default reset was accepted");
        assert!(format!("{error:#}").contains("not the active session"));
        assert_eq!(
            std::fs::read(&state_file)?,
            missing.as_os_str().as_encoded_bytes()
        );
        assert!(supervisor.active.is_some());
        assert!(supervisor.recovery.is_none());
        assert_eq!(
            supervisor
                .hardware()
                .inner
                .presented_frames()
                .last()
                .expect("default frame disappeared after rejected reset"),
            &default_frame
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn all_saved_startup_failures_start_the_default_without_resetting_path() -> Result<()> {
        let cases = [
            ("missing", None),
            ("invalid", Some("return {}")),
            (
                "start",
                Some("require('sliver.v1'); return { api_version = 1, start = function() error('start failed') end, render = function() end }"),
            ),
            (
                "render",
                Some("require('sliver.v1'); return { api_version = 1, render = function() error('render failed') end }"),
            ),
        ];
        for (name, contents) in cases {
            let directory = tempfile::tempdir()?;
            let state_file = directory.path().join("state/sliver/config-path");
            let source = directory.path().join(format!("{name}.lua"));
            if let Some(contents) = contents {
                std::fs::write(&source, contents)?;
            }
            PreparedPathState::prepare(&state_file, &source)?.commit()?;
            let (logind, _) = active_local_logind(name);
            let supervisor = Supervisor::new_with_startup_candidate(
                FakeTouchBar::new(),
                state_file.clone(),
                logind,
                None,
            )?;
            assert_eq!(
                std::fs::read(&state_file)?,
                source.as_os_str().as_encoded_bytes()
            );
            assert_eq!(supervisor.hardware().backlight_level(), 0.75);
            assert!(!supervisor.hardware().presented_frames().is_empty());
            assert!(supervisor.active.is_some());
            assert!(supervisor.recovery.is_none());
            supervisor.shutdown()?;
        }
        Ok(())
    }

    #[test]
    fn absent_path_uses_one_injected_default_without_persisting_it() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let default = directory.path().join("default.lua");
        std::fs::write(
            &default,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }",
        )?;
        let (logind, _) = active_local_logind("default-session");
        let supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            logind,
            Some(LuaSource::file(default)),
        )?;

        assert!(!state_file.exists());
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("default was not presented")
                .rgba_at(10, 10),
            [0, 255, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn injected_default_failure_enters_recovery_without_selecting_a_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let (logind, _) = active_local_logind("failed-default-session");
        let supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            logind,
            Some(LuaSource::embedded(
                b"require('sliver.v1'); error('embedded default failed')".to_vec(),
            )),
        )?;

        assert!(!state_file.exists());
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        assert!(!supervisor.hardware().presented_frames().is_empty());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn saved_path_wins_over_an_injected_default() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let saved = directory.path().join("saved.lua");
        let default = directory.path().join("default.lua");
        std::fs::write(
            &saved,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        std::fs::write(
            &default,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1) end }",
        )?;
        PreparedPathState::prepare(&state_file, &saved)?.commit()?;
        let (logind, _) = active_local_logind("saved-over-default-session");
        let supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            logind,
            Some(LuaSource::file(default)),
        )?;

        assert_eq!(
            std::fs::read(&state_file)?,
            saved.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("saved source was not presented")
                .rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn successful_live_apply_exits_failed_worker_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let missing = directory.path().join("missing.lua");
        let healthy = directory.path().join("healthy.lua");
        std::fs::write(
            &healthy,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;
        assert!(supervisor.apply(&missing).is_err());
        assert!(supervisor.recovery.is_some());

        supervisor.apply(&healthy)?;

        assert!(supervisor.active.is_some());
        assert!(supervisor.recovery.is_none());
        assert_eq!(
            std::fs::read(&state_file)?,
            healthy.as_os_str().as_encoded_bytes()
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn service_restart_starts_the_default_after_saved_source_failure_without_resetting_path(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let saved = directory.path().join("saved.lua");
        let default = LuaSource::embedded(
            b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
        );
        PreparedPathState::prepare(&state_file, &saved)?.commit()?;
        let (first_logind, _) = active_local_logind("failed-restart-session");
        let supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            first_logind,
            Some(default.clone()),
        )?;

        assert!(supervisor.active.is_some());
        assert!(supervisor.recovery.is_none());
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("default worker did not present a frame")
                .rgba_at(10, 10),
            [0, 255, 0, 255]
        );
        assert_eq!(
            std::fs::read(&state_file)?,
            saved.as_os_str().as_encoded_bytes()
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn service_restart_retries_the_saved_source_once() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let saved = directory.path().join("saved.lua");
        PreparedPathState::prepare(&state_file, &saved)?.commit()?;
        let (first_logind, _) = active_local_logind("first-restart-session");
        let first = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            first_logind,
            None,
        )?;
        assert!(first.active.is_some());
        assert!(first.recovery.is_none());
        first.shutdown()?;

        std::fs::write(
            &saved,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let (second_logind, _) = active_local_logind("second-restart-session");
        let second = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file.clone(),
            second_logind,
            None,
        )?;

        assert!(second.active.is_some());
        assert!(second.recovery.is_none());
        assert_eq!(
            std::fs::read(&state_file)?,
            saved.as_os_str().as_encoded_bytes()
        );
        second.shutdown()?;
        Ok(())
    }

    #[test]
    fn live_hung_worker_enters_fixed_recovery_without_resetting_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let source = directory.path().join("hung.lua");
        std::fs::write(
            &source,
            r#"
            local sliver = require("sliver.v1")
            sliver.timer.after(0.1, function()
                while true do end
            end)
            return {
                api_version = 1,
                render = function() end,
            }
            "#,
        )?;
        let mut supervisor = Supervisor::new_with_logind_process(
            FakeTouchBar::new(),
            state_file.clone(),
            FakeLogind::new(),
        )?;
        supervisor.apply(&source)?;

        let error = supervisor
            .step_at(1.0)
            .expect_err("a hung live callback was not fenced");
        assert!(error.to_string().contains("two seconds"));
        assert!(supervisor.active.is_none());
        assert!(supervisor
            .recovery
            .as_ref()
            .is_some_and(|recovery| !recovery.owner_is_healthy()));
        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn persisted_hung_source_is_attempted_once_before_default_start() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let source = directory.path().join("hung.lua");
        let attempts = directory.path().join("attempts");
        std::fs::write(
            &source,
            format!(
                r#"
                local attempts = assert(io.open({attempts:?}, "a"))
                attempts:write("loaded\n")
                attempts:close()
                require("sliver.v1")
                return {{
                    api_version = 1,
                    render = function()
                        while true do end
                    end,
                }}
                "#,
                attempts = attempts.to_string_lossy(),
            ),
        )?;
        PreparedPathState::prepare(&state_file, &source)?.commit()?;
        let (logind, _) = active_local_logind("persisted-hung-session");
        let supervisor = Supervisor::new_with_startup_candidate_process(
            FakeTouchBar::new(),
            state_file.clone(),
            logind,
            None,
        )?;

        assert!(supervisor.has_active_worker());
        assert!(!supervisor.has_recovery());
        assert_eq!(std::fs::read_to_string(&attempts)?, "loaded\n");
        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn persisted_hung_source_releases_its_systemd_cgroup_before_default_start() -> Result<()> {
        let _systemd_tests = crate::lock_systemd_tests();
        let available = std::process::Command::new("systemd-run")
            .args(["--user", "--wait", "--quiet", "true"])
            .status();
        anyhow::ensure!(
            available.is_ok_and(|status| status.success()),
            "systemd user manager is required for this worker lifecycle test"
        );

        let directory = tempfile::tempdir()?;
        let attempts = directory.path().join("attempts");
        let descendant_marker = directory.path().join("descendant");
        let helper = directory.path().join("descendant.sh");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s\n' \"$$\" > {}\ncat /proc/$$/cgroup >> {}\nexec sleep 30\n",
                descendant_marker.display(),
                descendant_marker.display(),
            ),
        )?;
        let mut permissions = std::fs::metadata(&helper)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&helper, permissions)?;
        let source = directory.path().join("hung.lua");
        std::fs::write(
            &source,
            format!(
                r#"
                local attempts = assert(io.open({attempts:?}, "a"))
                attempts:write("loaded\n")
                attempts:close()
                local sliver = require("sliver.v1")
                assert(os.execute({helper:?} .. " >/dev/null 2>&1 &"))
                return {{
                    api_version = 1,
                    render = function()
                        while true do end
                    end,
                }}
                "#,
                attempts = attempts.to_string_lossy(),
                helper = helper.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        PreparedPathState::prepare(&state_file, &source)?.commit()?;
        let (logind, _) = active_local_logind("systemd-persisted-hung-session");
        let supervisor = Supervisor::new_with_startup_candidate_systemd(
            FakeTouchBar::new(),
            state_file.clone(),
            logind,
            None,
        )?;

        assert!(supervisor.has_active_worker());
        assert!(!supervisor.has_recovery());
        assert_eq!(std::fs::read_to_string(&attempts)?, "loaded\n");
        let descendant_contents = std::fs::read_to_string(&descendant_marker)?;
        let mut descendant_lines = descendant_contents.lines();
        let descendant = descendant_lines.next().context("missing descendant PID")?;
        let cgroup_line = descendant_lines
            .find(|line| line.starts_with("0::"))
            .context("missing descendant cgroup")?;
        let cgroup = std::path::PathBuf::from("/sys/fs/cgroup").join(
            cgroup_line
                .trim_start_matches("0::")
                .trim_start_matches('/'),
        );
        let descendant_proc = std::path::PathBuf::from("/proc").join(descendant);
        let deadline = Instant::now() + Duration::from_secs(2);
        while descendant_proc.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !descendant_proc.exists(),
            "worker child {descendant} survived cleanup"
        );
        if cgroup.exists() {
            assert!(
                std::fs::read_to_string(cgroup.join("cgroup.procs"))
                    .map(|processes| processes.trim().is_empty())
                    .unwrap_or(true),
                "worker cgroup still contains a process"
            );
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let units = loop {
            let units = std::process::Command::new("systemctl")
                .args(["--user", "list-units", "--all", "--no-legend", "--plain"])
                .output()?;
            anyhow::ensure!(units.status.success(), "listing user worker units failed");
            let units = String::from_utf8(units.stdout)?;
            if units
                .lines()
                .any(|line| line.starts_with("sliver-lua-worker-"))
                || Instant::now() >= deadline
            {
                break units;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let worker_units: Vec<_> = units
            .lines()
            .filter(|line| line.starts_with("sliver-lua-worker-"))
            .collect();
        assert_eq!(
            worker_units.len(),
            1,
            "healthy default worker unit was not started"
        );
        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn repeating_timer_keeps_rendering_after_recovery_return() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("timed.lua");
        let log = directory.path().join("renders");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                sliver.timer.every(1.0, function() sliver.redraw() end)
                return {{
                    api_version = 1,
                    render = function() record("render") end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.step_at(3.0)?;
        assert!(supervisor.recovery.is_some());
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.step_at(4.0)?;
        assert!(supervisor.recovery.is_none());
        let renders_after_return = std::fs::read_to_string(&log)?.lines().count();

        supervisor.step_at(6.0)?;

        assert_eq!(
            std::fs::read_to_string(&log)?.lines().count(),
            renders_after_return + 1
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn healthy_worker_enters_recovery_at_exactly_two_seconds_and_returns_after_fn_up() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("healthy.lua");
        let log = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local log = {log:?}
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                local sliver = require("sliver.v1")
                return {{
                    api_version = 1,
                    visibility = function(event)
                        record("visibility:" .. tostring(event.visible))
                        if event.visible then sliver.redraw() end
                    end,
                    touch = function(event) record("touch:" .. event.phase) end,
                    key = function(event) record("key:" .. event.key .. ":" .. event.phase) end,
                    render = function() record("render") end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(2.0)?;
        supervisor.step_at(2.999)?;
        assert!(supervisor.recovery.is_none());
        supervisor.step_at(3.0)?;
        assert!(supervisor.recovery.is_some());
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(3.1)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.step_at(4.0)?;

        let events = std::fs::read_to_string(&log)?;
        assert!(events.contains("touch:down"));
        assert!(events.contains("touch:cancel"));
        assert!(events.contains("visibility:false"));
        assert!(events.contains("key:fn:up"));
        assert!(events.contains("visibility:true"));
        assert_eq!(
            events.lines().filter(|line| *line == "touch:down").count(),
            1
        );
        let lines: Vec<_> = events.lines().collect();
        let fn_up = lines
            .iter()
            .position(|line| *line == "key:fn:up")
            .expect("Fn-up was not delivered");
        let visible = lines
            .iter()
            .position(|line| *line == "visibility:true")
            .expect("visibility restoration was not delivered");
        let render = lines
            .iter()
            .rposition(|line| *line == "render")
            .expect("return render was not requested");
        assert!(fn_up < visible && visible < render);
        supervisor.step_at(6.0)?;
        assert_eq!(
            std::fs::read_to_string(&log)?
                .lines()
                .filter(|line| *line == "render")
                .count(),
            2
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn applying_during_healthy_recovery_keeps_fixed_row_until_fn_up() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let initial_source = directory.path().join("initial.lua");
        let candidate_source = directory.path().join("candidate.lua");
        let candidate_log = directory.path().join("candidate-events");
        std::fs::write(
            &initial_source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        std::fs::write(
            &candidate_source,
            format!(
                r#"
                local log = {log:?}
                local sliver = require("sliver.v1")
                return {{
                    api_version = 1,
                    visibility = function(event)
                        local file = assert(io.open(log, "a"))
                        file:write("visibility:", tostring(event.visible), "\n")
                        file:close()
                    end,
                    touch = function(event)
                        local file = assert(io.open(log, "a"))
                        file:write("touch:", event.phase, "\n")
                        file:close()
                    end,
                    render = function(canvas)
                        canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1)
                    end,
                }}
                "#,
                log = candidate_log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&initial_source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor.step_at(3.0)?;

        supervisor.apply(&candidate_source)?;
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("recovery row was not presented")
                .rgba_at(10, 10),
            [0, 0, 0, 255]
        );

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Up)));
        supervisor.step_at(4.1)?;
        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: false,
                },
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&candidate_log)?,
            "visibility:false\n"
        );

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor.step_at(5.0)?;
        assert_eq!(
            std::fs::read_to_string(&candidate_log)?,
            "visibility:false\nvisibility:true\n"
        );
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("candidate frame was not presented after Fn-up")
                .rgba_at(10, 10),
            [0, 255, 0, 255]
        );

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(2, TouchPhase::Down)));
        supervisor.step_at(6.0)?;
        assert_eq!(
            std::fs::read_to_string(&candidate_log)?,
            "visibility:false\nvisibility:true\ntouch:down\n"
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn late_batch_claims_recovery_before_fn_up_and_restores_for_later_touch() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("healthy.lua");
        let log = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                return {{
                    api_version = 1,
                    visibility = function(event)
                        record("visibility:" .. tostring(event.visible))
                    end,
                    key = function(event)
                        record("key:" .. event.key .. ":" .. event.phase)
                    end,
                    touch = function(event)
                        record("touch:" .. event.phase)
                    end,
                    render = function() end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: false });
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(3.0)?;

        let log_contents = std::fs::read_to_string(&log)?;
        let events: Vec<_> = log_contents.lines().collect();
        let hidden = events
            .iter()
            .position(|line| *line == "visibility:false")
            .expect("late batch did not claim recovery");
        let fn_up = events
            .iter()
            .position(|line| *line == "key:fn:up")
            .expect("Fn-up was not delivered");
        let visible = events
            .iter()
            .position(|line| *line == "visibility:true")
            .expect("healthy recovery did not exit");
        let touch = events
            .iter()
            .position(|line| *line == "touch:down")
            .expect("later batch touch was not routed");
        assert!(hidden < fn_up && fn_up < visible && visible < touch);
        assert!(supervisor.recovery.is_none());
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn an_idle_worker_exit_enters_recovery_on_the_next_poll() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("healthy.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function() end }",
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor.next_worker_deadline = None;
        supervisor
            .active
            .as_mut()
            .expect("candidate was not made active")
            .worker
            .exit_owner_for_test()?;

        supervisor.step_at(1.0)?;

        assert!(supervisor.active.is_none());
        assert!(supervisor.recovery.is_some());
        assert_eq!(supervisor.hardware().backlight_level(), 0.75);
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn deadline_poll_takes_recovery_ownership_before_routing_touch() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("healthy.lua");
        let log = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local log = {log:?}
                local sliver = require("sliver.v1")
                return {{
                    api_version = 1,
                    touch = function(event)
                        local file = assert(io.open(log, "a"))
                        file:write(event.phase, "\n")
                        file:close()
                    end,
                    render = function() end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Fn { active: true });
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(3.0)?;

        assert!(!log.exists() || !std::fs::read_to_string(&log)?.contains("down"));
        assert_eq!(supervisor.recovery.as_ref().map(|_| true), Some(true));
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn startup_recovery_bridges_modifiers_from_the_claim_snapshot() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let missing = directory.path().join("missing.lua");
        PreparedPathState::prepare(&state_file, &missing)?.commit()?;
        let mut input_state = InputState::default();
        input_state.modifiers.set(Modifier::LeftCtrl, true);
        let (logind, _) = active_local_logind("startup-recovery-session");
        let mut supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::with_input_state(input_state),
            state_file,
            logind,
            None,
        )?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Up)));
        supervisor.step_at(2.0)?;

        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: true,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::Escape),
                    active: false,
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false,
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn recovery_key_waits_for_finger_up_and_bridges_physical_modifiers() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let missing = directory.path().join("missing.lua");
        PreparedPathState::prepare(&state_file, &missing)?.commit()?;
        let (logind, _) = active_local_logind("recovery-session");
        let mut supervisor = Supervisor::new_with_startup_candidate(
            FakeTouchBar::new(),
            state_file,
            logind,
            Some(LuaSource::embedded(
                b"require('sliver.v1'); error('embedded default failed')".to_vec(),
            )),
        )?;
        supervisor.hardware_mut().inject(HardwareEvent::Modifier {
            modifier: Modifier::LeftCtrl,
            active: true,
        });
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Down,
                id: 1,
                time: 1.0,
                x: 10.0,
                y: 30.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(2.0)?;
        assert!(supervisor.hardware().synthetic_keys().is_empty());
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("pressed recovery frame was not presented")
                .rgba_at(10, 30),
            [56, 56, 56, 255]
        );
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Up,
                id: 1,
                time: 2.0,
                x: 10.0,
                y: 30.0,
                modifiers: ModifierState::default(),
                pressure: None,
                width: None,
                height: None,
            }));
        supervisor.step_at(3.0)?;
        assert_eq!(
            supervisor
                .hardware()
                .presented_frames()
                .last()
                .expect("released recovery frame was not presented")
                .rgba_at(10, 30),
            [0, 0, 0, 255]
        );
        assert_eq!(
            supervisor.hardware().synthetic_keys(),
            &[
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: true
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::F1),
                    active: false
                },
                FakeKeyEvent {
                    key: FakeKey::Keyboard(KeyboardKey::LeftCtrl),
                    active: false
                },
            ]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn successful_candidate_commits_frame_and_selected_path() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("config.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return {
                api_version = 1,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file.clone())?;

        supervisor.apply(&source)?;

        let frame = supervisor
            .hardware()
            .presented_frames()
            .last()
            .context("supervisor did not present candidate frame")?;
        assert_eq!(frame.rgba_at(10, 10), [255, 0, 0, 255]);
        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            std::fs::metadata(&state_file)?.permissions().mode() & 0o777,
            0o600
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn suspend_pauses_a_healthy_worker_and_resumes_its_frame_and_brightness() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("suspend.lua");
        let log = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                sliver.backlight.set(0.4)
                sliver.timer.every(10, function()
                    record("timer")
                    sliver.redraw()
                end)
                return {{
                    api_version = 1,
                    visibility = function(event) record("visibility:" .. tostring(event.visible)) end,
                    touch = function(event) record("touch:" .. event.phase) end,
                    render = function() record("render") end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        let frames_before_suspend = supervisor.hardware().presented_frames().len();
        assert_eq!(supervisor.hardware().backlight_level(), 0.4);

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(1.0)?;
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Visibility { visible: false });
        supervisor.step_at(2.0)?;
        let events_during_suspend = std::fs::read_to_string(&log)?;
        assert!(events_during_suspend.contains("touch:cancel"));
        assert!(events_during_suspend.contains("visibility:false"));
        assert_eq!(supervisor.hardware().backlight_level(), 0.0);

        supervisor.step_at(100.0)?;
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            frames_before_suspend
        );
        assert!(!std::fs::read_to_string(&log)?.contains("timer"));

        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Visibility { visible: true });
        supervisor.step_at(101.0)?;
        assert_eq!(supervisor.hardware().reacquire_calls(), 1);
        assert_eq!(supervisor.hardware().backlight_level(), 0.4);
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            frames_before_suspend + 1
        );
        assert!(std::fs::read_to_string(&log)?.contains("visibility:true"));
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn late_hardware_discovery_reclaims_before_the_first_apply() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("late.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return { api_version = 1, render = function() end }
            "#,
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::unavailable(), state_file)?;
        assert!(!supervisor.is_hardware_available());
        supervisor.poll(Duration::ZERO)?;
        assert!(!supervisor.is_hardware_available());
        supervisor.hardware_mut().make_available();
        supervisor.poll(Duration::ZERO)?;
        assert!(supervisor.is_hardware_available());
        supervisor.apply(&source)?;
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn device_loss_hides_without_rendering_and_recovery_accepts_applies_again() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("device.lua");
        let log = directory.path().join("events");
        std::fs::write(
            &source,
            format!(
                r#"
                local sliver = require("sliver.v1")
                local log = {log:?}
                local function record(value)
                    local file = assert(io.open(log, "a"))
                    file:write(value, "\n")
                    file:close()
                end
                sliver.timer.every(1, function()
                    record("timer")
                    sliver.redraw()
                end)
                return {{
                    api_version = 1,
                    visibility = function(event) record("visibility:" .. tostring(event.visible) .. ":" .. event.reason) end,
                    touch = function(event) record("touch:" .. event.phase) end,
                    render = function() record("render") end,
                }}
                "#,
                log = log.to_string_lossy(),
            ),
        )?;
        let state_file = directory.path().join("state/sliver/config-path");
        let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
        supervisor.apply(&source)?;
        let frame_count = supervisor.hardware().presented_frames().len();
        supervisor
            .hardware_mut()
            .inject(HardwareEvent::Touch(overlap_touch(1, TouchPhase::Down)));
        supervisor.step_at(0.5)?;
        supervisor.hardware_mut().inject(HardwareEvent::Capability {
            capability: HardwareCapability::Display,
            present: false,
        });
        supervisor.step_at(1.0)?;
        assert!(!supervisor.is_hardware_available());
        let error = supervisor
            .apply(&source)
            .expect_err("apply was accepted without the display");
        assert!(format!("{error:#}").contains("display"));
        let events = std::fs::read_to_string(&log)?;
        assert!(events.contains("touch:cancel"));
        assert!(events.contains("visibility:false:device"));

        supervisor.step_at(5.0)?;
        assert_eq!(supervisor.hardware().presented_frames().len(), frame_count);
        assert!(std::fs::read_to_string(&log)?.contains("timer"));

        supervisor.hardware_mut().inject(HardwareEvent::Capability {
            capability: HardwareCapability::Display,
            present: true,
        });
        supervisor.step_at(6.0)?;
        assert!(supervisor.is_hardware_available());
        assert_eq!(
            supervisor.hardware().presented_frames().len(),
            frame_count + 1
        );
        assert!(std::fs::read_to_string(&log)?.contains("visibility:true:device"));
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn each_missing_hardware_capability_rejects_new_applies() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("capability.lua");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return { api_version = 1, render = function() end }
            "#,
        )?;

        for capability in HardwareCapability::ALL {
            let state_file = directory
                .path()
                .join(format!("state/{capability:?}/config-path"));
            let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
            supervisor.apply(&source)?;
            supervisor.hardware_mut().inject(HardwareEvent::Capability {
                capability,
                present: false,
            });
            supervisor.step_at(1.0)?;

            let error = supervisor
                .apply(&source)
                .expect_err("apply was accepted with a missing capability");
            assert!(
                format!("{error:#}").contains(capability.name()),
                "missing capability was not named: {error:#}"
            );
            supervisor.shutdown()?;
        }
        Ok(())
    }

    #[test]
    fn display_loss_during_candidate_presentation_marks_hardware_unavailable() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let state_file = directory.path().join("state/sliver/config-path");
        let old_source = directory.path().join("old.lua");
        let new_source = directory.path().join("new.lua");
        let config = |red: u8, blue: u8| {
            format!(
                "require('sliver.v1'); return {{ api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, {}, 0, {}, 1) end }}",
                f64::from(red) / 255.0,
                f64::from(blue) / 255.0,
            )
        };
        std::fs::write(&old_source, config(255, 0))?;
        std::fs::write(&new_source, config(0, 255))?;
        let mut supervisor =
            Supervisor::new(FailingPresentHardware::new(state_file.clone()), state_file)?;
        supervisor.apply(&old_source)?;
        supervisor.hardware_mut().fail_next_present = true;
        supervisor.hardware_mut().fail_present_as_unavailable = true;

        let error = supervisor
            .apply(&new_source)
            .expect_err("display loss during presentation was accepted");
        assert!(format!("{error:#}").contains("injected presentation failure"));
        assert!(!supervisor.is_hardware_available());
        let error = supervisor
            .apply(&new_source)
            .expect_err("apply was accepted after display loss");
        assert!(format!("{error:#}").contains("display"));
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn failed_resume_reacquisition_waits_for_hardware_recovery() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("resume.lua");
        let state_file = directory.path().join("state/sliver/config-path");
        std::fs::write(
            &source,
            r#"
            require("sliver.v1")
            return { api_version = 1, render = function() end }
            "#,
        )?;
        let mut supervisor = Supervisor::new(
            FailingPresentHardware::new(state_file),
            directory.path().join("state/sliver/config-path"),
        )?;
        supervisor.apply(&source)?;
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Visibility { visible: false });
        supervisor.step_at(1.0)?;
        supervisor.hardware_mut().fail_reacquire = true;
        supervisor
            .hardware_mut()
            .inner
            .inject(HardwareEvent::Visibility { visible: true });
        supervisor.step_at(2.0)?;

        assert!(!supervisor.is_hardware_available());
        let error = supervisor
            .apply(&source)
            .expect_err("apply was accepted after failed resume reacquisition");
        assert!(format!("{error:#}").contains("display"));
        supervisor.shutdown()?;
        Ok(())
    }
}
