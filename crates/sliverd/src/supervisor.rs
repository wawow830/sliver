use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};

use crate::apply_ipc::absolute_lexical;
use crate::authorization::{AuthorizationGrant, SessionAuthorizer};
use crate::hardware::{
    ContactId, HardwareEvent, InputState, InputTransition, KeyboardKey, LogicalFrame, Modifier,
    ObservedKey, OutputKey, SyntheticKeyEvent, TouchBarHardware, TouchEvent, TouchPhase,
};
use crate::logind::{Logind, RealLogind};
use crate::lua_worker::{
    KeyOperation, KeyRequest, LuaWorker, ModifierMode, StagedLuaWorker, StopReason, WorkerEffects,
};
use crate::path_state::{PathStateSnapshot, PreparedPathState};
use crate::peer_credentials::PeerCredentials;

const MAX_POLL_WAIT: Duration = Duration::from_millis(50);

struct ActiveConfig {
    worker: LuaWorker,
    _selected_path: PathBuf,
    frame: LogicalFrame,
    backlight: f64,
    contacts: BTreeMap<ContactId, TouchEvent>,
}

struct AuthorizedRequest {
    path: PathBuf,
    peer: PeerCredentials,
    grant: AuthorizationGrant,
}

struct TouchQueue {
    events: Vec<Option<TouchEvent>>,
    moves: BTreeMap<ContactId, usize>,
}

#[derive(Clone, Default)]
struct SyntheticState {
    held: Vec<(crate::hardware::OutputKey, Vec<crate::hardware::OutputKey>)>,
}

impl TouchQueue {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            moves: BTreeMap::new(),
        }
    }

    fn push(&mut self, event: TouchEvent) {
        if event.phase == TouchPhase::Move {
            if let Some(index) = self.moves.insert(event.id, self.events.len()) {
                self.events[index] = None;
            }
        } else {
            self.moves.remove(&event.id);
        }
        self.events.push(Some(event));
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
                        !next.held.iter().any(|(key, _)| *key == request.key),
                        "synthetic key is already held"
                    );
                    let modifiers = next.resolve_modifiers(&request.modifiers, input_state)?;
                    ensure!(
                        !modifiers.contains(&request.key),
                        "a synthetic key cannot mirror itself"
                    );
                    for modifier in &modifiers {
                        if !next.is_held(*modifier) {
                            events.push(SyntheticKeyEvent {
                                key: *modifier,
                                active: true,
                            });
                        }
                    }
                    events.push(SyntheticKeyEvent {
                        key: request.key,
                        active: true,
                    });
                    next.held.push((request.key, modifiers));
                }
                KeyOperation::Up => {
                    let index = next
                        .held
                        .iter()
                        .position(|(key, _)| *key == request.key)
                        .context("synthetic key is not held")?;
                    let (_, modifiers) = next.held.remove(index);
                    events.push(SyntheticKeyEvent {
                        key: request.key,
                        active: false,
                    });
                    for modifier in modifiers.into_iter().rev() {
                        if !next.is_held(modifier) {
                            events.push(SyntheticKeyEvent {
                                key: modifier,
                                active: false,
                            });
                        }
                    }
                }
                KeyOperation::Tap => {
                    let modifiers = next.resolve_modifiers(&request.modifiers, input_state)?;
                    ensure!(
                        !modifiers.contains(&request.key),
                        "a synthetic key cannot mirror itself"
                    );
                    let mirrored: Vec<_> = modifiers
                        .into_iter()
                        .filter(|modifier| !next.is_held(*modifier))
                        .collect();
                    events.extend(
                        mirrored
                            .iter()
                            .copied()
                            .map(|key| SyntheticKeyEvent { key, active: true }),
                    );
                    events.push(SyntheticKeyEvent {
                        key: request.key,
                        active: true,
                    });
                    events.push(SyntheticKeyEvent {
                        key: request.key,
                        active: false,
                    });
                    events.extend(
                        mirrored
                            .into_iter()
                            .rev()
                            .map(|key| SyntheticKeyEvent { key, active: false }),
                    );
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
            ModifierMode::Inherit => Modifier::ALL
                .into_iter()
                .filter(|modifier| input_state.modifiers.is_active(*modifier))
                .map(modifier_key)
                .collect(),
            ModifierMode::None => Vec::new(),
            ModifierMode::Explicit(keys) => keys.clone(),
        };
        for key in &modifiers {
            ensure!(
                is_modifier_key(*key),
                "explicit modifiers must be modifier keys"
            );
        }
        ensure!(
            modifiers.windows(2).all(|pair| pair[0] != pair[1]),
            "explicit modifiers must not contain duplicates"
        );
        Ok(modifiers)
    }

    fn is_held(&self, key: OutputKey) -> bool {
        self.held.iter().any(|(held, _)| *held == key)
    }

    fn release(&self) -> (Self, Vec<SyntheticKeyEvent>) {
        let mut events = Vec::new();
        for (key, modifiers) in self.held.iter().rev() {
            events.push(SyntheticKeyEvent {
                key: *key,
                active: false,
            });
            for modifier in modifiers.iter().rev() {
                events.push(SyntheticKeyEvent {
                    key: *modifier,
                    active: false,
                });
            }
        }
        (Self::default(), events)
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

fn modifier_key(modifier: Modifier) -> OutputKey {
    OutputKey::Keyboard(match modifier {
        Modifier::LeftCtrl => KeyboardKey::LeftCtrl,
        Modifier::RightCtrl => KeyboardKey::RightCtrl,
        Modifier::LeftAlt => KeyboardKey::LeftAlt,
        Modifier::RightAlt => KeyboardKey::RightAlt,
        Modifier::LeftShift => KeyboardKey::LeftShift,
        Modifier::RightShift => KeyboardKey::RightShift,
        Modifier::LeftSuper => KeyboardKey::LeftSuper,
        Modifier::RightSuper => KeyboardKey::RightSuper,
    })
}

pub(crate) struct Supervisor<H: TouchBarHardware, L: Logind = RealLogind> {
    hardware: H,
    state_file: PathBuf,
    active: Option<ActiveConfig>,
    claimed: bool,
    origin: Instant,
    backlight: f64,
    input_state: InputState,
    down_contacts: BTreeMap<ContactId, TouchEvent>,
    ignored_contacts: BTreeSet<ContactId>,
    touch_queue: TouchQueue,
    next_timer_deadline: Option<f64>,
    synthetic: SyntheticState,
    authorizer: SessionAuthorizer<L>,
}

impl<H: TouchBarHardware> Supervisor<H, RealLogind> {
    pub(crate) fn new(hardware: H, state_file: PathBuf) -> Result<Self> {
        Self::new_with_logind(hardware, state_file, RealLogind::default())
    }
}

impl<H: TouchBarHardware, L: Logind> Supervisor<H, L> {
    pub(crate) fn new_with_logind(mut hardware: H, state_file: PathBuf, logind: L) -> Result<Self> {
        hardware.claim()?;
        let backlight = match hardware.get_backlight() {
            Ok(level) => level,
            Err(error) => {
                let _ = hardware.release();
                return Err(error).context("reading initial Touch Bar backlight");
            }
        };
        Ok(Self {
            hardware,
            state_file,
            active: None,
            claimed: true,
            origin: Instant::now(),
            backlight,
            input_state: InputState::default(),
            down_contacts: BTreeMap::new(),
            ignored_contacts: BTreeSet::new(),
            touch_queue: TouchQueue::new(),
            next_timer_deadline: None,
            synthetic: SyntheticState::default(),
            authorizer: SessionAuthorizer::new(logind),
        })
    }

    fn now_seconds(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    #[cfg(test)]
    pub(crate) fn apply(&mut self, requested_path: &Path) -> Result<()> {
        self.apply_candidate(requested_path, None)
    }

    fn apply_authorized(&mut self, request: AuthorizedRequest) -> Result<()> {
        self.authorizer.recheck(request.peer, &request.grant)?;
        self.apply_candidate(&request.path, Some((request.peer, request.grant)))
    }

    fn apply_candidate(
        &mut self,
        requested_path: &Path,
        authorization: Option<(PeerCredentials, AuthorizationGrant)>,
    ) -> Result<()> {
        let selected_path = absolute_lexical(requested_path)?;
        let metadata = std::fs::metadata(&selected_path)
            .with_context(|| format!("reading config metadata for {}", selected_path.display()))?;
        ensure!(
            metadata.is_file(),
            "config is not a regular file: {}",
            selected_path.display()
        );

        self.poll_hardware(Duration::ZERO)?;
        let current_backlight = self.hardware.get_backlight()?;
        self.backlight = current_backlight;
        let StagedLuaWorker {
            worker,
            frame,
            pending_backlight,
        } = LuaWorker::stage_with_backlight_and_input(
            &selected_path,
            current_backlight,
            self.input_state,
        )?;
        self.poll_hardware(Duration::ZERO)?;
        let latest_backlight = self.hardware.get_backlight()?;
        self.backlight = latest_backlight;
        let previous_path_state = PathStateSnapshot::capture(&self.state_file)?;
        let path_state = PreparedPathState::prepare(&self.state_file, &selected_path)?;
        if let Some((peer, grant)) = authorization {
            self.authorizer.recheck(peer, &grant)?;
        }
        path_state.commit()?;

        let old_frame = self.active.as_ref().map(|active| active.frame.clone());
        let old_backlight = latest_backlight;
        let candidate_backlight = pending_backlight.unwrap_or(old_backlight);
        let brightness_attempted = pending_backlight.is_some();
        let mut brightness_changed = false;
        if let Some(level) = pending_backlight {
            if let Err(error) = self.hardware.set_backlight(level) {
                return self.rollback_candidate(
                    previous_path_state,
                    old_frame.as_ref(),
                    old_backlight,
                    false,
                    brightness_attempted,
                    error,
                );
            }
            brightness_changed = true;
        }

        if let Err(error) = self.hardware.present(&frame) {
            return self.rollback_candidate(
                previous_path_state,
                old_frame.as_ref(),
                old_backlight,
                true,
                brightness_changed,
                error,
            );
        }

        let now = self.now_seconds();
        if let Err(error) = worker.commit(now, self.input_state) {
            return self.rollback_candidate(
                previous_path_state,
                old_frame.as_ref(),
                old_backlight,
                true,
                brightness_changed,
                error,
            );
        }

        self.backlight = candidate_backlight;
        self.release_synthetic_keys()?;
        self.ignored_contacts
            .extend(self.down_contacts.keys().copied());
        self.next_timer_deadline = Some(now);
        let replaced = self.active.replace(ActiveConfig {
            worker,
            _selected_path: selected_path,
            frame,
            backlight: candidate_backlight,
            contacts: BTreeMap::new(),
        });
        if let Some(replaced) = replaced {
            let cancels: Vec<_> = replaced
                .contacts
                .values()
                .map(|event| TouchEvent {
                    phase: TouchPhase::Cancel,
                    time: now,
                    ..*event
                })
                .collect();
            if !cancels.is_empty() {
                if let Err(error) =
                    replaced
                        .worker
                        .drive(now, self.input_state, Vec::new(), cancels)
                {
                    eprintln!(
                        "replaced Lua worker did not receive contact cancellation: {error:#}"
                    );
                }
            }
            if let Err(error) = replaced.worker.shutdown(StopReason::Replaced) {
                eprintln!("replaced Lua worker did not stop cleanly: {error:#}");
            }
        }
        Ok(())
    }

    fn release_synthetic_keys(&mut self) -> Result<()> {
        let (empty, events) = self.synthetic.release();
        if !events.is_empty() {
            self.hardware.emit_key_events(&events)?;
        }
        self.synthetic = empty;
        Ok(())
    }

    fn rollback_candidate(
        &mut self,
        previous_path_state: PathStateSnapshot,
        old_frame: Option<&LogicalFrame>,
        old_backlight: f64,
        restore_frame: bool,
        restore_backlight: bool,
        error: anyhow::Error,
    ) -> Result<()> {
        let mut error = error;
        if restore_frame {
            if let Some(frame) = old_frame {
                if let Err(restore_error) = self.hardware.present(frame) {
                    error = error.context(format!(
                        "restoring the previous frame after candidate failure also failed: {restore_error:#}"
                    ));
                }
            }
        }
        if restore_backlight {
            if let Err(restore_error) = self.hardware.set_backlight(old_backlight) {
                error = error.context(format!(
                    "restoring the previous backlight after candidate failure also failed: {restore_error:#}"
                ));
            }
        }
        if let Err(restore_error) = previous_path_state.restore(&self.state_file) {
            error = error.context(format!(
                "restoring selected path after candidate failure also failed: {restore_error:#}"
            ));
        }
        Err(error)
    }

    fn route_touch(&mut self, event: TouchEvent) {
        match event.phase {
            TouchPhase::Down => {
                if self.down_contacts.insert(event.id, event).is_some() {
                    return;
                }
                if self.ignored_contacts.contains(&event.id) {
                    return;
                }
                if let Some(active) = self.active.as_mut() {
                    active.contacts.insert(event.id, event);
                    self.touch_queue.push(event);
                }
            }
            TouchPhase::Move => {
                let Some(contact) = self.down_contacts.get_mut(&event.id) else {
                    return;
                };
                *contact = event;
                if self.ignored_contacts.contains(&event.id) {
                    return;
                }
                if let Some(active) = self.active.as_mut() {
                    if let std::collections::btree_map::Entry::Occupied(mut contact) =
                        active.contacts.entry(event.id)
                    {
                        contact.insert(event);
                        self.touch_queue.push(event);
                    }
                }
            }
            TouchPhase::Up | TouchPhase::Cancel => {
                self.down_contacts.remove(&event.id);
                if self.ignored_contacts.remove(&event.id) {
                    return;
                }
                if let Some(active) = self.active.as_mut() {
                    if active.contacts.remove(&event.id).is_some() {
                        self.touch_queue.push(event);
                    }
                }
            }
        }
    }

    fn poll_hardware(&mut self, timeout: Duration) -> Result<()> {
        let events = self.hardware.poll(timeout)?;
        let now = self.now_seconds();
        self.process_events_at(now, events)
    }

    fn process_events_at(&mut self, now: f64, events: Vec<HardwareEvent>) -> Result<()> {
        let mut transitions = Vec::new();
        for event in events {
            match event {
                HardwareEvent::Touch(touch) => self.route_touch(touch),
                HardwareEvent::Fn { active } => {
                    if self.input_state.apply(ObservedKey::Fn, active) {
                        transitions.push(InputTransition {
                            key: ObservedKey::Fn,
                            active,
                            state: self.input_state,
                        });
                    }
                }
                HardwareEvent::Modifier { modifier, active } => {
                    if self
                        .input_state
                        .apply(ObservedKey::Modifier(modifier), active)
                    {
                        transitions.push(InputTransition {
                            key: ObservedKey::Modifier(modifier),
                            active,
                            state: self.input_state,
                        });
                    }
                }
                HardwareEvent::Device { .. } | HardwareEvent::Visibility { .. } => {}
            }
        }
        let touches = self.touch_queue.drain();
        let timer_due = self
            .next_timer_deadline
            .is_some_and(|deadline| deadline <= now);
        if !transitions.is_empty() || !touches.is_empty() || timer_due {
            self.drive_active(now, transitions, touches)?;
        }
        Ok(())
    }

    fn drive_active(
        &mut self,
        now: f64,
        transitions: Vec<InputTransition>,
        touches: Vec<TouchEvent>,
    ) -> Result<()> {
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        let effects = match active
            .worker
            .drive(now, self.input_state, transitions, touches)
        {
            Ok(effects) => effects,
            Err(error) => return self.fail_active_worker(error),
        };
        self.next_timer_deadline = if effects.redraw_pending {
            Some(now)
        } else {
            effects
                .next_timer_deadline
                .or_else(|| effects.frame.as_ref().map(|_| now))
        };
        if let Err(error) = self.apply_effects(effects) {
            return self.fail_active_worker(error);
        }
        Ok(())
    }

    fn fail_active_worker(&mut self, error: anyhow::Error) -> Result<()> {
        if let Err(cleanup_error) = self.release_synthetic_keys() {
            return Err(error.context(format!(
                "releasing synthetic keys after worker failure also failed: {cleanup_error:#}"
            )));
        }
        Err(error)
    }

    fn apply_effects(&mut self, effects: WorkerEffects) -> Result<()> {
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        let (next_synthetic, key_events) = self
            .synthetic
            .plan(&effects.key_requests, self.input_state)?;
        if !key_events.is_empty() {
            self.hardware.emit_key_events(&key_events)?;
        }
        self.synthetic = next_synthetic;
        let old_frame = active.frame.clone();
        let old_backlight = active.backlight;
        let frame = effects.frame;
        let backlight = effects.backlight;
        let mut brightness_changed = false;

        if let Some(level) = backlight {
            if let Err(error) = self.hardware.set_backlight(level) {
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
            if let Err(error) = self.hardware.present(frame) {
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
            active.frame = frame;
        }
        if let Some(level) = backlight {
            active.backlight = level;
            self.backlight = level;
        }
        Ok(())
    }

    fn poll_wait(&self, now: f64) -> Duration {
        let Some(deadline) = self.next_timer_deadline else {
            return MAX_POLL_WAIT;
        };
        if deadline <= now {
            return Duration::ZERO;
        }
        Duration::from_secs_f64((deadline - now).min(MAX_POLL_WAIT.as_secs_f64()))
    }

    #[cfg(test)]
    fn step_at(&mut self, now: f64) -> Result<()> {
        let events = self.hardware.poll(Duration::ZERO)?;
        self.process_events_at(now, events)
    }

    pub(crate) fn shutdown(mut self) -> Result<()> {
        let now = self.now_seconds();
        let synthetic_result = self.release_synthetic_keys();
        let stop_result = match self.active.take() {
            Some(active) => {
                let cancels: Vec<_> = active
                    .contacts
                    .values()
                    .map(|event| TouchEvent {
                        phase: TouchPhase::Cancel,
                        time: now,
                        ..*event
                    })
                    .collect();
                if !cancels.is_empty() {
                    let _ = active
                        .worker
                        .drive(now, self.input_state, Vec::new(), cancels);
                }
                active.worker.shutdown(StopReason::Shutdown)
            }
            None => Ok(()),
        };
        let release_result = self.hardware.release();
        self.claimed = false;
        match (stop_result.and(synthetic_result), release_result) {
            (Err(error), Err(release_error)) => {
                eprintln!("hardware release failed after Lua stop error: {release_error:#}");
                Err(error)
            }
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error).context("releasing supervisor hardware"),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn hardware(&self) -> &H {
        &self.hardware
    }

    #[cfg(test)]
    pub(crate) fn hardware_mut(&mut self) -> &mut H {
        &mut self.hardware
    }
}

struct QueuedRequest {
    stream: UnixStream,
    request: Result<AuthorizedRequest>,
}

pub(crate) fn serve<H: TouchBarHardware, L: Logind>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H, L>,
) -> Result<()> {
    serve_queue(listener, supervisor, None)
}

#[cfg(test)]
fn serve_for_test<H: TouchBarHardware, L: Logind>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H, L>,
    request_limit: usize,
) -> Result<()> {
    serve_queue(listener, supervisor, Some(request_limit))
}

fn serve_queue<H: TouchBarHardware, L: Logind>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H, L>,
    request_limit: Option<usize>,
) -> Result<()> {
    let authorizer = supervisor.authorizer.clone();
    let (sender, receiver) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let acceptor_stop = stop.clone();
    let acceptor = thread::spawn(move || {
        accept_requests(listener, authorizer, sender, request_limit, acceptor_stop)
    });

    let mut service_result = Ok(());
    let mut processed = 0;
    loop {
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
            }
            Err(mpsc::TryRecvError::Empty) => {
                let now = supervisor.now_seconds();
                let wait = supervisor.poll_wait(now);
                match supervisor.hardware.poll(wait) {
                    Ok(events) => {
                        let now = supervisor.now_seconds();
                        if let Err(error) = supervisor.process_events_at(now, events) {
                            service_result = Err(error);
                            break;
                        }
                    }
                    Err(error) => {
                        service_result = Err(error);
                        break;
                    }
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }

    stop.store(true, Ordering::Release);
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
    sender: mpsc::Sender<QueuedRequest>,
    request_limit: Option<usize>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    let mut sent = 0;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::park_timeout(Duration::from_millis(10));
                continue;
            }
            Err(error) => return Err(error).context("accepting apply request"),
        };
        let request = read_authorized_request(&mut stream, &authorizer);
        if sender.send(QueuedRequest { stream, request }).is_err() {
            return Ok(());
        }
        sent += 1;
        if request_limit.is_some_and(|limit| sent >= limit) {
            return Ok(());
        }
    }
}

fn read_authorized_request<L: Logind>(
    stream: &mut UnixStream,
    authorizer: &SessionAuthorizer<L>,
) -> Result<AuthorizedRequest> {
    let peer = crate::peer_credentials::read(stream)?;
    let path = crate::apply_ipc::read_request(stream)?;
    let grant = authorizer.authorize(peer)?;
    Ok(AuthorizedRequest { path, peer, grant })
}

fn serve_queued_request<H: TouchBarHardware, L: Logind>(
    mut queued: QueuedRequest,
    supervisor: &mut Supervisor<H, L>,
) -> Result<()> {
    let result = queued
        .request
        .and_then(|request| supervisor.apply_authorized(request));
    crate::apply_ipc::write_reply(&mut queued.stream, &result).context("sending apply reply")
}

#[cfg(test)]
fn serve_connection<H: TouchBarHardware, L: Logind>(
    stream: &mut UnixStream,
    supervisor: &mut Supervisor<H, L>,
) -> Result<()> {
    let result = (|| {
        let peer = crate::peer_credentials::read(stream)?;
        let path = crate::apply_ipc::read_request(stream)?;
        let grant = supervisor.authorizer.authorize(peer)?;
        Ok(AuthorizedRequest { path, peer, grant })
    })()
    .and_then(|request| supervisor.apply_authorized(request));
    crate::apply_ipc::write_reply(stream, &result).context("sending apply reply")
}

impl<H: TouchBarHardware, L: Logind> Drop for Supervisor<H, L> {
    fn drop(&mut self) {
        self.active.take();
        if self.claimed {
            let _ = self.hardware.release();
            self.claimed = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};

    use crate::hardware::{
        ConsumerKey, FakeAction, FakeKey, FakeKeyEvent, FakeTouchBar, HardwareEvent, KeyboardKey,
        LogicalFrame, Modifier, ModifierState, TouchBarHardware, TouchEvent, TouchPhase,
    };
    use crate::logind::{ActiveSession, FakeLogind, Session};

    use super::{serve_connection, serve_for_test, Supervisor};

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

    struct FailingPresentHardware {
        inner: FakeTouchBar,
        state_file: std::path::PathBuf,
        fail_next_present: bool,
        fail_next_backlight: bool,
        state_seen_at_failure: Vec<u8>,
    }

    impl FailingPresentHardware {
        fn new(state_file: std::path::PathBuf) -> Self {
            Self {
                inner: FakeTouchBar::new(),
                state_file,
                fail_next_present: false,
                fail_next_backlight: false,
                state_seen_at_failure: Vec::new(),
            }
        }
    }

    impl TouchBarHardware for FailingPresentHardware {
        fn claim(&mut self) -> Result<()> {
            self.inner.claim()
        }

        fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
            self.inner.poll(timeout)
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            if self.fail_next_present {
                self.fail_next_present = false;
                self.state_seen_at_failure = std::fs::read(&self.state_file)?;
                bail!("injected presentation failure");
            }
            self.inner.present(frame)
        }

        fn emit_key_events(&mut self, events: &[crate::hardware::SyntheticKeyEvent]) -> Result<()> {
            self.inner.emit_key_events(events)
        }

        fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()> {
            self.inner.tap_function_key(index, modifiers)
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
            new_source.as_os_str().as_encoded_bytes()
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
    fn static_worker_does_not_render_without_a_request() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("static.lua");
        let log = directory.path().join("renders");
        std::fs::write(
            &source,
            format!(
                r#"
                require("sliver.v1")
                local sliver = require("sliver.v1")
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
}
