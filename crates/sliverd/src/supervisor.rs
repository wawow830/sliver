use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};

use crate::apply_ipc::absolute_lexical;
use crate::hardware::{
    ContactId, HardwareEvent, LogicalFrame, TouchBarHardware, TouchEvent, TouchPhase,
};
use crate::lua_worker::{LuaWorker, StagedLuaWorker, StopReason, WorkerEffects};
use crate::path_state::{PathStateSnapshot, PreparedPathState};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_POLL_WAIT: Duration = Duration::from_millis(50);

struct ActiveConfig {
    worker: LuaWorker,
    _selected_path: PathBuf,
    frame: LogicalFrame,
    backlight: f64,
    contacts: BTreeMap<ContactId, TouchEvent>,
}

struct TouchQueue {
    events: Vec<Option<TouchEvent>>,
    moves: BTreeMap<ContactId, usize>,
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

pub(crate) struct Supervisor<H: TouchBarHardware> {
    hardware: H,
    state_file: PathBuf,
    active: Option<ActiveConfig>,
    claimed: bool,
    origin: Instant,
    backlight: f64,
    down_contacts: BTreeMap<ContactId, TouchEvent>,
    ignored_contacts: BTreeSet<ContactId>,
    touch_queue: TouchQueue,
    next_timer_deadline: Option<f64>,
    last_presented_time: Option<f64>,
}

impl<H: TouchBarHardware> Supervisor<H> {
    pub(crate) fn new(mut hardware: H, state_file: PathBuf) -> Result<Self> {
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
            down_contacts: BTreeMap::new(),
            ignored_contacts: BTreeSet::new(),
            touch_queue: TouchQueue::new(),
            next_timer_deadline: None,
            last_presented_time: None,
        })
    }

    fn now_seconds(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    pub(crate) fn apply(&mut self, requested_path: &Path) -> Result<()> {
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
        let stage_time = self.now_seconds();
        let StagedLuaWorker {
            worker,
            frame: staged_frame,
            pending_backlight,
        } = LuaWorker::stage_with_backlight_at(
            &selected_path,
            current_backlight,
            stage_time,
        )?;
        let crate::lua_worker::TimedFrame {
            frame,
            timing: frame_timing,
        } = staged_frame;
        self.poll_hardware(Duration::ZERO)?;
        let latest_backlight = self.hardware.get_backlight()?;
        self.backlight = latest_backlight;
        let previous_path_state = PathStateSnapshot::capture(&self.state_file)?;
        let path_state = PreparedPathState::prepare(&self.state_file, &selected_path)?;
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
        if let Err(error) = worker.commit(now) {
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
        self.last_presented_time = Some(frame_timing.presentation_time);
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
                if let Err(error) = replaced.worker.drive(now, 0.0, cancels) {
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
        for event in events {
            if let HardwareEvent::Touch(touch) = event {
                self.route_touch(touch);
            }
        }
        let touches = self.touch_queue.drain();
        let timer_due = self
            .next_timer_deadline
            .is_some_and(|deadline| deadline <= now);
        if !touches.is_empty() || timer_due {
            self.drive_active(now, touches)?;
        }
        Ok(())
    }

    fn drive_active(&mut self, now: f64, touches: Vec<TouchEvent>) -> Result<()> {
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        let delta = self
            .last_presented_time
            .map(|previous| (now - previous).max(0.0))
            .unwrap_or(0.0);
        let effects = active.worker.drive(now, delta, touches)?;
        self.next_timer_deadline = effects
            .next_timer_deadline
            .or_else(|| effects.frame.as_ref().map(|_| now));
        self.apply_effects(effects)
    }

    fn apply_effects(&mut self, effects: WorkerEffects) -> Result<()> {
        let Some(active) = self.active.as_ref() else {
            return Ok(());
        };
        let old_frame = active.frame.clone();
        let old_backlight = active.backlight;
        let frame = effects.frame;
        let backlight = effects.backlight;
        let frame_time = frame.as_ref().map(|frame| frame.timing.presentation_time);
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
            if let Err(error) = self.hardware.present(&frame.frame) {
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
                    let _ = active.worker.drive(now, 0.0, cancels);
                }
                active.worker.shutdown(StopReason::Shutdown)
            }
            None => Ok(()),
        };
        let release_result = self.hardware.release();
        self.claimed = false;
        match (stop_result, release_result) {
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

struct PendingRequest {
    stream: UnixStream,
    header: [u8; 4],
    header_len: usize,
    length: Option<usize>,
    payload: Vec<u8>,
    payload_len: usize,
}

impl PendingRequest {
    fn new(stream: UnixStream) -> Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            header: [0; 4],
            header_len: 0,
            length: None,
            payload: Vec::new(),
            payload_len: 0,
        })
    }

    fn try_path(&mut self) -> Result<Option<PathBuf>> {
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

        Ok(Some(PathBuf::from(std::ffi::OsString::from_vec(
            std::mem::take(&mut self.payload),
        ))))
    }
}

pub(crate) fn serve<H: TouchBarHardware>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H>,
) -> Result<()> {
    listener.set_nonblocking(true)?;
    let mut requests = VecDeque::new();
    loop {
        loop {
            match listener.accept() {
                Ok((stream, _)) => requests.push_back(PendingRequest::new(stream)?),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error).context("accepting apply request"),
            }
        }

        if let Some(request) = requests.front_mut() {
            match request.try_path() {
                Ok(Some(path)) => {
                    let mut request = requests.pop_front().expect("request was present");
                    let result = supervisor.apply(&path);
                    request.stream.set_nonblocking(false)?;
                    crate::apply_ipc::write_reply(&mut request.stream, &result)
                        .context("sending apply reply")?;
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    let mut request = requests.pop_front().expect("request was present");
                    let result: Result<()> = Err(error);
                    request.stream.set_nonblocking(false)?;
                    crate::apply_ipc::write_reply(&mut request.stream, &result)
                        .context("sending apply error")?;
                    continue;
                }
            }
        }

        let now = supervisor.now_seconds();
        let wait = supervisor.poll_wait(now);
        let events = supervisor.hardware.poll(wait)?;
        let now = supervisor.now_seconds();
        supervisor.process_events_at(now, events)?;
    }
}

#[cfg(test)]
fn serve_connection<H: TouchBarHardware>(
    stream: &mut UnixStream,
    supervisor: &mut Supervisor<H>,
) -> Result<()> {
    let result = crate::apply_ipc::read_request(stream).and_then(|path| supervisor.apply(&path));
    crate::apply_ipc::write_reply(stream, &result).context("sending apply reply")
}

impl<H: TouchBarHardware> Drop for Supervisor<H> {
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
        FakeAction, FakeTouchBar, HardwareEvent, LogicalFrame, Modifier, ModifierState,
        TouchBarHardware, TouchEvent, TouchPhase,
    };

    use super::{serve_connection, Supervisor};

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
        supervisor.step_at(1.0)?;

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
        assert!((lines[1][1] - (lines[1][0] - lines[0][0])).abs() < 0.001);
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
        let server = thread::spawn(move || -> Result<Supervisor<FakeTouchBar>> {
            let mut supervisor = Supervisor::new(FakeTouchBar::new(), state_file)?;
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
