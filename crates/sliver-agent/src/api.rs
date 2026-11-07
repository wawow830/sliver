//! `Control1`, the agent's session D-Bus interface at `io.github.wawow830.Sliver` (names in
//! `sliver_ipc::dbus`, the interface in `docs/dbus/io.github.wawow830.Sliver.xml`, which a test
//! compares with what's served): the user's own tools (sliverctl) driving the agent. Sandboxed
//! callers (Flatpak, Snap) are refused.
//!
//! zbus runs the handlers on its own executor thread; they reach the main loop through a wake
//! socket it polls and a request channel ([`ApiLink`]), or a test's [`Host`].

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sliver_ipc::dbus::{BUS_NAME, ROOT_PATH};

mod control1;
mod prop;

use control1::ControlIface;
pub use prop::{prop_from_value, prop_to_value};

/// A D-Bus value, decoupled from zvariant.
#[derive(Debug, Clone, PartialEq)]
pub enum Prop {
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Prop>),
    Map(BTreeMap<String, Prop>),
}

/// An `a{sv}`.
pub type Props = BTreeMap<String, Prop>;

/// D-Bus errors. Names are checked against `sliver_ipc::dbus::error` in tests.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.wawow830.Sliver.Error")]
pub enum Error {
    #[zbus(error)]
    ZBus(zbus::Error),
    AccessDenied(String),
    InvalidArgs(String),
    Failed(String),
}

/// Requests the agent core answers. Module ids follow sliver-config(5).
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    SwitchLayer(String),
    PushLayer(String, u32),
    PopLayer,
    ToggleLayer(String),
    SetLayout(String),
    SetProfile(String),
    ReloadConfig,
    RenderPreview(String, Props),
    SetVariable(String, Prop),
    Signal(u32),
    Refresh(String),
    Show(String),
    Hide(String),
    Toggle(String),
    ResetVisibility,
    /// A tap, double tap, hold or swipe, as if on the module's cell.
    Trigger(String, sliver_config::Gesture),
    Update(String, Props),
    ListModules,
    /// `show`, `hide` or `toggle` the whole bar (the hatch keeps working).
    Bar(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ControlReply {
    Done,
    Errors(Vec<String>),
    Png(Vec<u8>),
    Modules(Vec<Props>),
}

pub type Reply = Result<ControlReply, String>;

/// `Control1` properties.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControlState {
    pub active_layer: String,
    /// The layout the base layer shows now.
    pub active_layout: String,
    /// Index into the config's `rules` of the rule that picked it, or -1.
    pub active_rule: i32,
    /// That rule's `name` (empty if unnamed or none).
    pub active_rule_name: String,
    /// Why this layout: `rule`, `default` (the layer's default) or `forced` (`SetLayout`).
    /// Empty before the first frame.
    pub layout_source: String,
    pub layers: Vec<String>,
    pub active_profile: String,
    pub profiles: Vec<String>,
    pub variables: Props,
    pub healthy: bool,
    /// Config, stylesheet and module problems now, one per line (`Errors`; empty = none).
    pub errors: Vec<String>,
}

/// What answers `Control1`: the agent ([`ApiLink`]), or a test's stand-in.
pub trait Host: Send + Sync + 'static {
    fn control(&self, req: Control) -> Reply;
    fn control_state(&self) -> ControlState;
    /// This connection got or lost [`BUS_NAME`] (another agent took it with `--replace`, or gave
    /// it back by exiting).
    fn name_owned(&self, _owned: bool) {}
}

/// `Control1`, served.
#[derive(Clone)]
pub struct Api {
    conn: zbus::Connection,
}

impl Api {
    /// Serves on `conn` and requests [`BUS_NAME`]; `NameTaken` if another agent owns it. With
    /// `replace`, takes the name from a running agent (`--replace`), which waits in the bus's
    /// queue and gets it back when this one exits.
    ///
    /// Never replace by default: systemd runs the agent as `Type=dbus`, so a second agent taking
    /// the name and exiting would stop the installed one (zbus's `request_name` both replaces and
    /// allows replacement). No `DoNotQueue`: queueing is what gives a replaced owner the name back.
    pub async fn serve(
        conn: zbus::Connection,
        host: Arc<dyn Host>,
        replace: bool,
    ) -> zbus::Result<Api> {
        use zbus::fdo::{RequestNameFlags, RequestNameReply};
        let api = Self::serve_unnamed(conn, host.clone()).await?;
        let me = api.conn.unique_name().map(|n| n.to_string());
        let mut changes = zbus::fdo::DBusProxy::new(&api.conn)
            .await?
            .receive_name_owner_changed()
            .await?;
        api.conn
            .executor()
            .spawn(
                async move {
                    use futures_lite::StreamExt;
                    while let Some(sig) = changes.next().await {
                        let Ok(args) = sig.args() else { continue };
                        if args.name() == BUS_NAME {
                            let owner = args.new_owner().as_ref().map(|o| o.to_string());
                            host.name_owned(owner.is_some() && owner == me);
                        }
                    }
                },
                "sliver-name-owner-changed",
            )
            .detach();
        let flags = if replace {
            RequestNameFlags::AllowReplacement | RequestNameFlags::ReplaceExisting
        } else {
            RequestNameFlags::AllowReplacement.into()
        };
        match api.conn.request_name_with_flags(BUS_NAME, flags).await? {
            RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => Ok(api),
            RequestNameReply::InQueue | RequestNameReply::Exists => {
                // Don't wait in line: the caller says why and stops.
                let _ = api.conn.release_name(BUS_NAME).await;
                Err(zbus::Error::NameTaken)
            }
        }
    }

    /// Serves without the well-known name (tests running several agents on one bus).
    pub async fn serve_unnamed(conn: zbus::Connection, host: Arc<dyn Host>) -> zbus::Result<Api> {
        conn.object_server()
            .at(ROOT_PATH, ControlIface { host })
            .await?;
        Ok(Api { conn })
    }

    pub fn connection(&self) -> &zbus::Connection {
        &self.conn
    }

    /// The core's `Control1` properties changed.
    pub async fn control_state_changed(&self) -> zbus::Result<()> {
        let iface = self
            .conn
            .object_server()
            .interface::<_, ControlIface>(ROOT_PATH)
            .await?;
        iface
            .get()
            .await
            .state_changed(iface.signal_emitter())
            .await
    }
}

/// Runs in a Flatpak or Snap sandbox (read from `/proc`, never from what the client claims).
fn sandboxed(pid: u32) -> bool {
    let snap = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).is_ok_and(|cg| {
        cg.lines()
            .filter_map(|l| l.strip_prefix("0::"))
            .any(|path| {
                path.rsplit('/')
                    .next()
                    .is_some_and(|l| l.starts_with("snap."))
            })
    });
    snap || std::path::Path::new(&format!("/proc/{pid}/root/.flatpak-info")).exists()
}

const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

/// The agent's [`Host`]: requests wait on a channel for the main loop, which a wake socket
/// rouses.
struct AgentHost {
    wake: UnixStream,
    requests: Mutex<Sender<(Control, Sender<Reply>)>>,
    state: Mutex<ControlState>,
    /// False while a `--replace` agent has the bus name.
    owned: AtomicBool,
}

impl AgentHost {
    fn wake(&self) {
        let _ = (&self.wake).write(&[1]);
    }
}

impl Host for AgentHost {
    fn control(&self, req: Control) -> Reply {
        let (tx, rx) = channel();
        let sent = self
            .requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send((req, tx));
        if sent.is_err() {
            return Err("the agent is shutting down".into());
        }
        self.wake();
        rx.recv_timeout(CONTROL_TIMEOUT)
            .unwrap_or_else(|_| Err("the agent didn't answer in time".into()))
    }

    fn control_state(&self) -> ControlState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn name_owned(&self, owned: bool) {
        if self.owned.swap(owned, Ordering::SeqCst) != owned {
            self.wake();
        }
    }
}

/// The main loop's side of the served API.
pub struct ApiLink {
    pub api: Api,
    host: Arc<AgentHost>,
    wake_rx: UnixStream,
    requests: Receiver<(Control, Sender<Reply>)>,
}

impl ApiLink {
    /// Serves on the session bus as `io.github.wawow830.Sliver`. `NameTaken` if another agent
    /// owns it, unless `replace` (`--replace`).
    pub fn start(replace: bool) -> zbus::Result<ApiLink> {
        // zbus's `blocking` API runs a pool thread that wakes twice a second.
        let conn = zbus::block_on(sliver_bus::session_async())?;
        ApiLink::serve_on(conn, true, replace)
    }

    /// Serves on `conn` (tests use a private bus; `named = false` skips the well-known name).
    pub fn serve(conn: zbus::Connection, named: bool) -> zbus::Result<ApiLink> {
        ApiLink::serve_on(conn, named, false)
    }

    fn serve_on(conn: zbus::Connection, named: bool, replace: bool) -> zbus::Result<ApiLink> {
        let (wake_rx, wake) = UnixStream::pair()?;
        wake_rx.set_nonblocking(true)?;
        wake.set_nonblocking(true)?;
        let (tx, requests) = channel();
        let host = Arc::new(AgentHost {
            wake,
            requests: Mutex::new(tx),
            state: Mutex::default(),
            owned: AtomicBool::new(true),
        });
        let h: Arc<dyn Host> = host.clone();
        let api = zbus::block_on(async {
            if named {
                Api::serve(conn, h, replace).await
            } else {
                Api::serve_unnamed(conn, h).await
            }
        })?;
        Ok(ApiLink {
            api,
            host,
            wake_rx,
            requests,
        })
    }

    /// Readable when [`ApiLink::drain`] has something.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.wake_rx.as_fd()
    }

    /// False while another agent took the name with `--replace`; this one stays off sliverd until
    /// the name comes back, since two agents would revoke each other.
    pub fn owns_name(&self) -> bool {
        self.host.owned.load(Ordering::SeqCst)
    }

    /// The pending requests.
    pub fn drain(&self) -> Vec<(Control, Sender<Reply>)> {
        let mut buf = [0u8; 64];
        while matches!((&self.wake_rx).read(&mut buf), Ok(n) if n > 0) {}
        self.requests.try_iter().collect()
    }

    pub fn set_control_state(&self, s: ControlState) {
        let mut cur = self.host.state.lock().unwrap_or_else(|e| e.into_inner());
        if *cur != s {
            *cur = s;
            drop(cur);
            let _ = zbus::block_on(self.api.control_state_changed());
        }
    }

    /// `Healthy` goes false, the rest stays as it was.
    pub fn set_disconnected(&self) {
        let mut s = self.host.control_state();
        if s.healthy {
            s.healthy = false;
            self.set_control_state(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_names_match_the_spec() {
        use sliver_ipc::dbus::error;
        use zbus::DBusError;
        let name = |e: Error| e.name().as_str().to_owned();
        assert_eq!(
            name(Error::AccessDenied(String::new())),
            error::ACCESS_DENIED
        );
        assert_eq!(name(Error::InvalidArgs(String::new())), error::INVALID_ARGS);
        assert_eq!(name(Error::Failed(String::new())), error::FAILED);
    }

    #[test]
    fn value_roundtrip() {
        let p = Prop::Map(Props::from([
            (
                "a".into(),
                Prop::List(vec![Prop::Int(1), Prop::Str("x".into())]),
            ),
            ("b".into(), Prop::Bytes(vec![1, 2, 3])),
            ("c".into(), Prop::Num(0.5)),
            ("d".into(), Prop::Int(1 << 40)),
        ]));
        assert_eq!(prop_from_value(&prop_to_value(&p)), Some(p));
    }

    #[test]
    fn this_test_is_not_sandboxed() {
        assert_eq!(
            sandboxed(std::process::id()),
            std::path::Path::new("/.flatpak-info").exists()
        );
    }
}
