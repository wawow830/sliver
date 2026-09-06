use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};

use crate::authorization::{
    NoActiveUserSession, SessionAuthorizer, SessionChanged, WorkerNotOwnedByActiveUser,
};
use crate::hardware::{
    ContactId, HardwareCapability, HardwareEvent, InputState, LogicalFrame, Modifier,
    ModifierState, OutputKey, SyntheticKeyEvent, TouchBarHardware, TouchEvent, TouchPhase,
};
use crate::logind::RealLogind;
use crate::lua_worker::StopReason;
use crate::m2_hardware::M2TouchBar;
use crate::supervisor::Supervisor;

const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HARDWARE_POLL_WAIT: Duration = Duration::from_millis(50);
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
const HARDWARE_UNAVAILABLE: u8 = 4;
const WAIT_FOR_HARDWARE: u8 = 5;
const REVOKED_REPLACED: u8 = 0;
const REVOKED_LOGOUT: u8 = 1;
const LOGOUT_COMPLETE: u8 = 9;
const SEAT: &str = "seat0";

#[derive(Clone, Copy)]
enum PeerVerification {
    Production,
    #[cfg(test)]
    Test,
    #[cfg(test)]
    TestProduction,
}

#[derive(Debug)]
struct OwnerFenceFailed;

impl std::fmt::Display for OwnerFenceFailed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("broker could not fence the previous owner's frame")
    }
}

impl std::error::Error for OwnerFenceFailed {}

fn fence_owner_output<H: TouchBarHardware, L: crate::logind::Logind>(
    fallback: &mut Supervisor<H, L>,
) -> Result<()> {
    fallback
        .fence_owner_output()
        .map_err(|error| error.context(OwnerFenceFailed))
}

fn is_owner_fence_failure(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<OwnerFenceFailed>().is_some())
}

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
    hardware_available: bool,
    connection_lost: bool,
    missing_capabilities: BTreeSet<HardwareCapability>,
    unavailable_capability: Option<HardwareCapability>,
    revoked_reason: Option<StopReason>,
    #[cfg(test)]
    test_uid: Option<libc::uid_t>,
}

impl BrokerHardware {
    pub(crate) fn new() -> Self {
        Self {
            stream: None,
            socket: None,
            input_state: InputState::default(),
            claimed: false,
            session_revoked: false,
            hardware_available: true,
            connection_lost: false,
            missing_capabilities: BTreeSet::new(),
            unavailable_capability: None,
            revoked_reason: None,
            #[cfg(test)]
            test_uid: None,
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
            hardware_available: true,
            connection_lost: false,
            missing_capabilities: BTreeSet::new(),
            unavailable_capability: None,
            revoked_reason: None,
            test_uid: None,
        }
    }

    #[cfg(test)]
    // The test transport supplies a second UID because a unit test cannot
    // create a second Unix user. Production always uses SO_PEERCRED.
    fn new_at_as(socket: PathBuf, uid: libc::uid_t) -> Self {
        let mut hardware = Self::new_at(socket);
        hardware.test_uid = Some(uid);
        hardware
    }

    pub(crate) fn session_revoked(&self) -> bool {
        self.session_revoked
    }

    pub(crate) fn revoked_stop_reason(&self) -> StopReason {
        self.revoked_reason.unwrap_or(StopReason::Shutdown)
    }

    fn unavailable_error(&self, body: &[u8]) -> anyhow::Error {
        let capability = self
            .unavailable_capability
            .map(HardwareCapability::name)
            .unwrap_or("hardware contract");
        if body.is_empty() {
            anyhow::anyhow!("hardware interface unavailable: {capability}")
        } else {
            anyhow::anyhow!(
                "hardware interface unavailable: {capability}: {}",
                String::from_utf8_lossy(body)
            )
        }
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
        let result = (|| {
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
                HARDWARE_UNAVAILABLE => {
                    self.hardware_available = false;
                    self.unavailable_capability = body
                        .first()
                        .copied()
                        .and_then(|value| HardwareCapability::from_wire(value).ok());
                    if let Some(capability) = self.unavailable_capability {
                        self.missing_capabilities.insert(capability);
                    }
                    bail!(
                        "{}",
                        self.unavailable_error(&body[usize::from(!body.is_empty())..])
                    )
                }
                other => bail!("broker returned unknown status {other}"),
            }
        })();
        if result.as_ref().err().is_some_and(is_disconnect) {
            // The broker owns the hardware. Once its socket disappears, this
            // supervisor claim is already gone and teardown must not try to
            // paint or release it again.
            self.stream = None;
            self.claimed = false;
            self.hardware_available = false;
            self.connection_lost = true;
        }
        result
    }
}

impl TouchBarHardware for BrokerHardware {
    fn claim(&mut self) -> Result<()> {
        ensure!(!self.claimed, "broker hardware is already claimed");
        let path = self.socket.clone().unwrap_or(socket_path()?);
        let mut stream = UnixStream::connect(&path)
            .with_context(|| format!("connecting to hardware broker at {}", path.display()))?;
        #[cfg(test)]
        let claim_payload = self
            .test_uid
            .map_or_else(Vec::new, |uid| uid.to_be_bytes().to_vec());
        #[cfg(not(test))]
        let claim_payload = Vec::new();
        write_message(&mut stream, CLAIM, &claim_payload)?;
        let response = read_message(&mut stream)?;
        let (status, body) = response
            .split_first()
            .context("broker claim response is empty")?;
        if *status != OK {
            if *status == WAIT_FOR_HARDWARE {
                self.hardware_available = false;
                self.unavailable_capability = body
                    .first()
                    .copied()
                    .and_then(|value| HardwareCapability::from_wire(value).ok());
                if let Some(capability) = self.unavailable_capability {
                    self.missing_capabilities.insert(capability);
                }
                let detail = body
                    .get(1..)
                    .map(String::from_utf8_lossy)
                    .unwrap_or_default();
                return Err(anyhow::anyhow!(detail.into_owned()).context(crate::WaitForHardware));
            }
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
        self.hardware_available = true;
        self.connection_lost = false;
        self.missing_capabilities.clear();
        self.unavailable_capability = None;
        let _ = backlight;
        Ok(())
    }

    fn reacquire(&mut self) -> Result<()> {
        if self.claimed && self.hardware_available {
            return Ok(());
        }
        if self.claimed {
            self.release()?;
        }
        self.claim()
    }

    fn is_available(&self) -> bool {
        self.hardware_available
    }

    fn unavailable_capability(&self) -> Option<HardwareCapability> {
        self.unavailable_capability
    }

    fn session_revoked(&self) -> bool {
        self.session_revoked
    }

    fn connection_lost(&self) -> bool {
        self.connection_lost
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
                    | HardwareEvent::Capability { .. }
                    | HardwareEvent::Visibility { .. } => match *event {
                        HardwareEvent::Device { present } => {
                            if present {
                                self.missing_capabilities.clear();
                                self.unavailable_capability = None;
                            } else {
                                self.missing_capabilities.extend(HardwareCapability::ALL);
                            }
                            self.hardware_available = present;
                        }
                        HardwareEvent::Capability {
                            capability,
                            present,
                        } => {
                            if present {
                                self.missing_capabilities.remove(&capability);
                                self.unavailable_capability =
                                    self.missing_capabilities.iter().next().copied();
                            } else {
                                self.missing_capabilities.insert(capability);
                                self.unavailable_capability = Some(capability);
                            }
                            self.hardware_available = self.missing_capabilities.is_empty();
                        }
                        _ => {}
                    },
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

pub(crate) fn broker_main() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let socket = socket_path()?;
    let directory = socket
        .parent()
        .context("broker socket path has no parent directory")?;
    std::fs::create_dir_all(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o750))?;
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }
    let running = Arc::new(AtomicBool::new(true));
    let signal_running = running.clone();
    ctrlc::set_handler(move || signal_running.store(false, Ordering::Release))?;
    let fallback = loop {
        match Supervisor::new_fallback(
            M2TouchBar::new(),
            PathBuf::from("/var/lib/sliver/config-path"),
        ) {
            Ok(fallback) => break fallback,
            Err(error) if running.load(Ordering::Acquire) => {
                crate::system_log::broker_error(format!(
                    "hardware is not ready; retrying discovery: {error:#}"
                ));
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(_) => {
                let _ = std::fs::remove_file(&socket);
                return Ok(());
            }
        }
    };
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o660))?;
    let authorizer = SessionAuthorizer::new(RealLogind::default());
    let result = run_broker(
        listener,
        fallback,
        authorizer,
        running,
        SEAT,
        PeerVerification::Production,
    );
    let _ = std::fs::remove_file(socket);
    result
}

fn notify_ready() {
    let state = b"READY=1\0";
    // sd_notify returns zero when no notification socket is configured, which
    // is expected when the broker is run outside systemd.
    unsafe {
        ffi::sd_notify(0, state.as_ptr().cast());
    }
}

mod ffi {
    extern "C" {
        pub(super) fn sd_notify(
            unset_environment: libc::c_int,
            state: *const libc::c_char,
        ) -> libc::c_int;
    }
}

fn run_broker<H: TouchBarHardware, L: crate::logind::Logind>(
    listener: UnixListener,
    fallback: Supervisor<H, L>,
    authorizer: SessionAuthorizer<L>,
    running: Arc<AtomicBool>,
    seat: &str,
    peer_verification: PeerVerification,
) -> Result<()> {
    run_broker_with_connection_stop(
        listener,
        fallback,
        authorizer,
        running.clone(),
        seat,
        peer_verification,
        None,
    )
}

#[cfg(test)]
fn run_broker_for_test_with_connection_stop<H: TouchBarHardware, L: crate::logind::Logind>(
    listener: UnixListener,
    fallback: Supervisor<H, L>,
    authorizer: SessionAuthorizer<L>,
    running: Arc<AtomicBool>,
    seat: &str,
    peer_verification: PeerVerification,
    connection_stop: Arc<AtomicBool>,
) -> Result<()> {
    run_broker_with_connection_stop(
        listener,
        fallback,
        authorizer,
        running,
        seat,
        peer_verification,
        Some(connection_stop),
    )
}

fn run_broker_with_connection_stop<H: TouchBarHardware, L: crate::logind::Logind>(
    listener: UnixListener,
    mut fallback: Supervisor<H, L>,
    authorizer: SessionAuthorizer<L>,
    running: Arc<AtomicBool>,
    seat: &str,
    peer_verification: PeerVerification,
    connection_stop: Option<Arc<AtomicBool>>,
) -> Result<()> {
    let initial_active = authorizer.active_session(seat)?;
    let mut fallback_running = false;
    let mut fallback_attempted = false;
    if initial_active.is_none() {
        fallback.start_fallback()?;
        fallback_running = fallback.has_active_worker();
        fallback_attempted = true;
    }
    // With Type=notify, systemd does not advertise the broker as started
    // until hardware discovery, fallback staging, and socket setup complete.
    notify_ready();
    let mut last_active = initial_active;

    listener.set_nonblocking(true)?;
    while running.load(Ordering::Acquire) {
        if let Some(stream) = accept_nonblocking(&listener)? {
            let result = handle_client_with_connection_stop(
                stream,
                &mut fallback,
                &authorizer,
                &mut fallback_running,
                seat,
                peer_verification,
                connection_stop.as_deref(),
                Some(running.as_ref()),
            );
            match result {
                Ok(outcome) => {
                    // A client can disappear without completing the broker's
                    // revocation handshake. The command-mode panel retains
                    // its last frame in that case, so fence it before waiting
                    // for a replacement supervisor. During broker shutdown,
                    // go straight to the adapter's final black/release path.
                    if !outcome.logout_acknowledged && running.load(Ordering::Acquire) {
                        fence_owner_output(&mut fallback)?;
                    }
                    let active = authorizer.active_session(seat)?;
                    fallback_attempted = !outcome.logout_acknowledged && active.is_some();
                    last_active = active;
                }
                Err(error) => {
                    if is_owner_fence_failure(&error) {
                        return Err(error);
                    }
                    crate::system_log::broker_error(format!("broker client failed: {error:#}"));
                    let active = authorizer.active_session(seat)?;
                    fallback_attempted = active.is_some();
                    last_active = active;
                }
            }
            if running.load(Ordering::Acquire)
                && !fallback_running
                && !fallback_attempted
                && authorizer.active_session(seat)?.is_none()
            {
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
        if fallback_running || fallback.has_recovery() || !fallback.is_hardware_available() {
            let was_available = fallback.is_hardware_available();
            fallback.poll(Duration::from_millis(50))?;
            fallback_running = fallback.has_active_worker();
            if !was_available && fallback.is_hardware_available() {
                fallback_attempted = false;
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fallback.shutdown_for_broker()
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
    claimed: bool,
    held_keys: HeldKeys,
    contacts: ActiveContacts,
}

struct ClientOutcome {
    logout_acknowledged: bool,
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

#[allow(clippy::too_many_arguments)]
fn handle_client_with_connection_stop<H: TouchBarHardware, L: crate::logind::Logind>(
    stream: UnixStream,
    fallback: &mut Supervisor<H, L>,
    authorizer: &SessionAuthorizer<L>,
    fallback_running: &mut bool,
    seat: &str,
    peer_verification: PeerVerification,
    connection_stop: Option<&AtomicBool>,
    running: Option<&AtomicBool>,
) -> Result<ClientOutcome> {
    let mut client_state = ClientState::default();
    let result = handle_client_inner(
        stream,
        fallback,
        authorizer,
        fallback_running,
        &mut client_state,
        seat,
        peer_verification,
        connection_stop,
        running,
    );
    let cleanup = client_state.held_keys.release(fallback.hardware_mut());
    let result = match (result, cleanup) {
        (Err(error), Err(cleanup_error)) => {
            Err(error).context(format!("broker key cleanup also failed: {cleanup_error:#}"))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(_outcome), Err(error)) => Err(error).context("cleaning up broker client keys"),
        (Ok(outcome), Ok(())) => Ok(outcome),
    };
    let fence_result = if client_state.claimed
        && result.is_err()
        && running.is_none_or(|running| running.load(Ordering::Acquire))
    {
        fence_owner_output(fallback)
    } else {
        Ok(())
    };
    match (result, fence_result) {
        (Err(error), Err(fence_error)) => {
            Err(fence_error).context(format!("client teardown also failed: {error:#}"))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Ok(_), Err(fence_error)) => Err(fence_error),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_client_inner<H: TouchBarHardware, L: crate::logind::Logind>(
    mut stream: UnixStream,
    fallback: &mut Supervisor<H, L>,
    authorizer: &SessionAuthorizer<L>,
    fallback_running: &mut bool,
    client_state: &mut ClientState,
    seat: &str,
    peer_verification: PeerVerification,
    connection_stop: Option<&AtomicBool>,
    running: Option<&AtomicBool>,
) -> Result<ClientOutcome> {
    let peer = crate::peer_credentials::read(&stream)?;
    let mut stop_input = Vec::new();
    let peer_check = match peer_verification {
        PeerVerification::Production => ensure_supervisor_peer(peer.pid),
        #[cfg(test)]
        PeerVerification::Test => Ok(()),
        #[cfg(test)]
        PeerVerification::TestProduction => ensure_supervisor_test_peer(peer.pid),
    };
    if let Err(error) = peer_check {
        write_client_message(
            &mut stream,
            ERROR,
            error.to_string().as_bytes(),
            connection_stop,
            running,
        )?;
        return Err(error);
    }
    let request = if connection_stop.is_some() || running.is_some() {
        match read_message_until_stop(&mut stream, &mut stop_input, connection_stop, running)? {
            Some(request) => request,
            None => {
                return Ok(ClientOutcome {
                    logout_acknowledged: false,
                })
            }
        }
    } else {
        read_message(&mut stream)?
    };
    let (operation, payload) = request.split_first().context("broker request is empty")?;
    ensure!(*operation == CLAIM, "broker expected a claim request");
    let production_peer = matches!(peer_verification, PeerVerification::Production);
    #[cfg(test)]
    let production_peer =
        production_peer || matches!(peer_verification, PeerVerification::TestProduction);
    let peer_uid = if production_peer {
        ensure!(payload.is_empty(), "broker claim has an unexpected payload");
        peer.uid
    } else if payload.is_empty() {
        peer.uid
    } else {
        ensure!(payload.len() == 4, "test broker claim UID is malformed");
        u32::from_be_bytes(payload.try_into()?) as libc::uid_t
    };
    let grant = match authorizer.authorize_active_uid(peer_uid, seat) {
        Ok(grant) => grant,
        Err(error) => {
            let status = broker_error_status(&error);
            write_client_message(
                &mut stream,
                status,
                error.to_string().as_bytes(),
                connection_stop,
                running,
            )?;
            return Err(error);
        }
    };
    if let Err(error) = authorizer.recheck_active_uid(peer_uid, &grant) {
        // The session can change during this short claim transaction. Tell a
        // supervisor to retry instead of closing the stream with a raw reset.
        let status = broker_error_status(&error);
        write_client_message(
            &mut stream,
            status,
            error.to_string().as_bytes(),
            connection_stop,
            running,
        )?;
        return Err(error);
    }
    if *fallback_running || fallback.has_recovery() {
        fallback.handoff_owner_with_reason(StopReason::Replaced)?;
        *fallback_running = false;
    }
    // Ownership has transferred to this authorized connection even before
    // the claim reply is complete. Fence it on every later setup failure.
    client_state.claimed = true;
    let (input_state, backlight) = {
        let hardware = fallback.hardware_mut();
        let input_state = hardware.input_state();
        let backlight = hardware.get_backlight();
        match backlight {
            Ok(backlight) => (input_state, backlight),
            Err(error) if !hardware.is_available() => {
                let mut body = vec![hardware
                    .unavailable_capability()
                    .unwrap_or(HardwareCapability::Backlight)
                    .to_wire()];
                body.extend_from_slice(error.to_string().as_bytes());
                write_client_message(
                    &mut stream,
                    WAIT_FOR_HARDWARE,
                    &body,
                    connection_stop,
                    running,
                )?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    };
    let mut body = Vec::new();
    encode_input_state(&mut body, input_state);
    body.extend_from_slice(&backlight.to_bits().to_be_bytes());
    write_client_message(&mut stream, OK, &body, connection_stop, running)?;
    let mut session_revoked = false;
    let mut logout_acknowledged = false;

    loop {
        let request = if connection_stop.is_some() || running.is_some() {
            match read_message_until_stop(&mut stream, &mut stop_input, connection_stop, running)? {
                Some(request) => request,
                None => break,
            }
        } else {
            match read_message(&mut stream) {
                Ok(request) => request,
                Err(error) if is_disconnect(&error) => break,
                Err(error) => return Err(error),
            }
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
                write_client_message(&mut stream, OK, &[], connection_stop, running)?;
                continue;
            }
            if *operation != LOGOUT_COMPLETE {
                write_client_message(&mut stream, SESSION_REVOKED, &[], connection_stop, running)?;
                continue;
            }
            ensure!(payload.is_empty(), "logout acknowledgement has a payload");
            write_client_message(&mut stream, OK, &[], connection_stop, running)?;
            logout_acknowledged = true;
            break;
        }
        if let Err(error) = authorizer.recheck_active_uid(peer_uid, &grant) {
            let reason = if authorizer.active_session(seat)?.is_some() {
                REVOKED_REPLACED
            } else {
                REVOKED_LOGOUT
            };
            let mut revocation = vec![reason];
            revocation.extend(encode_events(&client_state.contacts.cancel())?);
            session_revoked = true;
            // The panel retains its last command-mode frame after scanout
            // stops. Fence that frame before notifying the revoked
            // supervisor, so a replacement cannot inherit the old user's
            // pixels if its startup is delayed or fails.
            fence_owner_output(fallback)?;
            write_client_message(
                &mut stream,
                SESSION_REVOKED,
                &revocation,
                connection_stop,
                running,
            )?;
            eprintln!("broker revoked user session: {error:#}");
            continue;
        }
        let response = match *operation {
            POLL => {
                ensure!(payload.len() == 8, "broker poll request is malformed");
                let timeout = u64::from_be_bytes(payload.try_into()?);
                let timeout =
                    Duration::from_millis(timeout.min(MAX_HARDWARE_POLL_WAIT.as_millis() as u64));
                let was_available = fallback.is_hardware_available();
                let mut events = fallback.poll_events(timeout)?;
                if !was_available && fallback.is_hardware_available() {
                    let already_reported = events.iter().any(|event| {
                        matches!(
                            event,
                            HardwareEvent::Device { present: true }
                                | HardwareEvent::Capability { present: true, .. }
                        )
                    });
                    if !already_reported {
                        events.push(HardwareEvent::Device { present: true });
                    }
                }
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
                write_client_message(&mut stream, OK, &[], connection_stop, running)?;
                break;
            }
            other => bail!("unknown broker request {other}"),
        };
        let response = match response {
            Ok(response) => response,
            Err(error) if !fallback.hardware().is_available() => {
                let mut body = vec![fallback
                    .hardware()
                    .unavailable_capability()
                    .unwrap_or(HardwareCapability::Display)
                    .to_wire()];
                body.extend_from_slice(error.to_string().as_bytes());
                write_client_message(
                    &mut stream,
                    HARDWARE_UNAVAILABLE,
                    &body,
                    connection_stop,
                    running,
                )?;
                continue;
            }
            Err(error) => return Err(error),
        };
        write_client_message(&mut stream, OK, &response, connection_stop, running)?;
    }
    Ok(ClientOutcome {
        logout_acknowledged,
    })
}

fn broker_error_status(error: &anyhow::Error) -> u8 {
    if error.downcast_ref::<NoActiveUserSession>().is_some()
        || error.downcast_ref::<SessionChanged>().is_some()
        || error.downcast_ref::<WorkerNotOwnedByActiveUser>().is_some()
    {
        WAIT_FOR_SESSION
    } else {
        ERROR
    }
}

fn ensure_supervisor_peer(pid: libc::pid_t) -> Result<()> {
    ensure_supervisor_peer_at(pid, std::path::Path::new("/proc"))
}

fn ensure_supervisor_peer_at(pid: libc::pid_t, proc_root: &std::path::Path) -> Result<()> {
    let cgroup = std::fs::read_to_string(proc_root.join(pid.to_string()).join("cgroup"))
        .with_context(|| format!("reading the supervisor cgroup for peer {pid}"))?;
    ensure!(
        cgroup
            .lines()
            .any(|line| line.ends_with("/sliver-supervisor.service")),
        "broker peer is not the Sliver user supervisor"
    );
    // The broker is deliberately unprivileged. Linux protects
    // /proc/<peer>/exe from a different UID, so the peer's kernel credentials
    // and systemd cgroup are the production identity boundary.
    Ok(())
}

#[cfg(test)]
fn ensure_supervisor_test_peer(pid: libc::pid_t) -> Result<()> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .with_context(|| format!("reading the supervisor cgroup for peer {pid}"))?;
    ensure!(
        cgroup.lines().any(|line| {
            line.rsplit('/').next().is_some_and(|unit| {
                unit.starts_with("sliver-supervisor-test-") && unit.ends_with(".service")
            })
        }),
        "broker peer is not the test Sliver user supervisor"
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

fn encode_message(operation: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let length = 1usize
        .checked_add(payload.len())
        .context("broker message length overflow")?;
    ensure!(length <= MAX_MESSAGE_BYTES, "broker message is too large");
    let mut message = Vec::with_capacity(4 + length);
    message.extend_from_slice(&u32::try_from(length)?.to_be_bytes());
    message.push(operation);
    message.extend_from_slice(payload);
    Ok(message)
}

fn write_message(stream: &mut UnixStream, operation: u8, payload: &[u8]) -> Result<()> {
    stream.write_all(&encode_message(operation, payload)?)?;
    Ok(())
}

fn write_client_message(
    stream: &mut UnixStream,
    operation: u8,
    payload: &[u8],
    connection_stop: Option<&AtomicBool>,
    running: Option<&AtomicBool>,
) -> Result<()> {
    if connection_stop.is_none() && running.is_none() {
        return write_message(stream, operation, payload);
    }
    let message = encode_message(operation, payload)?;
    let mut written = 0;
    while written < message.len() {
        if connection_stop.is_some_and(|stop| stop.load(Ordering::Acquire))
            || running.is_some_and(|running| !running.load(Ordering::Acquire))
        {
            bail!("broker shutdown interrupted a client response");
        }
        let sent = unsafe {
            libc::send(
                stream.as_raw_fd(),
                message[written..].as_ptr().cast(),
                message.len() - written,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent > 0 {
            written += sent as usize;
            continue;
        }
        if sent == 0 {
            bail!("broker connection closed while writing a response");
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
        let mut pollfd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut pollfd, 1, 10) };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn read_message_until_stop(
    stream: &mut UnixStream,
    input: &mut Vec<u8>,
    connection_stop: Option<&AtomicBool>,
    running: Option<&AtomicBool>,
) -> Result<Option<Vec<u8>>> {
    let mut bytes = [0u8; 8192];
    loop {
        if connection_stop.is_some_and(|stop| stop.load(Ordering::Acquire))
            || running.is_some_and(|running| !running.load(Ordering::Acquire))
        {
            return Ok(None);
        }
        if let Some(message) = take_message(input)? {
            return Ok(Some(message));
        }
        let received = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                libc::MSG_DONTWAIT,
            )
        };
        if received > 0 {
            input.extend_from_slice(&bytes[..received as usize]);
            ensure!(
                input.len() <= MAX_MESSAGE_BYTES + 4,
                "broker message buffer is full"
            );
            continue;
        }
        if received == 0 {
            if input.is_empty() {
                return Ok(None);
            }
            bail!("broker connection closed in the middle of a message");
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
        let mut pollfd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut pollfd, 1, 10) };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().into());
        }
    }
}

fn take_message(input: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    if input.len() < 4 {
        return Ok(None);
    }
    let length = u32::from_be_bytes(input[..4].try_into()?) as usize;
    ensure!(
        length > 0 && length <= MAX_MESSAGE_BYTES,
        "invalid broker message length"
    );
    if input.len() < 4 + length {
        return Ok(None);
    }
    let packet: Vec<_> = input.drain(..4 + length).collect();
    Ok(Some(packet[4..].to_vec()))
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
            HardwareEvent::Capability {
                capability,
                present,
            } => {
                output.push(5);
                output.push(capability.to_wire());
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
            5 => {
                let capability = HardwareCapability::from_wire({
                    ensure!(!reader.is_empty(), "broker capability event is truncated");
                    let value = reader[0];
                    reader = &reader[1..];
                    value
                })?;
                HardwareEvent::Capability {
                    capability,
                    present: read_bool(&mut reader)?,
                }
            }
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
        width == crate::DISPLAY_WIDTH,
        "broker frame width is invalid"
    );
    ensure!(
        height == crate::DISPLAY_HEIGHT,
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
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::Instant;

    use crate::hardware::{
        FakeAction, FakeTouchBar, HardwareEvent, InputState, KeyboardKey, LogicalFrame,
        ModifierState, OutputKey, SyntheticKeyEvent, TouchBarHardware, TouchEvent, TouchPhase,
    };
    use crate::logind::{ActiveSession, FakeLogind, Logind, Session};
    use crate::lua_worker::LuaSource;
    use crate::path_state::PreparedPathState;
    use crate::supervisor::serve_until;

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

        fn inject(&self, event: HardwareEvent) {
            self.0
                .lock()
                .expect("test hardware mutex poisoned")
                .inject(event);
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

    struct SlowPollHardware(FakeTouchBar);

    impl TouchBarHardware for SlowPollHardware {
        fn claim(&mut self) -> Result<()> {
            self.0.claim()
        }

        fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
            thread::sleep(timeout.min(Duration::from_millis(500)));
            self.0.poll(Duration::ZERO)
        }

        fn input_state(&self) -> InputState {
            self.0.input_state()
        }

        fn present(&mut self, frame: &LogicalFrame) -> Result<()> {
            self.0.present(frame)
        }

        fn emit_key_events(&mut self, events: &[SyntheticKeyEvent]) -> Result<()> {
            self.0.emit_key_events(events)
        }

        fn get_backlight(&mut self) -> Result<f64> {
            self.0.get_backlight()
        }

        fn set_backlight(&mut self, level: f64) -> Result<()> {
            self.0.set_backlight(level)
        }

        fn release(&mut self) -> Result<()> {
            self.0.release()
        }
    }

    #[derive(Clone)]
    struct GenerationBumpingLogind {
        inner: FakeLogind,
        bump_on_read: usize,
        generation_reads: Arc<AtomicUsize>,
    }

    impl GenerationBumpingLogind {
        fn new(inner: FakeLogind, bump_on_read: usize) -> Self {
            Self {
                inner,
                bump_on_read,
                generation_reads: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Logind for GenerationBumpingLogind {
        fn generation(&self) -> Result<u64> {
            let read = self.generation_reads.fetch_add(1, Ordering::Relaxed) + 1;
            if read == self.bump_on_read {
                self.inner.bump_generation();
            }
            self.inner.generation()
        }

        fn session_for_pid(&self, pid: libc::pid_t) -> Result<Option<Session>> {
            self.inner.session_for_pid(pid)
        }

        fn active_session(&self, seat: &str) -> Result<Option<ActiveSession>> {
            self.inner.active_session(seat)
        }
    }

    #[test]
    fn an_unrelated_logind_session_event_does_not_revoke_a_stable_claim() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "stable-session".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                server_shared,
                directory.path().join("broker-state/config-path"),
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
                PeerVerification::Test,
            )
        });

        let mut client = BrokerHardware::new_at(socket);
        client.claim()?;

        // A root manager session is unrelated to the active local user on
        // seat0. It must not revoke an otherwise stable broker claim.
        logind.set_session(
            4242,
            Some(Session {
                id: "unrelated-root-session".into(),
                uid: 0,
                seat: None,
                remote: false,
                active: true,
            }),
        );
        client.poll(Duration::ZERO)?;
        client.release()?;

        running.store(false, Ordering::Release);
        server.join().expect("broker server panicked")?;
        Ok(())
    }

    #[test]
    fn failed_claim_recheck_returns_a_protocol_error_instead_of_reset() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let base_logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        let logind = GenerationBumpingLogind::new(base_logind, 3);
        let shared = ThreadFakeHardware::new();
        let server_shared = shared.clone();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let server_logind = logind.clone();
        let server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                server_shared,
                directory.path().join("broker-state/config-path"),
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
                PeerVerification::Test,
            )
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().is_empty())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !shared.inspect(|hardware| hardware.presented_frames().is_empty()),
            "fallback did not present before the user claim"
        );
        logind.inner.set_active(
            SEAT,
            Some(ActiveSession {
                id: "claim-recheck-session".into(),
                uid,
            }),
        );

        let mut client = BrokerHardware::new_at(socket.clone());
        let error = client
            .claim()
            .expect_err("a failed pre-claim recheck reset the broker stream");
        assert!(
            format!("{error:#}").contains("worker session changed during broker request"),
            "unexpected claim error: {error:#}"
        );
        assert!(
            format!("{error:#}").starts_with("waiting for an active local user session:"),
            "claim recheck failure was not marked retryable: {error:#}"
        );

        let mut retry = BrokerHardware::new_at(socket);
        retry.claim()?;
        retry.release()?;

        running.store(false, Ordering::Release);
        server.join().expect("broker server panicked")?;
        Ok(())
    }

    #[test]
    fn production_peer_rejection_is_returned_over_the_broker_protocol() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "peer-rejection-session".into(),
                uid,
            }),
        );
        let server_logind = logind.clone();
        let server_state = directory.path().join("broker-state/config-path");
        let server = thread::spawn(move || -> Result<()> {
            let mut fallback = Supervisor::new_with_logind(
                ThreadFakeHardware::new(),
                server_state,
                server_logind.clone(),
            )?;
            let (stream, _) = listener.accept()?;
            let mut fallback_running = false;
            let result = handle_client_with_connection_stop(
                stream,
                &mut fallback,
                &SessionAuthorizer::new(server_logind),
                &mut fallback_running,
                SEAT,
                PeerVerification::Production,
                None,
                None,
            );
            let error = match result {
                Ok(_) => {
                    return Err(anyhow::anyhow!(
                        "the test process was accepted as a production supervisor"
                    ))
                }
                Err(error) => error,
            };
            assert!(format!("{error:#}").contains("not the Sliver user supervisor"));
            fallback.shutdown()
        });

        let mut client = BrokerHardware::new_at(socket);
        let error = client
            .claim()
            .expect_err("a production peer rejection closed the stream without a reply");
        assert!(format!("{error:#}").contains("not the Sliver user supervisor"));

        server.join().expect("broker server panicked")?;
        Ok(())
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
                PeerVerification::Test,
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
        while !shared.inspect(|hardware| {
            hardware
                .presented_frames()
                .last()
                .is_some_and(|frame| frame.rgba_at(10, 10) == [0, 255, 0, 255])
        }) && Instant::now() < deadline
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
    fn broker_restores_fallback_when_logout_races_with_release() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let server_state = directory.path().join("broker-state/config-path");
        let server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                server_shared,
                server_state,
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
                PeerVerification::Test,
            )
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().is_empty())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!shared.inspect(|hardware| hardware.presented_frames().is_empty()));

        let mut client = UnixStream::connect(&socket)?;
        // Keep the broker in the client handler so it cannot observe the
        // active session before the connection completes its claim.
        client.write_all(&[0, 0, 0])?;
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "release-race-session".into(),
                uid,
            }),
        );
        client.write_all(&[5, CLAIM])?;
        client.write_all(&uid.to_be_bytes())?;
        let claim = read_message(&mut client)?;
        assert_eq!(claim.first(), Some(&OK));

        // A clean release while active must still establish the session
        // transition that the broker will observe when logout follows.
        write_message(&mut client, RELEASE, &[])?;
        assert_eq!(read_message(&mut client)?, vec![OK]);
        drop(client);
        logind.set_active(SEAT, None);

        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().len() < 3)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            shared.inspect(|hardware| {
                hardware
                    .presented_frames()
                    .last()
                    .expect("fallback frame was not restored")
                    .rgba_at(10, 10)
            }),
            [0, 255, 0, 255]
        );

        // The release authorization can also pass immediately before the
        // session disappears; that path must restore fallback as well.
        let mut raced_client = UnixStream::connect(&socket)?;
        raced_client.write_all(&[0, 0, 0])?;
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "second-release-race-session".into(),
                uid,
            }),
        );
        raced_client.write_all(&[5, CLAIM])?;
        raced_client.write_all(&uid.to_be_bytes())?;
        let claim = read_message(&mut raced_client)?;
        assert_eq!(claim.first(), Some(&OK));
        logind.clear_active_on_generation_read(12, SEAT);
        write_message(&mut raced_client, RELEASE, &[])?;
        assert_eq!(read_message(&mut raced_client)?, vec![OK]);
        drop(raced_client);

        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().len() < 5)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            shared.inspect(|hardware| {
                hardware
                    .presented_frames()
                    .last()
                    .expect("fallback frame was not restored after release race")
                    .rgba_at(10, 10)
            }),
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
                    b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
                )),
            )?;
            run_broker(
                first_listener,
                fallback,
                SessionAuthorizer::new(first_server_logind),
                first_server_running,
                SEAT,
                PeerVerification::Test,
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
                    b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
                )),
            )?;
            run_broker(
                second_listener,
                fallback,
                SessionAuthorizer::new(second_server_logind),
                second_server_running,
                SEAT,
                PeerVerification::Test,
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
    fn live_broker_restart_leaves_the_saved_source_ready_for_reconnect() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("saved.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let user_state = directory.path().join("user-state/config-path");
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_session(
            std::process::id() as libc::pid_t,
            Some(crate::logind::Session {
                id: "live-restart-session".into(),
                uid,
                seat: Some(SEAT.into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "live-restart-session".into(),
                uid,
            }),
        );
        let first_socket = directory.path().join("first-broker.sock");
        let first_listener = UnixListener::bind(&first_socket)?;
        let first_running = Arc::new(AtomicBool::new(true));
        let first_connection_stop = Arc::new(AtomicBool::new(false));
        let first_state = directory.path().join("first-state/config-path");
        let first_logind = logind.clone();
        let first_stop = first_connection_stop.clone();
        let first_server_running = first_running.clone();
        let first_server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                ThreadFakeHardware::new(),
                first_state,
                first_logind.clone(),
                Some(LuaSource::embedded(
                    b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
                )),
            )?;
            run_broker_for_test_with_connection_stop(
                first_listener,
                fallback,
                SessionAuthorizer::new(first_logind),
                first_server_running,
                SEAT,
                PeerVerification::Test,
                first_stop,
            )
        });
        let mut user = Supervisor::new_with_logind_process(
            BrokerHardware::new_at(first_socket),
            user_state.clone(),
            FakeLogind::new(),
        )?;
        user.apply(&source)?;
        first_connection_stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while user.poll(Duration::ZERO).is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        first_running.store(false, Ordering::Release);
        first_server.join().expect("first broker server panicked")?;
        drop(user);

        let second_socket = directory.path().join("second-broker.sock");
        let second_listener = UnixListener::bind(&second_socket)?;
        let second_shared = ThreadFakeHardware::new();
        let second_running = Arc::new(AtomicBool::new(true));
        let second_state = directory.path().join("second-state/config-path");
        let second_logind = logind.clone();
        let second_server_shared = second_shared.clone();
        let second_server_running = second_running.clone();
        let second_server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                second_server_shared,
                second_state,
                second_logind.clone(),
                Some(LuaSource::embedded(
                    b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
                )),
            )?;
            run_broker(
                second_listener,
                fallback,
                SessionAuthorizer::new(second_logind),
                second_server_running,
                SEAT,
                PeerVerification::Test,
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
    fn broker_shutdown_finishes_with_an_idle_supervisor_connection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "shutdown-session".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let (done_sender, done_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let fallback = Supervisor::new_fallback_with_logind(
                    server_shared,
                    directory.path().join("broker-state/config-path"),
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
                    PeerVerification::Test,
                )
            })();
            let _ = done_sender.send(result.is_ok());
            result
        });

        let mut client = BrokerHardware::new_at(socket);
        client.claim()?;
        client.emit_key_events(&[SyntheticKeyEvent {
            key: OutputKey::Keyboard(KeyboardKey::F2),
            active: true,
        }])?;
        running.store(false, Ordering::Release);
        let stopped_while_idle = done_receiver
            .recv_timeout(Duration::from_millis(250))
            .is_ok();
        drop(client);
        server.join().expect("broker server panicked")?;
        assert!(
            stopped_while_idle,
            "broker remained blocked in an idle client connection after shutdown"
        );
        let actions = shared.inspect(|hardware| hardware.actions().to_vec());
        let key_up = actions
            .iter()
            .position(|action| {
                matches!(
                    action,
                    FakeAction::SyntheticKey(event) if !event.active
                )
            })
            .expect("broker did not release the client's held key");
        let hardware_release = actions
            .iter()
            .position(|action| matches!(action, FakeAction::Release))
            .expect("broker did not release fake hardware");
        assert!(
            key_up < hardware_release,
            "broker released hardware before releasing client keys"
        );
        Ok(())
    }

    #[test]
    fn broker_shutdown_interrupts_an_unread_large_client_response() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "unread-response-session".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let (done_sender, done_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let fallback = Supervisor::new_fallback_with_logind(
                    server_shared,
                    directory.path().join("broker-state/config-path"),
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
                    PeerVerification::Test,
                )
            })();
            let _ = done_sender.send(result.is_ok());
            result
        });

        let mut client = UnixStream::connect(&socket)?;
        write_message(&mut client, CLAIM, &[])?;
        assert_eq!(read_message(&mut client)?.first(), Some(&OK));
        for id in 0..4096 {
            shared.inject(HardwareEvent::Touch(TouchEvent {
                phase: TouchPhase::Move,
                id,
                time: f64::from(id),
                x: 1.0,
                y: 1.0,
                modifiers: ModifierState::default(),
                pressure: Some(0.5),
                width: Some(0.5),
                height: Some(0.5),
            }));
        }
        write_message(&mut client, POLL, &0u64.to_be_bytes())?;
        thread::sleep(Duration::from_millis(20));
        running.store(false, Ordering::Release);
        let stopped_while_response_was_unread = done_receiver
            .recv_timeout(Duration::from_millis(500))
            .is_ok();
        drop(client);
        server.join().expect("broker server panicked")?;
        assert!(
            stopped_while_response_was_unread,
            "broker remained blocked writing an unread client response"
        );
        Ok(())
    }

    #[test]
    fn broker_clamps_client_hardware_poll_before_shutdown() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "bounded-poll-session".into(),
                uid,
            }),
        );
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_running = running.clone();
        let (done_sender, done_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let fallback = Supervisor::new_fallback_with_logind(
                    SlowPollHardware(FakeTouchBar::new()),
                    directory.path().join("broker-state/config-path"),
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
                    PeerVerification::Test,
                )
            })();
            let _ = done_sender.send(result.is_ok());
            result
        });

        let mut client = UnixStream::connect(&socket)?;
        write_message(&mut client, CLAIM, &[])?;
        assert_eq!(read_message(&mut client)?.first(), Some(&OK));
        write_message(&mut client, POLL, &u64::MAX.to_be_bytes())?;
        running.store(false, Ordering::Release);
        let stopped = done_receiver
            .recv_timeout(Duration::from_millis(250))
            .is_ok();
        server.join().expect("broker server panicked")?;
        assert!(
            stopped,
            "broker allowed a client poll timeout to exceed its shutdown bound"
        );
        Ok(())
    }

    #[test]
    fn supervisor_shutdown_accepts_a_broker_disconnect() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "disconnect-session".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let connection_stop = Arc::new(AtomicBool::new(false));
        let broker_state = directory.path().join("broker-state/config-path");
        let user_state = directory.path().join("user-state/config-path");
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let server_connection_stop = connection_stop.clone();
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
            run_broker_for_test_with_connection_stop(
                listener,
                fallback,
                SessionAuthorizer::new(server_logind),
                server_running,
                SEAT,
                PeerVerification::Test,
                server_connection_stop,
            )
        });

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
        let mut supervisor = Supervisor::new_with_logind_process(
            BrokerHardware::new_at(socket),
            user_state,
            FakeLogind::new(),
        )?;
        supervisor.apply(&source)?;
        shared.inject(HardwareEvent::Touch(TouchEvent {
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
        supervisor.poll(Duration::ZERO)?;
        assert!(shared.inspect(|hardware| {
            hardware
                .synthetic_keys()
                .last()
                .is_some_and(|event| event.active)
        }));
        connection_stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !supervisor.hardware().connection_lost() && Instant::now() < deadline {
            let _ = supervisor.poll(Duration::ZERO);
            thread::sleep(Duration::from_millis(1));
        }
        anyhow::ensure!(
            supervisor.hardware().connection_lost(),
            "broker connection did not close after the stop request"
        );
        running.store(false, Ordering::Release);
        server.join().expect("broker server panicked")?;
        supervisor.shutdown()?;
        assert!(shared.inspect(|hardware| {
            hardware
                .synthetic_keys()
                .last()
                .is_some_and(|event| !event.active)
        }));
        Ok(())
    }

    #[test]
    fn real_supervisor_failed_login_starts_default_through_the_broker_loop() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let source = directory.path().join("failed.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); error('failed login candidate')",
        )?;
        let user_state = directory.path().join("user-state/config-path");
        PreparedPathState::prepare(&user_state, &source)?.commit()?;
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
                    b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
                )),
            )?;
            run_broker(
                listener,
                fallback,
                SessionAuthorizer::new(server_logind),
                server_running,
                SEAT,
                PeerVerification::Test,
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
                id: "failed-login-session".into(),
                uid,
            }),
        );
        let user = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at(socket),
            user_state.clone(),
            FakeLogind::new(),
            None,
        )?;
        assert!(user.has_active_worker());
        assert!(!user.has_recovery());
        assert_eq!(
            std::fs::read(&user_state)?,
            source.as_os_str().as_encoded_bytes()
        );
        let frames = shared.inspect(|hardware| hardware.presented_frames().to_vec());
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].rgba_at(10, 10), [0, 255, 0, 255]);
        assert_eq!(frames[1].rgba_at(10, 10), [0, 0, 0, 255]);
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
            r#"
            local sliver = require('sliver.v1')
            local key = sliver.input.keys.keyboard.f2
            return {
                api_version = 1,
                touch = function(event)
                    if event.phase == 'down' then sliver.input.key.down(key)
                    elseif event.phase == 'up' then sliver.input.key.up(key) end
                end,
                render = function(canvas)
                    canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1)
                end,
            }
            "#,
        )?;
        std::fs::write(
            &source_b,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 0, 1, 1) end }",
        )?;
        let state_a = directory.path().join("user-a-state/config-path");
        let state_b = directory.path().join("user-b-state/config-path");
        PreparedPathState::prepare(&state_a, &source_a)?.commit()?;
        PreparedPathState::prepare(&state_b, &source_b)?.commit()?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        let second_uid = uid.wrapping_add(1);
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
                    b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec(),
                )),
            )?;
            run_broker(
                listener,
                fallback,
                SessionAuthorizer::new(server_logind),
                server_running,
                SEAT,
                PeerVerification::Test,
            )
        });

        let mut first = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at(socket.clone()),
            state_a.clone(),
            FakeLogind::new(),
            None,
        )?;
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [255, 0, 0, 255]
        );
        shared.inject(HardwareEvent::Touch(TouchEvent {
            phase: TouchPhase::Down,
            id: 1,
            time: 0.0,
            x: 10.0,
            y: 10.0,
            modifiers: ModifierState::default(),
            pressure: None,
            width: None,
            height: None,
        }));
        first.poll(Duration::ZERO)?;
        assert!(shared.inspect(|hardware| hardware
            .synthetic_keys()
            .last()
            .is_some_and(|event| event.active)));
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "user-b-session".into(),
                uid: second_uid,
            }),
        );
        assert!(first.poll(Duration::ZERO).is_err());
        first.handoff_owner_with_reason(StopReason::Replaced)?;
        first.hardware_mut().logout_complete()?;
        assert!(shared.inspect(|hardware| hardware
            .synthetic_keys()
            .last()
            .is_some_and(|event| !event.active)));
        first.shutdown()?;

        let mut second = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at_as(socket.clone(), second_uid),
            state_b.clone(),
            FakeLogind::new(),
            None,
        )?;
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
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "user-a-again-session".into(),
                uid,
            }),
        );
        assert!(second.poll(Duration::ZERO).is_err());
        second.handoff_owner_with_reason(StopReason::Replaced)?;
        second.hardware_mut().logout_complete()?;
        second.shutdown()?;

        let mut third = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at(socket.clone()),
            state_a.clone(),
            FakeLogind::new(),
            None,
        )?;
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [255, 0, 0, 255]
        );

        logind.set_active(SEAT, None);
        assert!(third.poll(Duration::ZERO).is_err());
        third.handoff_owner_with_reason(StopReason::Logout)?;
        third.hardware_mut().logout_complete()?;
        third.shutdown()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().len() < 7)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        let frames = shared.inspect(|hardware| {
            hardware
                .presented_frames()
                .iter()
                .map(|frame| frame.rgba_at(10, 10))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            frames,
            vec![
                [255, 0, 0, 255],
                [0, 0, 0, 255],
                [0, 0, 255, 255],
                [0, 0, 0, 255],
                [255, 0, 0, 255],
                [0, 0, 0, 255],
                [0, 255, 0, 255],
            ]
        );
        running.store(false, Ordering::Release);
        server.join().expect("broker server panicked")?;
        Ok(())
    }

    #[test]
    fn a_revoked_owner_cannot_leave_its_frame_visible_while_the_next_session_starts() -> Result<()>
    {
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
        PreparedPathState::prepare(&state_a, &source_a)?.commit()?;
        PreparedPathState::prepare(&state_b, &source_b)?.commit()?;

        let logind = FakeLogind::new();
        let uid_a = unsafe { libc::getuid() };
        let uid_b = uid_a.wrapping_add(1);
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "session-88".into(),
                uid: uid_a,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                server_shared,
                directory.path().join("broker-state/config-path"),
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
                PeerVerification::Test,
            )
        });

        let mut first = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at_as(socket.clone(), uid_a),
            state_a,
            FakeLogind::new(),
            None,
        )?;
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [255, 0, 0, 255]
        );

        // logind removes session 88 and reports the greeter while session 93
        // is being created. There is no replacement supervisor yet, so the
        // old frame must not remain visible during that interval.
        logind.set_active(SEAT, None);
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "greeter-c404".into(),
                uid: uid_b.wrapping_add(1),
            }),
        );
        assert!(first.poll(Duration::ZERO).is_err());
        first.handoff_owner_with_reason(StopReason::Replaced)?;
        first.hardware_mut().logout_complete()?;
        first.shutdown()?;

        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.inspect(|hardware| hardware.presented_frames().len() < 2)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(1));
        }
        assert_ne!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [255, 0, 0, 255],
            "the revoked user's frame remained visible before the replacement claimed the seat"
        );
        assert!(
            shared.inspect(|hardware| {
                let frame = hardware.presented_frames().last().unwrap();
                (0..2008).any(|x| (0..60).any(|y| frame.rgba_at(x, y) == [255, 255, 255, 255]))
            }),
            "the handoff fence presented a blank frame instead of recovery controls"
        );

        // A supervisor started during the greeter interval must wait and
        // retry. It must not claim the seat or inherit user A's frame.
        let startup_error = match Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at_as(socket.clone(), uid_b),
            state_b.clone(),
            FakeLogind::new(),
            None,
        ) {
            Ok(_) => anyhow::bail!("the second supervisor claimed the greeter session"),
            Err(error) => error,
        };
        assert!(
            format!("{startup_error:#}").contains("waiting for an active local user session"),
            "greeter startup returned the wrong retryable error: {startup_error:#}"
        );

        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "session-93".into(),
                uid: uid_b,
            }),
        );
        let second = Supervisor::new_with_startup_candidate_process(
            BrokerHardware::new_at_as(socket, uid_b),
            state_b,
            FakeLogind::new(),
            None,
        )?;
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().last().unwrap().rgba_at(10, 10)),
            [0, 0, 255, 255]
        );
        second.shutdown()?;
        running.store(false, Ordering::Release);
        server.join().expect("broker server panicked")?;
        Ok(())
    }

    #[test]
    fn service_files_keep_the_broker_and_worker_policy_explicit() {
        let broker = include_str!("../../../systemd/sliver-broker.service");
        let supervisor = include_str!("../../../systemd/user/sliver-supervisor.service");
        let worker = include_str!("../../../systemd/sliver-lua-worker-.service.d/50-defaults.conf");

        for setting in [
            "Type=notify",
            "User=sliver",
            "SupplementaryGroups=sliver-drm sliver-input sliver-backlight",
            "Restart=on-failure",
            "TimeoutStopSec=5s",
            "FinalKillSignal=SIGKILL",
        ] {
            assert!(broker.contains(setting), "broker service lacks {setting}");
        }
        assert!(supervisor.contains("WantedBy=graphical-session.target"));
        assert!(supervisor.contains("ConditionGroup=sliver-supervisors"));
        assert!(supervisor.contains("TimeoutStopSec=5s"));
        assert!(supervisor.contains("FinalKillSignal=SIGKILL"));
        assert!(!supervisor.contains("PartOf=graphical-session.target"));
        assert!(!broker.contains("XDG_RUNTIME_DIR=/run/user/%U"));
        assert!(!broker.contains("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/%U/bus"));
        for setting in [
            "MemoryMax=512M",
            "TasksMax=64",
            "KillMode=control-group",
            "PrivateDevices=yes",
            "DevicePolicy=closed",
            "Restart=no",
        ] {
            assert!(worker.contains(setting), "worker policy lacks {setting}");
        }
    }

    #[test]
    fn production_peer_verification_accepts_a_real_supervisor_unit() -> Result<()> {
        let _systemd_tests = crate::lock_systemd_tests();
        let supervisor = std::env::current_exe()?
            .parent()
            .and_then(|path| path.parent())
            .map(|path| path.join("sliver-supervisor"));
        let supervisor = supervisor.context("supervisor binary is required for this test")?;
        ensure!(
            supervisor.exists(),
            "supervisor binary is required for this test"
        );
        let available = std::process::Command::new("systemd-run")
            .args(["--user", "--wait", "--quiet", "true"])
            .status();
        ensure!(
            available.is_ok_and(|status| status.success()),
            "systemd user manager is required for this test"
        );

        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let source = directory.path().join("supervisor.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let state_home = directory.path().join("state");
        let state_file = state_home.join("sliver/config-path");
        PreparedPathState::prepare(&state_file, &source)?.commit()?;
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "systemd-test".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let broker_state = directory.path().join("broker-state/config-path");
        let supervisor_socket = directory.path().join("supervisor.sock");
        let server_shared = shared.clone();
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
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
                PeerVerification::TestProduction,
            )
        });
        let unit = format!("sliver-supervisor-test-{}.service", std::process::id());
        let mut launcher = std::process::Command::new("systemd-run");
        launcher.args(["--user", "--unit"]);
        launcher
            .arg(&unit)
            .args(["--collect", "--quiet", "--service-type=exec", "--setenv"]);
        launcher.arg(format!("SLIVER_BROKER_SOCKET={}", socket.display()));
        launcher.arg("--setenv");
        launcher.arg(format!(
            "SLIVER_SUPERVISOR_SOCKET={}",
            supervisor_socket.display()
        ));
        launcher.arg("--setenv");
        launcher.arg(format!("XDG_STATE_HOME={}", state_home.display()));
        launcher.arg(&supervisor);
        let mut child = launcher.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while shared.inspect(|hardware| hardware.presented_frames().is_empty())
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        let frame_presented = shared.inspect(|hardware| {
            hardware
                .presented_frames()
                .last()
                .is_some_and(|frame| frame.rgba_at(10, 10) == [255, 0, 0, 255])
        });
        logind.set_active(SEAT, None);
        let deadline = Instant::now() + Duration::from_secs(3);
        while shared.inspect(|hardware| hardware.presented_frames().len() < 2)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "stop"])
            .arg(&unit)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = child.wait();
        running.store(false, Ordering::Release);
        server.join().expect("systemd broker server panicked")?;
        assert!(
            frame_presented,
            "real supervisor did not pass broker peer verification"
        );
        assert!(
            shared.inspect(|hardware| {
                hardware
                    .presented_frames()
                    .last()
                    .is_some_and(|frame| frame.rgba_at(10, 10) == [0, 255, 0, 255])
            }),
            "fallback did not return after production logout"
        );
        Ok(())
    }

    #[test]
    fn systemd_supervisor_restart_starts_default_after_persisted_hung_source() -> Result<()> {
        let _systemd_tests = crate::lock_systemd_tests();
        let available = std::process::Command::new("systemd-run")
            .args(["--user", "--wait", "--quiet", "true"])
            .status();
        anyhow::ensure!(
            available.is_ok_and(|status| status.success()),
            "systemd user manager is required for this test"
        );

        let supervisor_binary = std::env::current_exe()?
            .parent()
            .and_then(|path| path.parent())
            .map(|path| path.join("sliver-supervisor"))
            .context("supervisor binary is required for this test")?;
        anyhow::ensure!(
            supervisor_binary.exists(),
            "supervisor binary is required for this test"
        );

        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("broker.sock");
        let listener = UnixListener::bind(&socket)?;
        let source = directory.path().join("saved.lua");
        let attempts = directory.path().join("attempts");
        std::fs::write(
            &source,
            format!(
                r#"
                local attempts = assert(io.open({attempts:?}, "a"))
                attempts:write("loaded\n")
                attempts:close()
                local sliver = require("sliver.v1")
                sliver.timer.after(0.1, function()
                    while true do end
                end)
                return {{
                    api_version = 1,
                    render = function() end,
                }}
                "#,
                attempts = attempts.to_string_lossy(),
            ),
        )?;
        let state_home = directory.path().join("state");
        let state_file = state_home.join("sliver/config-path");
        PreparedPathState::prepare(&state_file, &source)?.commit()?;

        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "systemd-restart-session".into(),
                uid,
            }),
        );
        let shared = ThreadFakeHardware::new();
        let running = Arc::new(AtomicBool::new(true));
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let server_running = running.clone();
        let broker_state = directory.path().join("broker-state/config-path");
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
                PeerVerification::TestProduction,
            )
        });

        let unit = format!(
            "sliver-supervisor-test-restart-{}.service",
            std::process::id()
        );
        let supervisor_socket = directory.path().join("supervisor.sock");
        let worker_binary = supervisor_binary
            .parent()
            .context("supervisor binary has no parent")?
            .join("sliver-lua-worker");
        let mut launcher = std::process::Command::new("systemd-run");
        launcher.args([
            "--user",
            "--unit",
            &unit,
            "--collect",
            "--quiet",
            "--service-type=exec",
            "--property",
            "KillSignal=SIGINT",
            "--property",
            "Restart=on-failure",
            "--property",
            "TimeoutStopSec=5s",
            "--setenv",
        ]);
        launcher.arg(format!("SLIVER_BROKER_SOCKET={}", socket.display()));
        launcher.args(["--setenv"]);
        launcher.arg(format!(
            "SLIVER_SUPERVISOR_SOCKET={}",
            supervisor_socket.display()
        ));
        launcher.args(["--setenv"]);
        launcher.arg(format!("XDG_STATE_HOME={}", state_home.display()));
        launcher.args(["--setenv"]);
        launcher.arg(format!("SLIVER_LUA_WORKER={}", worker_binary.display()));
        launcher.arg(&supervisor_binary);
        let mut launcher_child = launcher.spawn()?;

        let read_attempts =
            || -> Result<String> { Ok(std::fs::read_to_string(&attempts).unwrap_or_default()) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while read_attempts()?.lines().count() < 1 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(read_attempts()?.lines().count(), 1);
        let failure_state = state_file.with_extension("failure");
        let failure_deadline = Instant::now() + Duration::from_secs(5);
        while !failure_state.exists() && Instant::now() < failure_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            failure_state.exists(),
            "live hung worker did not record failure state"
        );
        let worker_units = || -> Result<usize> {
            let output = std::process::Command::new("systemctl")
                .args(["--user", "list-units", "--all", "--no-legend", "--plain"])
                .output()?;
            anyhow::ensure!(output.status.success(), "listing Lua worker units failed");
            let output = String::from_utf8(output.stdout)?;
            Ok(output
                .lines()
                .filter(|line| line.starts_with("sliver-lua-worker-"))
                .count())
        };
        let worker_deadline = Instant::now() + Duration::from_secs(5);
        while worker_units()? != 0 && Instant::now() < worker_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            worker_units()?,
            0,
            "hung worker unit survived live recovery"
        );
        let default_visible = || {
            shared.inspect(|hardware| {
                hardware.presented_frames().last().is_some_and(|frame| {
                    (0..2008).any(|x| (0..60).any(|y| frame.rgba_at(x, y) == [255, 255, 255, 255]))
                })
            })
        };
        let restart = std::process::Command::new("systemctl")
            .args(["--user", "restart", &unit])
            .status()?;
        anyhow::ensure!(restart.success(), "supervisor systemd restart failed");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !default_visible() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            default_visible(),
            "embedded default did not start after the known hung source failed"
        );
        assert_eq!(read_attempts()?.lines().count(), 1);
        let worker_units_deadline = Instant::now() + Duration::from_secs(2);
        let worker_units = loop {
            let worker_units = std::process::Command::new("systemctl")
                .args(["--user", "list-units", "--all", "--no-legend", "--plain"])
                .output()?;
            anyhow::ensure!(
                worker_units.status.success(),
                "listing Lua worker units failed"
            );
            let worker_units = String::from_utf8(worker_units.stdout)?;
            if worker_units
                .lines()
                .any(|line| line.starts_with("sliver-lua-worker-"))
                || Instant::now() >= worker_units_deadline
            {
                break worker_units;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let worker_unit_count = worker_units
            .lines()
            .filter(|line| line.starts_with("sliver-lua-worker-"))
            .count();
        assert_eq!(
            worker_unit_count, 1,
            "healthy default worker unit was not started"
        );
        assert_eq!(
            std::process::Command::new("systemctl")
                .args(["--user", "show", &unit, "-p", "ActiveState", "--value"])
                .output()?
                .stdout
                .as_slice(),
            b"active\n"
        );
        assert_eq!(
            std::process::Command::new("systemctl")
                .args(["--user", "show", &unit, "-p", "NRestarts", "--value"])
                .output()?
                .stdout
                .as_slice(),
            b"0\n"
        );
        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );

        let stop = std::process::Command::new("systemctl")
            .args(["--user", "stop", &unit])
            .status()?;
        anyhow::ensure!(stop.success(), "supervisor systemd stop failed");
        let _ = launcher_child.wait();
        let worker_units = std::process::Command::new("systemctl")
            .args(["--user", "list-units", "--all", "--no-legend", "--plain"])
            .output()?;
        anyhow::ensure!(
            worker_units.status.success(),
            "listing stopped worker units failed"
        );
        let worker_units = String::from_utf8(worker_units.stdout)?;
        assert!(
            !worker_units
                .lines()
                .any(|line| line.starts_with("sliver-lua-worker-")),
            "default worker unit survived supervisor shutdown"
        );
        running.store(false, Ordering::Release);
        server.join().expect("systemd broker server panicked")?;
        Ok(())
    }

    #[test]
    fn production_peer_verification_does_not_require_proc_exe_access() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let pid = 1234;
        let proc_pid = directory.path().join(pid.to_string());
        std::fs::create_dir(&proc_pid)?;
        std::fs::write(
            proc_pid.join("cgroup"),
            "0::/user.slice/user-1000.slice/user@1000.service/sliver-supervisor.service\n",
        )?;

        // There is intentionally no exe entry: the production broker cannot
        // read it across UIDs, but the service cgroup is still verifiable.
        ensure_supervisor_peer_at(pid, directory.path())
    }

    #[test]
    fn broker_rejects_a_non_supervisor_peer() {
        let error = ensure_supervisor_peer(std::process::id() as libc::pid_t)
            .expect_err("the test process was accepted as a supervisor");
        assert!(format!("{error:#}").contains("not the Sliver user supervisor"));
    }

    #[test]
    fn public_cli_applies_through_real_supervisor_worker_and_fake_hardware() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let supervisor_socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&supervisor_socket)?;
        let broker_state = directory.path().join("broker-state/config-path");
        let source = directory.path().join("user.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let invalid = directory.path().join("invalid.lua");
        std::fs::write(
            &invalid,
            "require('sliver.v1'); return { api_version = 1, render = function() error('render failed') end }",
        )?;

        let uid = unsafe { libc::getuid() };
        let logind = FakeLogind::new();
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "public-cli-session".into(),
                uid,
            }),
        );
        logind.set_session_for_any_pid(Session {
            id: "public-cli-session".into(),
            uid,
            seat: Some(SEAT.into()),
            remote: false,
            active: true,
        });
        let shared = ThreadFakeHardware::new();
        let server_logind = logind.clone();
        let server_shared = shared.clone();
        let running = Arc::new(AtomicBool::new(true));
        let server_running = running.clone();
        let server = thread::spawn(move || -> Result<()> {
            let mut supervisor = Supervisor::new_with_startup_candidate_process(
                server_shared,
                broker_state,
                server_logind,
                Some(LuaSource::embedded(default_source_bytes())),
            )?;
            serve_until(listener, &mut supervisor, server_running)
        });

        let cli_path = std::env::current_exe()?
            .parent()
            .and_then(|path| path.parent())
            .map(|path| path.join("sliver"))
            .context("public CLI binary is required for this test")?;
        ensure!(
            cli_path.exists(),
            "public CLI binary is required for this test"
        );
        let output = std::process::Command::new(&cli_path)
            .arg(&source)
            .env("SLIVER_SUPERVISOR_SOCKET", &supervisor_socket)
            .output()?;
        if !output.status.success() {
            running.store(false, Ordering::Release);
            let server_result = server.join().expect("supervisor server panicked");
            panic!(
                "CLI failed: {:?}; supervisor result: {:?}",
                output, server_result
            );
        }
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert_eq!(
            shared.inspect(|hardware| hardware
                .presented_frames()
                .last()
                .expect("successful CLI apply presented no frame")
                .rgba_at(10, 10)),
            [255, 0, 0, 255]
        );

        let frames_before_invalid = shared.inspect(|hardware| hardware.presented_frames().len());
        let output = std::process::Command::new(&cli_path)
            .arg(&invalid)
            .env("SLIVER_SUPERVISOR_SOCKET", &supervisor_socket)
            .output()?;
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(invalid.to_string_lossy().as_ref()),
            "{stderr}"
        );
        assert!(stderr.contains("[render]"), "{stderr}");
        assert!(stderr.contains("stack traceback"), "{stderr}");
        assert!(stderr.contains("invalid.lua:1"), "{stderr}");
        assert_eq!(
            shared.inspect(|hardware| hardware.presented_frames().len()),
            frames_before_invalid
        );

        let output = std::process::Command::new(&cli_path)
            .env("SLIVER_SUPERVISOR_SOCKET", &supervisor_socket)
            .output()?;
        assert!(
            output.status.success(),
            "default reset failed: {:?}",
            output
        );
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert_eq!(
            shared.inspect(|hardware| hardware
                .presented_frames()
                .last()
                .expect("default reset presented no frame")
                .rgba_at(10, 10)),
            [0, 255, 0, 255]
        );

        running.store(false, Ordering::Release);
        server.join().expect("supervisor server panicked")?;
        Ok(())
    }

    #[test]
    fn public_cli_crosses_broker_supervisor_worker_and_fake_hardware() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let broker_socket = directory.path().join("broker.sock");
        let broker_listener = UnixListener::bind(&broker_socket)?;
        let supervisor_socket = directory.path().join("supervisor.sock");
        let supervisor_listener = UnixListener::bind(&supervisor_socket)?;
        let source = directory.path().join("user.lua");
        std::fs::write(
            &source,
            "require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 1, 0, 0, 1) end }",
        )?;
        let invalid = directory.path().join("invalid.lua");
        std::fs::write(
            &invalid,
            "require('sliver.v1'); return { api_version = 1, render = function() error('render failed') end }",
        )?;

        let uid = unsafe { libc::getuid() };
        let logind = FakeLogind::new();
        logind.set_active(
            SEAT,
            Some(ActiveSession {
                id: "public-cli-broker-session".into(),
                uid,
            }),
        );
        logind.set_session_for_any_pid(Session {
            id: "public-cli-broker-session".into(),
            uid,
            seat: Some(SEAT.into()),
            remote: false,
            active: true,
        });

        let shared = ThreadFakeHardware::new();
        let broker_state = directory.path().join("broker-state/config-path");
        let broker_running = Arc::new(AtomicBool::new(true));
        let broker_logind = logind.clone();
        let broker_shared = shared.clone();
        let broker_stop = broker_running.clone();
        let broker_server = thread::spawn(move || -> Result<()> {
            let fallback = Supervisor::new_fallback_with_logind(
                broker_shared,
                broker_state,
                broker_logind.clone(),
                Some(LuaSource::embedded(default_source_bytes())),
            )?;
            run_broker(
                broker_listener,
                fallback,
                SessionAuthorizer::new(broker_logind),
                broker_stop,
                SEAT,
                PeerVerification::Test,
            )
        });

        let user_running = Arc::new(AtomicBool::new(true));
        let user_logind = logind.clone();
        let user_stop = user_running.clone();
        let user_state = directory.path().join("user-state/config-path");
        let user_server = thread::spawn(move || -> Result<()> {
            let mut supervisor = Supervisor::new_with_startup_candidate_process(
                BrokerHardware::new_at(broker_socket),
                user_state,
                user_logind,
                Some(LuaSource::embedded(default_source_bytes())),
            )?;
            serve_until(supervisor_listener, &mut supervisor, user_stop)
        });

        let cli = std::env::current_exe()?
            .parent()
            .and_then(|path| path.parent())
            .map(|path| path.join("sliver"))
            .context("public CLI binary is required for this test")?;
        ensure!(cli.exists(), "public CLI binary is required for this test");
        let result = (|| -> Result<()> {
            let output = std::process::Command::new(&cli)
                .arg(&source)
                .env("SLIVER_SUPERVISOR_SOCKET", &supervisor_socket)
                .output()?;
            ensure!(output.status.success(), "CLI apply failed: {:?}", output);
            ensure!(output.stdout.is_empty(), "CLI apply wrote to stdout");
            ensure!(output.stderr.is_empty(), "CLI apply wrote to stderr");
            ensure!(
                shared.inspect(|hardware| {
                    hardware
                        .presented_frames()
                        .last()
                        .is_some_and(|frame| frame.rgba_at(10, 10) == [255, 0, 0, 255])
                }),
                "broker did not present the explicit config frame"
            );

            let frames_before_invalid =
                shared.inspect(|hardware| hardware.presented_frames().len());
            let output = std::process::Command::new(&cli)
                .arg(&invalid)
                .env("SLIVER_SUPERVISOR_SOCKET", &supervisor_socket)
                .output()?;
            ensure!(
                output.status.code() == Some(1),
                "invalid CLI apply status: {:?}",
                output
            );
            ensure!(
                output.stdout.is_empty(),
                "invalid CLI apply wrote to stdout"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            ensure!(
                stderr.contains(invalid.to_string_lossy().as_ref()),
                "{stderr}"
            );
            ensure!(stderr.contains("[render]"), "{stderr}");
            ensure!(stderr.contains("stack traceback"), "{stderr}");
            ensure!(stderr.contains("invalid.lua:1"), "{stderr}");
            ensure!(
                shared.inspect(|hardware| hardware.presented_frames().len())
                    == frames_before_invalid,
                "invalid CLI apply replaced the active frame"
            );

            let output = std::process::Command::new(&cli)
                .env("SLIVER_SUPERVISOR_SOCKET", &supervisor_socket)
                .output()?;
            ensure!(
                output.status.success(),
                "default CLI reset failed: {:?}",
                output
            );
            ensure!(output.stdout.is_empty(), "default reset wrote to stdout");
            ensure!(output.stderr.is_empty(), "default reset wrote to stderr");
            ensure!(
                shared.inspect(|hardware| {
                    hardware
                        .presented_frames()
                        .last()
                        .is_some_and(|frame| frame.rgba_at(10, 10) == [0, 255, 0, 255])
                }),
                "broker did not present the embedded default frame"
            );
            Ok(())
        })();

        user_running.store(false, Ordering::Release);
        let user_result = user_server.join().expect("supervisor server panicked");
        broker_running.store(false, Ordering::Release);
        let broker_result = broker_server.join().expect("broker server panicked");
        result?;
        user_result?;
        broker_result?;
        Ok(())
    }

    fn default_source_bytes() -> Vec<u8> {
        b"require('sliver.v1'); return { api_version = 1, render = function(canvas) canvas:rectangle(0, 0, 20, 20, 0, 1, 0, 1) end }".to_vec()
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
