//! Who is capturing the microphone, the camera or the screen (the `privacy` module): PipeWire's
//! stream nodes, followed over PipeWire's own socket.
//!
//! Why not `pw-dump -m`: it re-dumps whole objects on every change, which on a desktop with an
//! audio filter chain (Fedora Asahi's) is hundreds of KB a second, about 2 % of a core, all day.
//! Here the registry sends one small message per object that appears or goes away, and only the
//! few stream nodes that could be a capture are bound to learn whether they are *running*.
//! Nothing polls: the thread blocks in `read`. While the bar is off ([`Handle::set_awake`]) it
//! disconnects, and a fresh connection on wake replays everything.
//!
//! The native protocol is small for this use (docs.pipewire.org, "Native protocol"): messages are
//! `{id, opcode << 24 | size, seq, n_fds}` followed by an SPA POD struct.
//!
//! What counts: `Stream/Input/Audio` is a recording app (the microphone), unless it only records
//! what a sink plays (`stream.monitor`, `stream.capture.sink`, or a level meter). `Stream/Input/Video`
//! is an app *receiving* video: a camera when it says `media.role = Camera`, a screen share
//! otherwise. Only streams in the `running` state count. A camera used straight through V4L2,
//! with PipeWire not involved, is invisible here.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};

use crate::Handle;

/// Apps capturing right now, by kind. Sorted, without duplicates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub mic: Vec<String>,
    pub camera: Vec<String>,
    pub screen: Vec<String>,
}

impl Capture {
    pub fn is_empty(&self) -> bool {
        self.mic.is_empty() && self.camera.is_empty() && self.screen.is_empty()
    }

    /// The kinds in use, in a fixed order, each with its apps: what the module draws.
    pub fn kinds(&self) -> Vec<(&'static str, &[String])> {
        [
            ("screen", &self.screen),
            ("camera", &self.camera),
            ("mic", &self.mic),
        ]
        .into_iter()
        .filter(|(_, apps)| !apps.is_empty())
        .map(|(k, apps)| (k, apps.as_slice()))
        .collect()
    }
}

// ---- SPA PODs -------------------------------------------------------------------------------

/// The parts of an SPA POD this client needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Pod {
    None,
    Bool(bool),
    Id(u32),
    Int(i32),
    Long(i64),
    Str(String),
    Struct(Vec<Pod>),
    /// Something else (floats, arrays, objects...): skipped.
    Other,
}

const T_NONE: u32 = 1;
const T_BOOL: u32 = 2;
const T_ID: u32 = 3;
const T_INT: u32 = 4;
const T_LONG: u32 = 5;
const T_STRING: u32 = 8;
const T_STRUCT: u32 = 14;

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// Parses one POD at the start of `b`; returns it and how many bytes it took.
pub fn parse_pod(b: &[u8]) -> Option<(Pod, usize)> {
    let size = u32::from_le_bytes(b.get(0..4)?.try_into().ok()?) as usize;
    let ty = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?);
    let body = b.get(8..8usize.checked_add(size)?)?;
    let total = 8 + align8(size);
    let int = || Some(i32::from_le_bytes(body.get(0..4)?.try_into().ok()?));
    let pod = match ty {
        T_NONE => Pod::None,
        T_BOOL => Pod::Bool(int()? != 0),
        T_ID => Pod::Id(int()? as u32),
        T_INT => Pod::Int(int()?),
        T_LONG => Pod::Long(i64::from_le_bytes(body.get(0..8)?.try_into().ok()?)),
        T_STRING => {
            let end = body.iter().position(|&c| c == 0).unwrap_or(body.len());
            Pod::Str(String::from_utf8_lossy(&body[..end]).into_owned())
        }
        T_STRUCT => {
            let mut items = Vec::new();
            let mut rest = body;
            while !rest.is_empty() {
                let (p, n) = parse_pod(rest)?;
                items.push(p);
                rest = rest.get(n.min(rest.len())..)?;
            }
            Pod::Struct(items)
        }
        _ => Pod::Other,
    };
    Some((pod, total))
}

/// A struct POD under construction.
#[derive(Default)]
pub struct StructBuilder(Vec<u8>);

impl StructBuilder {
    fn put(&mut self, ty: u32, body: &[u8]) {
        self.0.extend((body.len() as u32).to_le_bytes());
        self.0.extend(ty.to_le_bytes());
        self.0.extend(body);
        self.0.resize(align8(self.0.len()), 0);
    }

    pub fn int(mut self, v: i32) -> Self {
        self.put(T_INT, &v.to_le_bytes());
        self
    }

    pub fn string(mut self, s: &str) -> Self {
        let mut body = s.as_bytes().to_vec();
        body.push(0);
        self.put(T_STRING, &body);
        self
    }

    pub fn id(mut self, v: u32) -> Self {
        self.put(T_ID, &v.to_le_bytes());
        self
    }

    pub fn long(mut self, v: i64) -> Self {
        self.put(T_LONG, &v.to_le_bytes());
        self
    }

    pub fn none(mut self) -> Self {
        self.put(T_NONE, &[]);
        self
    }

    /// A POD built elsewhere (a nested struct).
    pub fn pod(mut self, pod: &[u8]) -> Self {
        self.0.extend(pod);
        self.0.resize(align8(self.0.len()), 0);
        self
    }

    /// The finished struct POD.
    pub fn build(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.0.len());
        out.extend((self.0.len() as u32).to_le_bytes());
        out.extend(T_STRUCT.to_le_bytes());
        out.extend(self.0);
        out
    }
}

/// Every scalar in `pod`, depth first: a struct inside a struct is flattened, so a property dict
/// reads the same whether PipeWire nests it or not (`n, k, v, k, v...`).
fn flatten<'a>(pod: &'a Pod, out: &mut Vec<&'a Pod>) {
    match pod {
        Pod::Struct(items) => items.iter().for_each(|p| flatten(p, out)),
        p => out.push(p),
    }
}

// ---- Messages -------------------------------------------------------------------------------

const CORE: u32 = 0;
const CLIENT: u32 = 1;
const REGISTRY: u32 = 2;
const CLIENT_UPDATE_PROPERTIES: u8 = 2;
const CORE_HELLO: u8 = 1;
const CORE_PONG: u8 = 3;
const CORE_GET_REGISTRY: u8 = 5;
const REGISTRY_BIND: u8 = 1;
const EV_CORE_PING: u8 = 2;
const EV_REGISTRY_GLOBAL: u8 = 0;
const EV_REGISTRY_GLOBAL_REMOVE: u8 = 1;
const EV_NODE_INFO: u8 = 0;
const NODE_INTERFACE: &str = "PipeWire:Interface:Node";
const VERSION: i32 = 3;
const STATE_RUNNING: i32 = 3;

fn message(id: u32, opcode: u8, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(16 + body.len());
    m.extend(id.to_le_bytes());
    m.extend(((u32::from(opcode) << 24) | body.len() as u32).to_le_bytes());
    m.extend(seq.to_le_bytes());
    m.extend(0u32.to_le_bytes());
    m.extend(body);
    m
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mic,
    Camera,
    Screen,
}

type Props = BTreeMap<String, String>;

fn is_true(p: &Props, k: &str) -> bool {
    p.get(k).is_some_and(|v| v == "true" || v == "1")
}

/// What kind of capture a node with these properties is, if any.
fn classify(p: &Props) -> Option<Kind> {
    match p.get("media.class")?.as_str() {
        "Stream/Input/Audio" => {
            // Recording what a sink plays, or a level meter: not the microphone.
            let meter = p
                .get("application.id")
                .is_some_and(|a| a == "org.PulseAudio.pavucontrol")
                || p.get("node.name")
                    .is_some_and(|n| n.eq_ignore_ascii_case("peak detect"));
            let wp = p
                .get("application.process.binary")
                .is_some_and(|b| b == "wireplumber");
            // The two halves of a loopback or filter chain (Fedora Asahi's microphone chain is
            // one) are plumbing; the app recording from the chain is what counts.
            let plumbing = p.contains_key("node.link-group");
            (!(is_true(p, "stream.monitor")
                || is_true(p, "stream.capture.sink")
                || meter
                || wp
                || plumbing))
                .then_some(Kind::Mic)
        }
        "Stream/Input/Video" => Some(if p.get("media.role").is_some_and(|r| r == "Camera") {
            Kind::Camera
        } else {
            Kind::Screen
        }),
        _ => None,
    }
}

fn app_name(p: &Props) -> String {
    [
        "application.name",
        "application.process.binary",
        "node.name",
    ]
    .iter()
    .find_map(|k| p.get(*k).filter(|v| !v.is_empty()))
    .cloned()
    .unwrap_or_else(|| "?".into())
}

/// `n, key, value, ...` from a flattened struct, starting at `at`.
fn dict(items: &[&Pod], at: usize) -> Props {
    let Some(Pod::Int(n)) = items.get(at) else {
        return Props::new();
    };
    let mut out = Props::new();
    for i in 0..(*n).max(0) as usize {
        if let (Some(Pod::Str(k)), Some(Pod::Str(v))) =
            (items.get(at + 1 + 2 * i), items.get(at + 2 + 2 * i))
        {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// One PipeWire connection's view of the stream nodes.
struct Tracker {
    stream: UnixStream,
    /// Registry global id -> what we know. Only nodes worth watching are in here.
    nodes: BTreeMap<u32, (Props, Option<i32>)>,
    /// Proxy id we chose when binding -> global id.
    proxies: BTreeMap<u32, u32>,
    next_proxy: u32,
    seq: u32,
}

impl Tracker {
    fn send(&mut self, id: u32, opcode: u8, body: &[u8]) -> io::Result<()> {
        self.seq = self.seq.wrapping_add(1);
        let m = message(id, opcode, self.seq, body);
        self.stream.write_all(&m)
    }

    fn start(&mut self) -> io::Result<()> {
        self.send(
            CORE,
            CORE_HELLO,
            &StructBuilder::default().int(VERSION).build(),
        )?;
        // Say who we are (the client object is always id 1).
        let props = StructBuilder::default()
            .int(2)
            .string("application.name")
            .string("sliver-agent")
            .string("media.category")
            .string("Manager")
            .build();
        self.send(
            CLIENT,
            CLIENT_UPDATE_PROPERTIES,
            &StructBuilder::default().pod(&props).build(),
        )?;
        self.send(
            CORE,
            CORE_GET_REGISTRY,
            &StructBuilder::default()
                .int(VERSION)
                .int(REGISTRY as i32)
                .build(),
        )
    }

    /// Handles one message. `Ok(true)` = the capture set may have changed.
    fn handle(&mut self, id: u32, opcode: u8, body: &[u8]) -> io::Result<bool> {
        let Some((pod, _)) = parse_pod(body) else {
            return Ok(false);
        };
        let mut items = Vec::new();
        flatten(&pod, &mut items);
        match (id, opcode) {
            (CORE, EV_CORE_PING) => {
                // Answer with the same id and seq it sent.
                if let (Some(Pod::Int(pid)), Some(Pod::Int(pseq))) = (items.first(), items.get(1)) {
                    let b = StructBuilder::default().int(*pid).int(*pseq).build();
                    self.send(CORE, CORE_PONG, &b)?;
                }
                Ok(false)
            }
            (REGISTRY, EV_REGISTRY_GLOBAL) => {
                // id, permissions, type, version, props...
                let (Some(Pod::Int(gid)), Some(Pod::Str(ty))) = (items.first(), items.get(2))
                else {
                    return Ok(false);
                };
                if ty != NODE_INTERFACE {
                    return Ok(false);
                }
                let props = dict(&items, 4);
                if classify(&props).is_none() {
                    return Ok(false);
                }
                let gid = *gid as u32;
                self.nodes.insert(gid, (props, None));
                let proxy = self.next_proxy;
                self.next_proxy += 1;
                self.proxies.insert(proxy, gid);
                let b = StructBuilder::default()
                    .int(gid as i32)
                    .string(NODE_INTERFACE)
                    .int(VERSION)
                    .int(proxy as i32)
                    .build();
                self.send(REGISTRY, REGISTRY_BIND, &b)?;
                Ok(false)
            }
            (REGISTRY, EV_REGISTRY_GLOBAL_REMOVE) => {
                let Some(Pod::Int(gid)) = items.first() else {
                    return Ok(false);
                };
                Ok(self.nodes.remove(&(*gid as u32)).is_some())
            }
            (proxy, EV_NODE_INFO) => {
                let Some(gid) = self.proxies.get(&proxy).copied() else {
                    return Ok(false);
                };
                // id, max in, max out, change mask, n in, n out, state, error, props...
                let Some((props, state)) = self.nodes.get_mut(&gid) else {
                    return Ok(false);
                };
                // PipeWire sends the state as an enum, an Id POD.
                match items.get(6) {
                    Some(Pod::Id(s)) => *state = Some(*s as i32),
                    Some(Pod::Int(s)) => *state = Some(*s),
                    _ => {}
                }
                // An update that changes no property carries an empty dict: keep what we had.
                props.extend(dict(&items, 8));
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn capture(&self) -> Capture {
        let mut c = Capture::default();
        for (props, state) in self.nodes.values() {
            if *state != Some(STATE_RUNNING) {
                continue;
            }
            let (Some(kind), app) = (classify(props), app_name(props)) else {
                continue;
            };
            let list = match kind {
                Kind::Mic => &mut c.mic,
                Kind::Camera => &mut c.camera,
                Kind::Screen => &mut c.screen,
            };
            list.push(app);
        }
        for l in [&mut c.mic, &mut c.camera, &mut c.screen] {
            l.sort();
            l.dedup();
        }
        c
    }
}

/// Reads messages until the connection drops, reporting the capture set after each change.
fn run(stream: UnixStream, mut report: impl FnMut(Capture) -> bool) -> io::Result<()> {
    let mut reader = stream.try_clone()?;
    let mut t = Tracker {
        stream,
        nodes: BTreeMap::new(),
        proxies: BTreeMap::new(),
        next_proxy: 3,
        seq: 0,
    };
    t.start()?;
    let mut last = Capture::default();
    loop {
        let mut head = [0u8; 16];
        reader.read_exact(&mut head)?;
        let id = u32::from_le_bytes(head[0..4].try_into().expect("4 bytes"));
        let os = u32::from_le_bytes(head[4..8].try_into().expect("4 bytes"));
        let (opcode, size) = ((os >> 24) as u8, (os & 0x00ff_ffff) as usize);
        let mut body = vec![0u8; size];
        reader.read_exact(&mut body)?;
        if t.handle(id, opcode, &body)? {
            let now = t.capture();
            if now != last {
                last = now.clone();
                if !report(now) {
                    return Ok(());
                }
            }
        }
    }
}

// ---- The thread -----------------------------------------------------------------------------

/// Where PipeWire listens: `$PIPEWIRE_REMOTE` (a name or a path), else `pipewire-0` in
/// `$PIPEWIRE_RUNTIME_DIR`, `$XDG_RUNTIME_DIR`.
pub fn socket_path(get: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let remote = get("PIPEWIRE_REMOTE").filter(|r| !r.is_empty());
    if let Some(r) = &remote
        && r.starts_with('/')
    {
        return Some(r.into());
    }
    let dir = get("PIPEWIRE_RUNTIME_DIR")
        .or_else(|| get("XDG_RUNTIME_DIR"))
        .filter(|d| !d.is_empty())?;
    Some(Path::new(&dir).join(remote.as_deref().unwrap_or("pipewire-0")))
}

/// Shared with [`Handle::set_awake`]: is the bar on, and the live connection to cut when it
/// turns off.
#[derive(Default)]
pub(crate) struct Gate {
    awake: Mutex<bool>,
    changed: Condvar,
    conn: Mutex<Option<UnixStream>>,
}

impl Gate {
    pub(crate) fn new() -> Arc<Gate> {
        Arc::new(Gate {
            awake: Mutex::new(true),
            ..Gate::default()
        })
    }

    pub(crate) fn set(&self, on: bool) {
        *self.awake.lock().unwrap_or_else(|e| e.into_inner()) = on;
        if !on && let Some(c) = self.conn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        self.changed.notify_all();
    }

    fn wait_awake(&self) {
        let mut a = self.awake.lock().unwrap_or_else(|e| e.into_inner());
        while !*a {
            a = self.changed.wait(a).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn is_awake(&self) -> bool {
        *self.awake.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Blocks until `dir` gets a new entry (PipeWire creating its socket): no polling.
fn wait_for_entry(dir: &Path, name: &std::ffi::OsStr) {
    use rustix::fs::inotify::{CreateFlags, WatchFlags, add_watch, init};
    let Ok(fd) = init(CreateFlags::CLOEXEC) else {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    };
    if add_watch(&fd, dir, WatchFlags::CREATE | WatchFlags::MOVED_TO).is_err() {
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    // The socket may have appeared between the failed connect and the watch.
    if dir.join(name).exists() {
        return;
    }
    let mut buf = [0u8; 4096];
    let _ = rustix::io::read(&fd, &mut buf);
}

impl Handle {
    /// Follow who is capturing the microphone, camera or screen, from the PipeWire socket at
    /// `socket`, into [`SessionState::capture`](crate::SessionState). A missing PipeWire is waited
    /// for (inotify), not retried on a timer. See the module docs.
    pub fn start_capture(&self, socket: PathBuf) {
        let hub = self.hub.clone();
        let gate = self.capture_gate.clone();
        std::thread::Builder::new()
            .name("sliver-session-capture".into())
            .spawn(move || {
                let mut delay = std::time::Duration::from_millis(200);
                loop {
                    gate.wait_awake();
                    let stream = match UnixStream::connect(&socket) {
                        Ok(s) => s,
                        Err(_) => {
                            let (Some(dir), Some(name)) = (socket.parent(), socket.file_name())
                            else {
                                return;
                            };
                            wait_for_entry(dir, name);
                            // Not a hot loop if the socket exists but nobody answers.
                            std::thread::sleep(delay);
                            delay = (delay * 2).min(std::time::Duration::from_secs(30));
                            continue;
                        }
                    };
                    *gate.conn.lock().unwrap_or_else(|e| e.into_inner()) = stream.try_clone().ok();
                    // The bar turned off between the wait and the connection being stored.
                    if !gate.is_awake() {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        continue;
                    }
                    let started = std::time::Instant::now();
                    let h = hub.clone();
                    let _ = run(stream, move |c| h.update(|s| s.capture = c));
                    gate.conn.lock().unwrap_or_else(|e| e.into_inner()).take();
                    if !gate.is_awake() {
                        continue; // cut on purpose: reconnect when the bar is back
                    }
                    // The server went away: nobody captures through it any more.
                    if !hub.update(|s| s.capture = Capture::default()) {
                        return;
                    }
                    // A connection that lasted resets the delay; one that dropped at once backs off.
                    if started.elapsed() > std::time::Duration::from_secs(5) {
                        delay = std::time::Duration::from_millis(200);
                    }
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(std::time::Duration::from_secs(30));
                }
            })
            .expect("spawn thread");
    }
}

#[cfg(test)]
mod tests;
