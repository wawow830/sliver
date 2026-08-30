use std::cell::{RefCell, RefMut};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};

use crate::authorization::SessionAuthorizer;
use crate::hardware::{
    function_key_output, tap_key_events, ContactId, HardwareEvent, InputState, LogicalFrame,
    Modifier, ModifierState, OutputKey, SyntheticKeyEvent, TouchBarHardware, TouchEvent,
    TouchPhase,
};
use crate::logind::RealLogind;
use crate::m2_hardware::M2TouchBar;
use crate::supervisor::Supervisor;

const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const CLAIM: u8 = 1;
const POLL: u8 = 2;
const PRESENT: u8 = 3;
const EMIT_KEYS: u8 = 4;
const GET_BACKLIGHT: u8 = 5;
const SET_BACKLIGHT: u8 = 6;
const RELEASE: u8 = 7;
const OK: u8 = 0;
const ERROR: u8 = 1;
const SEAT: &str = "seat0";

pub(crate) fn socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("SLIVER_BROKER_SOCKET") {
        return Ok(PathBuf::from(path));
    }
    Ok(PathBuf::from("/run/sliver/broker.sock"))
}

pub(crate) struct BrokerHardware {
    stream: Option<UnixStream>,
    input_state: InputState,
    claimed: bool,
}

impl BrokerHardware {
    pub(crate) fn new() -> Self {
        Self {
            stream: None,
            input_state: InputState::default(),
            claimed: false,
        }
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
            other => bail!("broker returned unknown status {other}"),
        }
    }
}

impl TouchBarHardware for BrokerHardware {
    fn claim(&mut self) -> Result<()> {
        ensure!(!self.claimed, "broker hardware is already claimed");
        let path = socket_path()?;
        let mut stream = UnixStream::connect(&path)
            .with_context(|| format!("connecting to hardware broker at {}", path.display()))?;
        write_message(&mut stream, CLAIM, &[])?;
        let response = read_message(&mut stream)?;
        let (status, body) = response
            .split_first()
            .context("broker claim response is empty")?;
        ensure!(*status == OK, "{}", String::from_utf8_lossy(body));
        let (input_state, backlight) = decode_claim(body)?;
        self.stream = Some(stream);
        self.input_state = input_state;
        self.claimed = true;
        let _ = backlight;
        Ok(())
    }

    fn poll(&mut self, timeout: Duration) -> Result<Vec<HardwareEvent>> {
        ensure!(self.claimed, "broker hardware is not claimed");
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
        let result = self.request(RELEASE, &[]).map(|_| ());
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

#[derive(Clone)]
pub(crate) struct SharedHardware(Rc<RefCell<M2TouchBar>>);

impl SharedHardware {
    fn new(hardware: M2TouchBar) -> Self {
        Self(Rc::new(RefCell::new(hardware)))
    }

    fn lock(&self) -> Result<RefMut<'_, M2TouchBar>> {
        self.0
            .try_borrow_mut()
            .map_err(|_| anyhow::anyhow!("broker hardware is already in use"))
    }
}

impl TouchBarHardware for SharedHardware {
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
    use std::sync::atomic::{AtomicBool, Ordering};

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
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666))?;

    let shared = SharedHardware::new(M2TouchBar::new());
    let mut fallback = Supervisor::new_fallback(
        shared,
        PathBuf::from("/var/lib/sliver/config-path"),
        Some(crate::default_source::source()),
    )?;
    let authorizer = SessionAuthorizer::new(RealLogind::default());
    let running = Arc::new(AtomicBool::new(true));
    let signal_running = running.clone();
    ctrlc::set_handler(move || signal_running.store(false, Ordering::Release))?;
    let mut fallback_running = fallback.has_active_worker();
    let mut fallback_attempted = true;
    let mut last_active = authorizer.active_session(SEAT)?;

    listener.set_nonblocking(true)?;
    while running.load(Ordering::Acquire) {
        if let Some(stream) = accept_nonblocking(&listener)? {
            let result = handle_client(stream, &mut fallback, &authorizer, &mut fallback_running);
            if let Err(error) = result {
                crate::system_log::broker_error(format!("broker client failed: {error:#}"));
            } else {
                // A client was the user owner. Permit one fresh fallback start
                // after that owner logs out.
                fallback_attempted = false;
            }
            if !fallback_running && authorizer.active_session(SEAT)?.is_none() {
                fallback.start_fallback()?;
                fallback_running = fallback.has_active_worker();
                fallback_attempted = true;
            }
            continue;
        }

        let active = authorizer.active_session(SEAT)?;
        if active != last_active {
            if active.is_none() {
                fallback_attempted = false;
            }
            last_active = active.clone();
        }
        if active.is_some() && fallback_running {
            fallback.handoff_owner()?;
            fallback_attempted = false;
        } else if active.is_none() && !fallback_running && !fallback_attempted {
            fallback.start_fallback()?;
            fallback_attempted = true;
        }
        fallback.poll(Duration::from_millis(50))?;
        fallback_running = fallback.has_active_worker();
    }

    fallback.shutdown()?;
    let _ = std::fs::remove_file(socket);
    Ok(())
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

    fn release(&mut self, hardware: &mut SharedHardware) -> Result<()> {
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

fn handle_client(
    stream: UnixStream,
    fallback: &mut Supervisor<SharedHardware, RealLogind>,
    authorizer: &SessionAuthorizer<RealLogind>,
    fallback_running: &mut bool,
) -> Result<()> {
    let mut held_keys = HeldKeys::default();
    let mut contacts = ActiveContacts::default();
    let result = handle_client_inner(
        stream,
        fallback,
        authorizer,
        fallback_running,
        &mut held_keys,
        &mut contacts,
    );
    let cleanup = held_keys.release(fallback.hardware_mut());
    match (result, cleanup) {
        (Err(error), Err(cleanup_error)) => {
            Err(error).context(format!("broker key cleanup also failed: {cleanup_error:#}"))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("cleaning up broker client keys"),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn handle_client_inner(
    mut stream: UnixStream,
    fallback: &mut Supervisor<SharedHardware, RealLogind>,
    authorizer: &SessionAuthorizer<RealLogind>,
    fallback_running: &mut bool,
    held_keys: &mut HeldKeys,
    contacts: &mut ActiveContacts,
) -> Result<()> {
    let peer = crate::peer_credentials::read(&stream)?;
    let grant = authorizer.authorize(peer)?;
    let request = read_message(&mut stream)?;
    let (operation, payload) = request.split_first().context("broker request is empty")?;
    ensure!(*operation == CLAIM, "broker expected a claim request");
    authorizer.recheck(peer, &grant)?;
    if *fallback_running {
        fallback.handoff_owner()?;
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

    loop {
        let request = match read_message(&mut stream) {
            Ok(request) => request,
            Err(error) if is_disconnect(&error) => break,
            Err(error) => return Err(error),
        };
        let (operation, payload) = request.split_first().context("broker request is empty")?;
        if let Err(error) = authorizer.recheck(peer, &grant) {
            if *operation == POLL {
                let cancellations = contacts.cancel();
                if !cancellations.is_empty() {
                    write_message(&mut stream, OK, &encode_events(&cancellations)?)?;
                }
            }
            return Err(error);
        }
        let response = (match *operation {
            POLL => {
                ensure!(payload.len() == 8, "broker poll request is malformed");
                let timeout = u64::from_be_bytes(payload.try_into()?).min(u64::from(u32::MAX));
                let events = fallback
                    .hardware_mut()
                    .poll(Duration::from_millis(timeout))?;
                contacts.observe(&events);
                encode_events(&events)
            }
            PRESENT => {
                let frame = decode_frame(payload)?;
                fallback.hardware_mut().present(&frame)?;
                Ok(Vec::new())
            }
            EMIT_KEYS => {
                let events = decode_key_events(payload)?;
                fallback.hardware_mut().emit_key_events(&events)?;
                held_keys.observe(&events);
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

fn is_disconnect(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}");
    message.contains("failed to fill whole buffer")
        || message.contains("failed to read")
        || message.contains("early eof")
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
    use std::path::Path;

    use super::*;

    #[test]
    fn broker_socket_can_be_overridden_without_touching_the_apply_socket() {
        std::env::set_var("SLIVER_BROKER_SOCKET", "/tmp/sliver-test-broker.sock");
        assert_eq!(
            socket_path().unwrap(),
            Path::new("/tmp/sliver-test-broker.sock")
        );
        std::env::remove_var("SLIVER_BROKER_SOCKET");
    }
}
