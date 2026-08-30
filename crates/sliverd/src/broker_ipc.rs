use std::cell::{RefCell, RefMut};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};

use crate::authorization::{NoActiveUserSession, SessionAuthorizer, WorkerNotOwnedByActiveUser};
use crate::hardware::{
    function_key_output, tap_key_events, ContactId, HardwareEvent, InputState, LogicalFrame,
    Modifier, ModifierState, OutputKey, SyntheticKeyEvent, TouchBarHardware, TouchEvent,
    TouchPhase,
};
use crate::logind::RealLogind;
use crate::lua_worker::StopReason;
use crate::m2_hardware::M2TouchBar;
use crate::supervisor::Supervisor;

const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const CLAIM: u8 = 1;
const POLL: u8 = 2;
const PRESENT: u8 = 3;
const CONFIRM: u8 = 4;
const EMIT_KEYS: u8 = 5;
const GET_BACKLIGHT: u8 = 6;
const SET_BACKLIGHT: u8 = 7;
const RELEASE: u8 = 8;
const OK: u8 = 0;
const ERROR: u8 = 1;
const SESSION_REVOKED: u8 = 2;
const WAIT_FOR_SESSION: u8 = 3;
const REVOKED_REPLACED: u8 = 0;
const REVOKED_LOGOUT: u8 = 1;
const LOGOUT_COMPLETE: u8 = 9;
const SEAT: &str = "seat0";

pub(crate) fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SLIVER_BROKER_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    Ok(PathBuf::from("/run/sliver/broker.sock"))
}

pub(crate) struct BrokerHardware {
    stream: Option<UnixStream>,
    socket: Option<PathBuf>,
    input_state: InputState,
    claimed: bool,
    session_revoked: bool,
    revoked_reason: Option<StopReason>,
}

impl BrokerHardware {
    pub(crate) fn new() -> Self {
        Self {
            stream: None,
            socket: None,
            input_state: InputState::default(),
            claimed: false,
            session_revoked: false,
            revoked_reason: None,
        }
    }

    #[cfg(test)]
    fn new_at(socket: PathBuf) -> Self {
        Self {
            stream: None,
            socket: Some(socket),
            input_state: InputState::default(),
            claimed: false,
            session_revoked: false,
            revoked_reason: None,
        }
    }

    pub(crate) fn session_revoked(&self) -> bool {
        self.session_revoked
    }

    pub(crate) fn revoked_stop_reason(&self) -> StopReason {
        self.revoked_reason.unwrap_or(StopReason::Shutdown)
    }

    pub(crate) fn logout_complete(&mut self) -> Result<()> {
        ensure!(self.session_revoked, "the broker session was not revoked");
        self.request(LOGOUT_COMPLETE, &[]).map(|_| ())
    }

    fn record_revocation(&mut self, payload: &[u8]) -> Result<()> {
        let reason = payload
            .first()
            .copied()
            .context("broker revocation has no reason")?;
        self.revoked_reason = Some(match reason {
            REVOKED_REPLACED => StopReason::Replaced,
            REVOKED_LOGOUT => StopReason::Logout,
            other => bail!("unknown broker revocation reason {other}"),
        });
        Ok(())
    }

    fn request(&mut self, operation: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let stream = self
            .stream
            .as_mut()
            .context("broker connection is not open")?;
        write_message(stream, operation, payload)?;
        let response = read_message(stream)?;
        let (status, body) = response.split_first().context("broker response is empty")?;
        match *status {
            OK => Ok(body.to_vec()),
            ERROR => bail!("{}", String::from_utf8_lossy(body)),
            SESSION_REVOKED => {
                self.session_revoked = true;
                self.record_revocation(body)?;
                bail!("broker revoked the user session")
            }
            other => bail!("broker returned unknown status {other}"),
        }
    }
}

impl TouchBarHardware for BrokerHardware {
    fn claim(&mut self) -> Result<()> {
        ensure!(!self.claimed, "broker hardware is already claimed");
        let path = self.socket.clone().unwrap_or(socket_path()?);
        let mut stream = UnixStream::connect(&path)
            .with_context(|| format!("connecting to hardware broker at {}", path.display()))?;
        write_message(&mut stream, CLAIM, &[])?;
        let response = read_message(&mut stream)?;
        let (status, body) = response
            .split_first()
            .context("broker claim response is empty")?;
        if *status != OK {
            let error = anyhow::anyhow!(String::from_utf8_lossy(body).into_owned());
            if *status == WAIT_FOR_SESSION {
                return Err(error.context(crate::WaitForActiveSession));
            }
            return Err(error);
        }
        let (input_state, backlight) = decode_claim(body)?;
        self.stream = Some(stream);
        self.input_state = input_state;
        self.claimed = true;
        let _ = backlight;
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        ensure!(self.claimed, "broker hardware is not claimed");
        if self.session_revoked {
            bail!("broker revoked the user session");
        }
        let millis = timeout.as_millis().min(u64::MAX as u128) as u64;
        let mut payload = Vec::new();
        payload.extend_from_slice(&millis.to_be_bytes());
        let body = self.request(POLL, &payload)?;
        decode_events(&body).inspect(|events| {
            for event in events {
                match *event {
                    HardwareEvent::Fn { active } => {
                        self.input_state
                            .apply(crate::hardware::ObservedKey::Fn, active);
                    }
                    HardwareEvent::Modifier { modifier, active } => {
                        self.input_state
                            .apply(crate::hardware::ObservedKey::Modifier(modifier), active);
                    }
                    HardwareEvent::Touch(_)
                    | HardwareEvent::Device { .. }
                    | HardwareEvent::Visibility { .. } => {}
                }
            }
        })
    }

    fn input_state(&self) -> InputState {
        self.input_state
    }

    fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
        ensure!(self.claimed, "broker hardware is not claimed");
        let mut payload = Vec::with_capacity(12 + frame.pixels().len());
        put_u32(&mut payload, frame.width())?;
        put_u32(&mut payload, frame.height())?;
        put_u32(&mut payload, frame.stride())?;
        payload.extend_from_slice(frame.pixels());
        self.request(PRESENT, &payload).map(|_| ())
    }

    fn confirm_owner(&mut self) -> Result<()> {
        ensure!(self.claimed, "broker hardware is not claimed");
        self.request(CONFIRM, &[]).map(|_| ())
    }

    fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
        ensure!(self.claimed, "broker hardware is not claimed");
        let mut payload = Vec::new();
        put_u32(&mut payload, events.len())?;
        for event in events {
            encode_key_event(&mut payload, *event);
        }
        self.request(EMIT_KEYS, &payload).map(|_| ())
    }

    fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()> {
        let key = function_key_output(index).context("function-key index is out of range")?;
        self.emit_key_events(&tap_key_events(
            key,
            &crate::hardware::modifier_output_keys(modifiers),
        ))
    }

    fn get_backlight(&mut self) -> Result<f64> {
        ensure!(self.claimed, "broker hardware is not claimed");
        let body = self.request(GET_BACKLIGHT, &[])?;
        ensure!(body.len() == 8, "broker backlight response is malformed");
        Ok(f64::from_bits(u64::from_be_bytes(
            body.as_slice().try_into()?,
        )))
    }

    fn set_backlight(&mut self, level: f64) -> Result<()> {
        ensure!(self.claimed, "broker hardware is not claimed");
        self.request(SET_BACKLIGHT, &level.to_bits().to_be_bytes())
            .map(|_| ())
    }

    fn release(&mut self) -> Result<()> {
        if !self.claimed {
            return Ok(());
        }
        let result = if self.session_revoked {
            Ok(())
        } else {
            self.request(RELEASE, &[]).map(|_| ())
        };
        self.stream = None;
        self.claimed = false;
        result
    }
}

impl Drop for BrokerHardware {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

pub(crate) struct SharedHardware<H: TouchBarHardware>(Rc<RefCell<H>>);

impl<H: TouchBarHardware> Clone for SharedHardware<H> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<H: TouchBarHardware> SharedHardware<H> {
    fn new(hardware: H) -> Self {
        Self(Rc::new(RefCell::new(hardware)))
    }

    #[cfg(test)]
    fn inspect<R>(&self, inspect: impl FnOnce(&H) -> R) -> R {
        inspect(&self.0.borrow())
    }

    fn lock(&self) -> Result<RefMut<'_, H>> {
        self.0
            .try_borrow_mut()
            .map_err(|_| anyhow::anyhow!("broker hardware is already in use"))
    }
}

impl<H: TouchBarHardware> TouchBarHardware for SharedHardware<H> {
    fn claim(&mut self) -> Result<()> {
        self.lock()?.claim()
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        self.lock()?.poll(timeout)
    }

    fn input_state(&self) -> InputState {
        self.0
            .try_borrow()
            .map(|hardware| hardware.input_state())
            .unwrap_or_default()
    }

    fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
        self.lock()?.present(frame)
    }

    fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
        self.lock()?.emit_key_events(events)
    }

    fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()> {
        self.lock()?.tap_function_key(index, modifiers)
    }

    fn get_backlight(&mut self) -> Result<f64> {
        self.lock()?.get_backlight()
    }

    fn set_backlight(&mut self, level: f64) -> Result<()> {
        self.lock()?.set_backlight(level)
    }

    fn release(&mut self) -> Result<()> {
        self.lock()?.release()
    }
}

pub(crate) fn broker_main() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let socket = socket_path()?;
    let directory = socket
        .parent()
        .context("broker socket path has no parent directory")?;
    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))?;
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;

    let shared = SharedHardware::new(M2TouchBar::new());
    let fallback = Supervisor::new_fallback(
        shared,
        PathBuf::from("/var/lib/sliver/config-path"),
        Some(crate::default_source::source()),
    )?;
    let authorizer = SessionAuthorizer::new(RealLogind::default());
    let running = Arc::new(AtomicBool::new(true));
    let result = run_broker(listener, fallback, authorizer, running, SEAT, true);
    let _ = std::fs::remove_file(socket);
    result
}

fn run_broker<H: TouchBarHardware, L: crate::logind::Logind>(
    listener: UnixListener,
    mut fallback: Supervisor<H, L>,
    authorizer: SessionAuthorizer<L>,
    running: Arc<AtomicBool>,
    seat: &str,
    verify_supervisor: bool,
) -> Result<()> {
    let initial_active = authorizer.active_session(seat)?;
    let mut fallback_running = false;
    let mut fallback_attempted = false;
    if initial_active.is_none() {
        fallback.start_fallback()?;
        fallback_running = fallback.has_active_worker();
        fallback_attempted = true;
    }
    let mut last_active = initial_active;

    listener.set_nonblocking(true)?;
    while running.load(Ordering::Acquire) {
        if let Some(stream) = accept_nonblocking(&listener)? {
            let result = handle_client(
                stream,
                &mut fallback,
                &authorizer,
                &mut fallback_running,
                seat,
                verify_supervisor,
            );
            if let Err(error) = result {
                crate::system_log::broker_error(format!("broker client failed: {error:#}"));
            } else {
                // A client was the user owner. Permit one fresh fallback start
                // after that owner logs out.
                fallback_attempted = false;
            }
            if !fallback_running && authorizer.active_session(seat)?.is_none() {
                fallback.start_fallback()?;
                fallback_running = fallback.has_active_worker();
                fallback_attempted = true;
            }
            continue;
        }

        let active = authorizer.active_session(seat)?;
        if active != last_active {
            if active.is_none() {
                fallback_attempted = false;
            }
            last_active = active.clone();
        }
        if active.is_some() && fallback_running {
            fallback.handoff_owner_with_reason(StopReason::Replaced)?;
            fallback_attempted = false;
        } else if active.is_none() && !fallback_running && !fallback_attempted {
            fallback.start_fallback()?;
            fallback_attempted = true;
        }
        if fallback_running || fallback.has_recovery() {
            fallback.poll(Duration::from_millis(50))?;
            fallback_running = fallback.has_active_worker();
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fallback.shutdown()
}

fn accept_nonblocking(listener: &UnixListener) -> Result<Option<UnixStream>> {
    match listener.accept() {
        Ok((stream, _)) => Ok(Some(stream)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[derive(Default)]
struct HeldKeys {
    keys: Vec<OutputKey>,
}

#[derive(Default)]
struct ClientState {
    held_keys: HeldKeys,
    contacts: ActiveContacts,
}

#[derive(Default)]
struct ActiveContacts {
    contacts: BTreeMap<ContactId, TouchEvent>,
}

impl ActiveContacts {
    fn observe(&mut self, events: &[HardwareEvent]) {
        for event in events {
            let HardwareEvent::Touch(event) = event else {
                continue;
            };
            match event.phase {
                TouchPhase::Down | TouchPhase::Move => {
                    self.contacts.insert(event.id, *event);
                }
                TouchPhase::Up | TouchPhase::Cancel => {
                    self.contacts.remove(&event.id);
                }
            }
        }
    }

    fn cancel(&mut self) -> Vec<HardwareEvent> {
        let events = self
            .contacts
            .values()
            .map(|event| {
                HardwareEvent::Touch(TouchEvent {
                    phase: TouchPhase::Cancel,
                    ..*event
                })
            })
            .collect();
        self.contacts.clear();
        events
    }
}

impl HeldKeys {
    fn observe(&mut self, events: &[SyntheticKeyEvent]) {
        for event in events {
            if event.active {
                if !self.keys.contains(&event.key) {
                    self.keys.push(event.key);
                }
            } else if let Some(index) = self.keys.iter().position(|key| *key == event.key) {
                self.keys.remove(index);
            }
        }
    }

    fn release<H: TouchBarHardware>(&mut self, hardware: &mut H) -> Result<()> {
        let events: Vec<_> = self
            .keys
            .iter()
            .rev()
            .copied()
            .map(|key| SyntheticKeyEvent { key, active: false })
            .collect();
        self.keys.clear();
        if events.is_empty() {
            return Ok(());
        }
        hardware.emit_key_events(&events)
    }
}

fn handle_client<H: TouchBarHardware, L: crate::logind::Logind>(
    stream: UnixStream,
    fallback: &mut Supervisor<H, L>,
    authorizer: &SessionAuthorizer<L>,
    fallback_running: &mut bool,
    seat: &str,
    verify_supervisor: bool,
) -> Result<()> {
    let mut client_state = ClientState::default();
    let result = handle_client_inner(
        stream,
        fallback,
        authorizer,
        fallback_running,
        &mut client_state,
        seat,
        verify_supervisor,
    );
    let cleanup = client_state.held_keys.release(fallback.hardware_mut());
    match (result, cleanup) {
        (Err(error), Err(cleanup_error)) => {
            Err(error).context(format!("broker key cleanup also failed: {cleanup_error:#}"))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("cleaning up broker client keys"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn handle_client_inner<H: TouchBarHardware, L: crate::logind::Logind>(
    mut stream: UnixStream,
    fallback: &mut Supervisor<H, L>,
    authorizer: &SessionAuthorizer<L>,
    fallback_running: &mut bool,
    client_state: &mut ClientState,
    seat: &str,
    verify_supervisor: bool,
) -> Result<()> {
    let peer = crate::peer_credentials::read(&stream)?;
    if verify_supervisor {
        ensure_supervisor_peer(peer.pid)?;
    }
    let grant = match authorizer.authorize_active_uid(peer.uid, seat) {
        Ok(grant) => grant,
        Err(error) => {
            let status = if error.chain().any(|cause| {
                cause.downcast_ref::<NoActiveUserSession>().is_some()
                    || cause.downcast_ref::<WorkerNotOwnedByActiveUser>().is_some()
            }) {
                WAIT_FOR_SESSION
            } else {
                ERROR
            };
            write_message(&mut stream, status, error.to_string().as_bytes())?;
            return Err(error);
        }
    };
    let request = read_message(&mut stream)?;
    let (operation, payload) = request.split_first().context("broker request is empty")?;
    ensure!(*operation == CLAIM, "broker expected a claim request");
    authorizer.recheck_active_uid(peer.uid, &grant)?;
    if *fallback_running {
        fallback.handoff_owner_with_reason(StopReason::Replaced)?;
        *fallback_running = false;
    }
    let (input_state, backlight) = {
        let hardware = fallback.hardware_mut();
        (hardware.input_state(), hardware.get_backlight()?)
    };
    ensure!(payload.is_empty(), "broker claim has an unexpected payload");
    let mut body = Vec::new();
    encode_input_state(&mut body, input_state);
    body.extend_from_slice(&backlight.to_bits().to_be_bytes());
    write_message(&mut stream, OK, &body)?;
    let mut session_revoked = false;

    loop {
        let request = match read_message(&mut stream) {
            Ok(request) => request,
            Err(error) if is_disconnect(&error) => break,
            Err(error) => return Err(error),
        };
        let (operation, payload) = request.split_first().context("broker request is empty")?;
        if session_revoked {
            if *operation == EMIT_KEYS {
                let events = decode_key_events(payload)?;
                ensure!(
                    events
                        .iter()
                        .all(|event| !event.active
                            && client_state.held_keys.keys.contains(&event.key)),
                    "revoked worker attempted new synthetic output"
                );
                fallback.hardware_mut().emit_key_events(&events)?;
                client_state.held_keys.observe(&events);
                write_message(&mut stream, OK, &[])?;
                continue;
            }
            if *operation != LOGOUT_COMPLETE {
                write_message(&mut stream, SESSION_REVOKED, &[])?;
                continue;
            }
            ensure!(payload.is_empty(), "logout acknowledgement has a payload");
            write_message(&mut stream, OK, &[])?;
            break;
        }
        if let Err(error) = authorizer.recheck_active_uid(peer.uid, &grant) {
            let reason = if authorizer.active_session(seat)?.is_some() {
                REVOKED_REPLACED
            } else {
                REVOKED_LOGOUT
            };
            let mut revocation = vec![reason];
            revocation.extend(encode_events(&client_state.contacts.cancel())?);
            write_message(&mut stream, SESSION_REVOKED, &revocation)?;
            session_revoked = true;
            eprintln!("broker revoked user session: {error:#}");
            continue;
        }
        let response = (match *operation {
            POLL => {
                ensure!(payload.len() == 8, "broker poll request is malformed");
                let timeout = u64::from_be_bytes(payload.try_into()?).min(u64::from(u32::MAX));
                let events = fallback
                    .hardware_mut()
                    .poll(Duration::from_millis(timeout))?;
                client_state.contacts.observe(&events);
                encode_events(&events)
            }
            PRESENT => {
                let frame = decode_frame(payload)?;
                fallback.hardware_mut().present(&frame)?;
                Ok(Vec::new())
            }
            CONFIRM => {
                ensure!(
                    payload.is_empty(),
                    "broker confirm has an unexpected payload"
                );
                Ok(Vec::new())
            }
            EMIT_KEYS => {
                let events = decode_key_events(payload)?;
                fallback.hardware_mut().emit_key_events(&events)?;
                client_state.held_keys.observe(&events);
                Ok(Vec::new())
            }
            GET_BACKLIGHT => Ok(fallback
                .hardware_mut()
                .get_backlight()?
                .to_bits()
                .to_be_bytes()
                .to_vec()),
            SET_BACKLIGHT => {
                ensure!(payload.len() == 8, "broker backlight request is malformed");
                fallback
                    .hardware_mut()
                    .set_backlight(f64::from_bits(u64::from_be_bytes(payload.try_into()?)))?;
                Ok(Vec::new())
            }
            RELEASE => {
                ensure!(
                    payload.is_empty(),
                    "broker release has an unexpected payload"
                );
                write_message(&mut stream, OK, &[])?;
                break;
            }
            other => bail!("unknown broker request {other}"),
        })?;
        write_message(&mut stream, OK, &response)?;
    }
    Ok(())
}

fn ensure_supervisor_peer(pid: libc::pid_t) -> Result<()> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .with_context(|| format!("reading the supervisor cgroup for peer {pid}"))?;
    ensure!(
        cgroup
            .lines()
            .any(|line| line.ends_with("/sliver-supervisor.service")),
        "broker peer is not the Sliver user supervisor"
    );
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    ensure!(
        executable.file_name() == Some(std::ffi::OsStr::new("sliver-supervisor")),
        "broker peer executable is not sliver-supervisor"
    );
    Ok(())
}

fn is_disconnect(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
            )
        })
    })
}

fn write_message(stream: &mut UnixStream, operation: u8, payload: &[u8]) -> Result<()> {
    let length = 1usize
        .checked_add(payload.len())
        .context("broker message length overflow")?;
    ensure!(length <= MAX_MESSAGE_BYTES, "broker message is too large");
    stream.write_all(&u32::try_from(length)?.to_be_bytes())?;
    stream.write_all(&[operation])?;
    stream.write_all(payload)?;
    Ok(())
}

fn read_message(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(
        length > 0 && length <= MAX_MESSAGE_BYTES,
        "invalid broker message length"
    );
    let mut message = vec![0; length];
    stream.read_exact(&mut message)?;
    Ok(message)
}

fn put_u32(output: &mut Vec<u8>, value: usize) -> Result<()> {
    output.extend_from_slice(&u32::try_from(value)?.to_be_bytes());
    Ok(())
}

fn read_u32(reader: &mut &[u8]) -> Result<usize> {
    ensure!(reader.len() >= 4, "broker payload is truncated");
    let value = u32::from_be_bytes(reader[..4].try_into()?) as usize;
    *reader = &reader[4..];
    Ok(value)
}

fn encode_input_state(output: &mut Vec<u8>, state: InputState) {
    output.push(u8::from(state.fn_active));
    for modifier in Modifier::ALL {
        output.push(u8::from(state.modifiers.is_active(modifier)));
    }
}

fn decode_input_state(reader: &mut &[u8]) -> Result<InputState> {
    ensure!(!reader.is_empty(), "broker input state is truncated");
    let fn_active = reader[0] != 0;
    *reader = &reader[1..];
    let mut modifiers = ModifierState::default();
    for modifier in Modifier::ALL {
        ensure!(!reader.is_empty(), "broker modifier state is truncated");
        modifiers.set(modifier, reader[0] != 0);
        *reader = &reader[1..];
    }
    Ok(InputState {
        fn_active,
        modifiers,
    })
}

fn decode_claim(payload: &[u8]) -> Result<(InputState, f64)> {
    let mut reader = payload;
    let input = decode_input_state(&mut reader)?;
    ensure!(reader.len() == 8, "broker claim response is malformed");
    Ok((
        input,
        f64::from_bits(u64::from_be_bytes(reader.try_into()?)),
    ))
}

fn encode_events(events: &[HardwareEvent]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    put_u32(&mut output, events.len())?;
    for event in events {
        match event {
            HardwareEvent::Fn { active } => {
                output.push(0);
                output.push(u8::from(*active));
            }
            HardwareEvent::Modifier { modifier, active } => {
                output.push(1);
                output.push(modifier.index() as u8);
                output.push(u8::from(*active));
            }
            HardwareEvent::Touch(event) => {
                output.push(2);
                encode_touch(&mut output, event)?;
            }
            HardwareEvent::Device { present } => {
                output.push(3);
                output.push(u8::from(*present));
            }
            HardwareEvent::Visibility { visible } => {
                output.push(4);
                output.push(u8::from(*visible));
            }
        }
    }
    Ok(output)
}

fn decode_events(payload: &[u8]) -> Result<Vec<HardwareEvent>> {
    let mut reader = payload;
    let count = read_u32(&mut reader)?;
    ensure!(count <= 4096, "broker event batch is too large");
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        ensure!(!reader.is_empty(), "broker event is truncated");
        let kind = reader[0];
        reader = &reader[1..];
        events.push(match kind {
            0 => HardwareEvent::Fn {
                active: read_bool(&mut reader)?,
            },
            1 => {
                ensure!(!reader.is_empty(), "broker modifier event is truncated");
                let index = reader[0] as usize;
                reader = &reader[1..];
                let modifier = *Modifier::ALL
                    .get(index)
                    .context("invalid broker modifier")?;
                HardwareEvent::Modifier {
                    modifier,
                    active: read_bool(&mut reader)?,
                }
            }
            2 => HardwareEvent::Touch(decode_touch(&mut reader)?),
            3 => HardwareEvent::Device {
                present: read_bool(&mut reader)?,
            },
            4 => HardwareEvent::Visibility {
                visible: read_bool(&mut reader)?,
            },
            other => bail!("unknown broker event {other}"),
        });
    }
    ensure!(reader.is_empty(), "broker event payload has trailing bytes");
    Ok(events)
}

fn read_bool(reader: &mut &[u8]) -> Result<bool> {
    ensure!(!reader.is_empty(), "broker boolean is truncated");
    let value = reader[0];
    *reader = &reader[1..];
    ensure!(value <= 1, "broker boolean is invalid");
    Ok(value != 0)
}

fn encode_touch(output: &mut Vec<u8>, event: &TouchEvent) -> Result<()> {
    output.push(match event.phase {
        TouchPhase::Down => 0,
        TouchPhase::Move => 1,
        TouchPhase::Up => 2,
        TouchPhase::Cancel => 3,
    });
    output.extend_from_slice(&event.id.to_be_bytes());
    for value in [event.time, event.x, event.y] {
        output.extend_from_slice(&value.to_bits().to_be_bytes());
    }
    encode_input_state(
        output,
        InputState {
            fn_active: false,
            modifiers: event.modifiers,
        },
    );
    for value in [event.pressure, event.width, event.height] {
        match value {
            Some(value) => {
                output.push(1);
                output.extend_from_slice(&value.to_bits().to_be_bytes());
            }
            None => output.push(0),
        }
    }
    Ok(())
}

fn read_f64(reader: &mut &[u8], context: &str) -> Result<f64> {
    ensure!(reader.len() >= 8, "{context} is truncated");
    let value = f64::from_bits(u64::from_be_bytes(reader[..8].try_into()?));
    *reader = &reader[8..];
    Ok(value)
}

fn decode_touch(reader: &mut &[u8]) -> Result<TouchEvent> {
    ensure!(!reader.is_empty(), "broker touch event is truncated");
    let phase = match reader[0] {
        0 => TouchPhase::Down,
        1 => TouchPhase::Move,
        2 => TouchPhase::Up,
        3 => TouchPhase::Cancel,
        value => bail!("unknown broker touch phase {value}"),
    };
    *reader = &reader[1..];
    ensure!(reader.len() >= 4, "broker touch ID is truncated");
    let id = u32::from_be_bytes(reader[..4].try_into()?);
    *reader = &reader[4..];
    let time = read_f64(reader, "broker touch time")?;
    let x = read_f64(reader, "broker touch x coordinate")?;
    let y = read_f64(reader, "broker touch y coordinate")?;
    let state = decode_input_state(reader)?;
    let pressure = if read_bool(reader)? {
        Some(read_f64(reader, "broker touch pressure")?)
    } else {
        None
    };
    let width = if read_bool(reader)? {
        Some(read_f64(reader, "broker touch width")?)
    } else {
        None
    };
    let height = if read_bool(reader)? {
        Some(read_f64(reader, "broker touch height")?)
    } else {
        None
    };
    Ok(TouchEvent {
        phase,
        id,
        time,
        x,
        y,
        modifiers: state.modifiers,
        pressure,
        width,
        height,
    })
}

fn encode_key_event(output: &mut Vec<u8>, event: SyntheticKeyEvent) {
    match event.key {
        OutputKey::Keyboard(key) => {
            output.push(0);
            output.push(key as u8);
        }
        OutputKey::Consumer(key) => {
            output.push(1);
            output.push(key as u8);
        }
    }
    output.push(u8::from(event.active));
}

fn decode_key_events(payload: &[u8]) -> Result<Vec<SyntheticKeyEvent>> {
    let mut reader = payload;
    let count = read_u32(&mut reader)?;
    ensure!(count <= 4096, "broker key batch is too large");
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        ensure!(reader.len() >= 3, "broker key event is truncated");
        let kind = reader[0];
        let value = reader[1];
        let active = reader[2];
        reader = &reader[3..];
        let key = match kind {
            0 => crate::hardware::KeyboardKey::from_wire(value).map(OutputKey::Keyboard)?,
            1 => crate::hardware::ConsumerKey::from_wire(value).map(OutputKey::Consumer)?,
            other => bail!("unknown broker key kind {other}"),
        };
        ensure!(active <= 1, "broker key state is invalid");
        events.push(SyntheticKeyEvent {
            key,
            active: active != 0,
        });
    }
    ensure!(reader.is_empty(), "broker key payload has trailing bytes");
    Ok(events)
}

fn decode_frame(payload: &[u8]) -> Result<LogicalFrame> {
    let mut reader = payload;
    let width = read_u32(&mut reader)?;
    let height = read_u32(&mut reader)?;
    let stride = read_u32(&mut reader)?;
    ensure!(
        width == sliver_core::STRIP_W as usize,
        "broker frame width is invalid"
    );
    ensure!(
        height == sliver_core::STRIP_H as usize,
        "broker frame height is invalid"
    );
    ensure!(stride >= width * 4, "broker frame stride is invalid");
    ensure!(
        reader.len() == stride * height,
        "broker frame pixels are invalid"
    );
    Ok(LogicalFrame::from_wire(
        width,
        height,
        stride,
        reader.to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Instant;

    use crate::hardware::{
        FakeTouchBar, FrameSnapshot, HardwareEvent, InputState, LogicalFrame, ModifierState,
        SyntheticKeyEvent, TouchBarHardware,
    };
    use crate::logind::{ActiveSession, FakeLogind};
    use crate::lua_worker::LuaSource;
    use crate::path_state::PreparedPathState;

    use super::*;

    #[derive(Clone)]
    struct ThreadFakeHardware(Arc<Mutex<FakeTouchBar>>);

    impl ThreadFakeHardware {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(FakeTouchBar::new())))
        }

        fn inspect<R>(&self, inspect: impl FnOnce(&FakeTouchBar) -> R) -> R {
            inspect(&self.0.lock().expect("test hardware mutex poisoned"))
        }
    }

    impl TouchBarHardware for ThreadFakeHardware {
        fn claim(&mut self) -> Result<()> {
            self.0.lock().expect("test hardware mutex poisoned").claim()
        }

        fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .poll(timeout)
        }

        fn input_state(&self) -> InputState {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .input_state()
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .present(frame)
        }

        fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .emit_key_events(events)
        }

        fn tap_function_key(&mut self, index: usize, modifiers: ModifierState) -> Result<()> {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .tap_function_key(index, modifiers)
        }

        fn get_backlight(&mut self) -> Result<f64> {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .get_backlight()
        }

        fn set_backlight(&mut self, level: f64) -> Result<()> {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .set_backlight(level)
        }

        fn release(&mut self) -> Result<()> {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .release()
        }
    }

    #[test]
    fn real_supervisor_applies_a_lua_worker_through_the_broker_loop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let source = directory.path().join("user.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_session(
            std::process::id() as libc::pid_t,
            Some(crate::logind::Session {
                id: "broker-test-session".into(),
                uid,
                seat: Some(SEAT.into()),
                remote: false,
                active: true,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let broker_state = directory.path().join("broker-state/config-path");
        let user_state = directory.path().join("user-state/config-path");
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                server_shared,
                broker_state,
                server_logind.clone(),
                Some(LuaSource::embedded(
                    b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
                )),
            )?;
            run_broker(
                listener,
                fallback,
                SessionAuthorizer::new(server_logind),
                server_running,
                SEAT,
                false,
            )
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().is_empty())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!shared.inspect(|hardware| hardware.presented_frames().is_empty()));

        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "broker-test-session".into(),
                uid,
            }),
        );
        let mut user = Supervisor::new_with_logind_process(
            BrokerHardware::new_at(socket),
            user_state.clone(),
            FakeLogind::new(),
        )?;
        user.apply(&source)?;
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [255, 0, 0, 255]
        );
        assert_eq!(
            std::fs::read(&user_state)?,
            source.as_os_str().as_encoded_bytes()
        );

        logind.set_active(SEAT, None);
        assert!(user.poll(Duration::ZERO).is_err());
        user.handoff_owner_with_reason(StopReason::Logout)?;
        user.hardware_mut().logout_complete()?;
        user.shutdown()?;

        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().len() < 3)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [0, 255, 0, 255]
        );
        running.store(false, Ordering::Release);
        server.join().expect("broker server panicked")?;
        Ok(())
    }

    #[test]
    fn broker_and_supervisor_restart_reload_the_saved_source_through_the_broker_loop() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("saved.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let user_state = directory.path().join("user-state/config-path");
        PreparedPathState::prepare(&user_state, &source)?.commit()?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_session(
            std::process::id() as libc::pid_t,
            Some(crate::logind::Session {
                id: "restart-session".into(),
                uid,
                seat: Some(SEAT.into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "restart-session".into(),
                uid,
            }),
        );

        let first_socket = directory.path().join("first-broker.sock");
        let first_listener = UnixListener::bind(&first_socket)?;
        let first_shared = ThreadFakeHardware::new();
        let first_running = Arc::new(AtomicBool::new(true));
        let first_broker_state = directory.path().join("first-broker-state/config-path");
        let first_server_logind = logind.clone();
        let first_server_shared = first_shared.clone();
        let first_server_running = first_running.clone();
        let first_server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                first_server_shared,
                first_broker_state,
                first_server_logind.clone(),
                Some(LuaSource::embedded(
                    b"require('sliver.v1'); return { api_version = 1, render = function() end }"
                        .to_vec(),
                )),
            )?;
            run_broker(
                first_listener,
                fallback,
                SessionAuthorizer::new(first_server_logind),
                first_server_running,
                SEAT,
                false,
            )
        });
        let first = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at(first_socket),
            user_state.clone(),
            FakeLogind::new(),
            None,
        )?;
        assert_eq!(
            first_shared.inspect(|hardware| hardware
                .presented_frames()
                .last()
                .unwrap()
                .rgba_at(10, 10)),
            [255, 0, 0, 255]
        );
        first.shutdown()?;
        first_running.store(false, Ordering::Release);
        first_server.join().expect("first broker server panicked")?;

        let second_socket = directory.path().join("second-broker.sock");
        let second_listener = UnixListener::bind(&second_socket)?;
        let second_shared = ThreadFakeHardware::new();
        let second_running = Arc::new(AtomicBool::new(true));
        let second_broker_state = directory.path().join("second-broker-state/config-path");
        let second_server_logind = logind.clone();
        let second_server_shared = second_shared.clone();
        let second_server_running = second_running.clone();
        let second_server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                second_server_shared,
                second_broker_state,
                second_server_logind.clone(),
                Some(LuaSource::embedded(
                    b"require('sliver.v1'); return { api_version = 1, render = function() end }"
                        .to_vec(),
                )),
            )?;
            run_broker(
                second_listener,
                fallback,
                SessionAuthorizer::new(second_server_logind),
                second_server_running,
                SEAT,
                false,
            )
        });
        let second = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at(second_socket),
            user_state,
            FakeLogind::new(),
            None,
        )?;
        assert_eq!(
            second_shared.inspect(|hardware| hardware
                .presented_frames()
                .last()
                .unwrap()
                .rgba_at(10, 10)),
            [255, 0, 0, 255]
        );
        second.shutdown()?;
        second_running.store(false, Ordering::Release);
        second_server
            .join()
            .expect("second broker server panicked")?;
        Ok(())
    }

    #[test]
    fn broker_handoff_keeps_frames_and_restarts_the_fallback_after_logout() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let default = LuaSource::embedded(
            b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
        );
        let shared = SharedHardware::new(FakeTouchBar::new());
        let mut fallback = Supervisor::new_fallback_with_logind(
            shared.clone(),
            directory.path().join("state/config-path"),
            logind.clone(),
            Some(default),
        )?;
        fallback.start_fallback()?;
        let mut fallback_running = true;
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "first-user".into(),
                uid,
            }),
        );
        let client_logind = logind.clone();
        let client_socket = socket.clone();
        let client = thread::spawn(move || -> Result<()> {
            let mut hardware = BrokerHardware::new_at(client_socket);
            hardware.claim()?;
            let stride = sliver_core::STRIP_W as usize * 4;
            let red = [0, 0, 255, 255].repeat(stride * sliver_core::STRIP_H as usize / 4);
            hardware.present(&LogicalFrame::from_wire(
                sliver_core::STRIP_W as usize,
                sliver_core::STRIP_H as usize,
                stride,
                red,
            ))?;
            hardware.confirm_owner()?;
            client_logind.set_active(SEAT, None);
            assert!(hardware.poll(Duration::ZERO).is_err());
            assert_eq!(hardware.revoked_stop_reason(), StopReason::Logout);
            hardware.logout_complete()?;
            Ok(())
        });

        let (stream, _) = listener.accept()?;
        let authorizer = SessionAuthorizer::new(logind.clone());
        handle_client(
            stream,
            &mut fallback,
            &authorizer,
            &mut fallback_running,
            SEAT,
            false,
        )?;
        client.join().expect("broker client panicked")?;
        assert!(!fallback_running);
        fallback.start_fallback()?;

        let frames: Vec<FrameSnapshot> =
            shared.inspect(|hardware| hardware.presented_frames().to_vec());
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].rgba_at(10, 10), [0, 255, 0, 255]);
        assert_eq!(frames[1].rgba_at(10, 10), [255, 0, 0, 255]);
        assert_eq!(frames[2].rgba_at(10, 10), [0, 255, 0, 255]);
        fallback.shutdown()?;
        Ok(())
    }

    #[test]
    fn broker_rejects_a_failed_login_without_replacing_the_fallback_frame() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let shared = SharedHardware::new(FakeTouchBar::new());
        let mut fallback = Supervisor::new_fallback_with_logind(
            shared.clone(),
            directory.path().join("state/config-path"),
            logind.clone(),
            Some(LuaSource::embedded(
                b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
            )),
        )?;
        fallback.start_fallback()?;
        let mut fallback_running = true;
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "failed-login".into(),
                uid,
            }),
        );
        let client_socket = socket.clone();
        let client = thread::spawn(move || -> Result<()> {
            let mut hardware = BrokerHardware::new_at(client_socket);
            hardware.claim()?;
            let _ = hardware.present(&LogicalFrame::from_wire(1, 1, 4, vec![0, 0, 0, 255]));
            Ok(())
        });
        let (stream, _) = listener.accept()?;
        let authorizer = SessionAuthorizer::new(logind.clone());
        let error = handle_client(
            stream,
            &mut fallback,
            &authorizer,
            &mut fallback_running,
            SEAT,
            false,
        )
        .expect_err("failed login handoff was accepted");
        assert!(format!("{error:#}").contains("broker frame width is invalid"));
        client
            .join()
            .expect("failed-login broker client panicked")?;
        assert!(!fallback_running);
        let before_logout: Vec<FrameSnapshot> =
            shared.inspect(|hardware| hardware.presented_frames().to_vec());
        assert_eq!(before_logout.len(), 1);
        assert_eq!(before_logout[0].rgba_at(10, 10), [0, 255, 0, 255]);
        logind.set_active(SEAT, None);
        fallback.start_fallback()?;
        let after_logout: Vec<FrameSnapshot> =
            shared.inspect(|hardware| hardware.presented_frames().to_vec());
        assert_eq!(after_logout.len(), 2);
        assert_eq!(after_logout[1].rgba_at(10, 10), [0, 255, 0, 255]);
        fallback.shutdown()?;
        Ok(())
    }

    #[test]
    fn real_supervisor_failed_login_enters_recovery_through_the_broker_loop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let source = directory.path().join("failed.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); error('failed login candidate')",
        )?;
        let user_state = directory.path().join("user-state/config-path");
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_session(
            std::process::id() as libc::pid_t,
            Some(crate::logind::Session {
                id: "failed-login-session".into(),
                uid,
                seat: Some(SEAT.into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "failed-login-session".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let broker_state = directory.path().join("broker-state/config-path");
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                server_shared,
                broker_state,
                server_logind.clone(),
                Some(LuaSource::embedded(
                    b"require('sliver.v1'); return { api_version = 1, render = function() end }"
                        .to_vec(),
                )),
            )?;
            run_broker(
                listener,
                fallback,
                SessionAuthorizer::new(server_logind),
                server_running,
                SEAT,
                false,
            )
        });
        let mut user = Supervisor::new_with_logind_process(
            BrokerHardware::new_at(socket),
            user_state.clone(),
            FakeLogind::new(),
        )?;
        let error = user
            .apply(&source)
            .expect_err("failed login candidate was accepted");
        assert!(format!("{error:#}").contains("failed login candidate"));
        assert!(!user.has_active_worker());
        assert!(user.has_recovery());
        assert!(user_state.exists());
        user.shutdown()?;
        running.store(false, Ordering::Release);
        server
            .join()
            .expect("failed-login broker server panicked")?;
        Ok(())
    }

    #[test]
    fn broker_switches_multiple_supervisors_through_the_production_loop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let source_a = directory.path().join("user-a.lua");
        let source_b = directory.path().join("user-b.lua");
        std::fs::write(
            &source_a,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        std::fs::write(
            &source_b,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1) end }",
        )?;
        let state_a = directory.path().join("user-a-state/config-path");
        let state_b = directory.path().join("user-b-state/config-path");
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_session(
            std::process::id() as libc::pid_t,
            Some(crate::logind::Session {
                id: "user-a-session".into(),
                uid,
                seat: Some(SEAT.into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "user-a-session".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let broker_state = directory.path().join("broker-state/config-path");
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                server_shared,
                broker_state,
                server_logind.clone(),
                Some(LuaSource::embedded(
                    b"require('sliver.v1'); return { api_version = 1, render = function() end }"
                        .to_vec(),
                )),
            )?;
            run_broker(
                listener,
                fallback,
                SessionAuthorizer::new(server_logind),
                server_running,
                SEAT,
                false,
            )
        });

        let mut first = Supervisor::new_with_logind_process(
            BrokerHardware::new_at(socket.clone()),
            state_a.clone(),
            FakeLogind::new(),
        )?;
        first.apply(&source_a)?;
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [255, 0, 0, 255]
        );
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "user-b-session".into(),
                uid,
            }),
        );
        assert!(first.poll(Duration::ZERO).is_err());
        first.handoff_owner_with_reason(StopReason::Replaced)?;
        first.hardware_mut().logout_complete()?;
        first.shutdown()?;

        let mut second = Supervisor::new_with_logind_process(
            BrokerHardware::new_at(socket),
            state_b.clone(),
            FakeLogind::new(),
        )?;
        second.apply(&source_b)?;
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [0, 0, 255, 255]
        );
        assert_eq!(
            std::fs::read(&state_a)?,
            source_a.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            std::fs::read(&state_b)?,
            source_b.as_os_str().as_encoded_bytes()
        );
        second.shutdown()?;
        running.store(false, Ordering::Release);
        server.join().expect("broker server panicked")?;
        Ok(())
    }

    #[test]
    fn revoked_user_must_acknowledge_lua_cleanup_before_disconnect() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let server = thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener.accept()?;
            let request = read_message(&mut stream)?;
            assert_eq!(request, vec![CLAIM]);
            let mut claim = Vec::new();
            encode_input_state(&mut claim, InputState::default());
            claim.extend_from_slice(&0.75f64.to_bits().to_be_bytes());
            write_message(&mut stream, OK, &claim)?;

            let request = read_message(&mut stream)?;
            assert_eq!(request[0], POLL);
            let mut revocation = vec![REVOKED_LOGOUT];
            revocation.extend(encode_events(&[])?);
            write_message(&mut stream, SESSION_REVOKED, &revocation)?;

            let request = read_message(&mut stream)?;
            assert_eq!(request, vec![LOGOUT_COMPLETE]);
            write_message(&mut stream, OK, &[])?;
            Ok(())
        });

        let mut hardware = BrokerHardware::new_at(socket);
        hardware.claim()?;
        assert!(hardware.poll(Duration::ZERO).is_err());
        assert!(hardware.session_revoked());
        hardware.logout_complete()?;
        server.join().expect("broker test server panicked")?;
        Ok(())
    }
}
