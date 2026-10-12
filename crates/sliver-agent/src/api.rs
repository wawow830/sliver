//! The public D-Bus API (`sliver_api`, cedar's) inside the agent: serving it, and the
//! [`Host`] side that lets D-Bus handlers reach the single-threaded agent core.
//!
//! zbus runs the handlers on its own executor thread. `Host::changed` only sets a flag and pokes
//! a wake socket the main loop polls. `Host::control` hands the request to the main loop and
//! waits (bounded) for its answer, so the core stays single-threaded.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sliver_api::identity::RealProc;
use sliver_api::registry::{AppPolicy, AppSource, ContextId, Focus, Policy};
use sliver_api::service::{Api, Control, ControlReply, ControlState, Host};

/// How long a D-Bus `Control1` call waits for the agent core.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

pub type Reply = Result<ControlReply, String>;

struct AgentHost {
    wake: UnixStream,
    changed: AtomicBool,
    requests: Mutex<Sender<(Control, Sender<Reply>)>>,
    state: Arc<Mutex<ControlState>>,
}

impl AgentHost {
    fn wake(&self) {
        let _ = (&self.wake).write(&[1]);
    }
}

impl Host for AgentHost {
    fn changed(&self) {
        self.changed.store(true, Ordering::SeqCst);
        self.wake();
    }

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
}

/// The served API, from the main loop's side.
pub struct ApiLink {
    pub api: Api,
    host: Arc<AgentHost>,
    wake_rx: UnixStream,
    requests: Receiver<(Control, Sender<Reply>)>,
    on_screen: HashSet<ContextId>,
    display: String,
}

impl ApiLink {
    /// Serves the API on the session bus as `io.github.wawow830.Sliver`.
    pub fn start() -> zbus::Result<ApiLink> {
        // sliver-bus: no zbus `blocking` pool thread waking twice a second (T103).
        let conn = zbus::block_on(sliver_bus::session_async())?;
        ApiLink::serve(conn, true)
    }

    /// Serves on `conn` (tests use a private bus; `named = false` skips the well-known name).
    pub fn serve(conn: zbus::Connection, named: bool) -> zbus::Result<ApiLink> {
        let (wake_rx, wake) = UnixStream::pair()?;
        wake_rx.set_nonblocking(true)?;
        wake.set_nonblocking(true)?;
        let (tx, requests) = channel();
        let host = Arc::new(AgentHost {
            wake,
            changed: AtomicBool::new(true),
            requests: Mutex::new(tx),
            state: Arc::default(),
        });
        let h: Arc<dyn Host> = host.clone();
        let api = zbus::block_on(async {
            if named {
                Api::serve(conn, h, Arc::new(RealProc)).await
            } else {
                Api::serve_unnamed(conn, h, Arc::new(RealProc)).await
            }
        })?;
        Ok(ApiLink {
            api,
            host,
            wake_rx,
            requests,
            on_screen: HashSet::new(),
            display: String::new(),
        })
    }

    /// Readable when [`ApiLink::drain`] has something.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.wake_rx.as_fd()
    }

    /// Whether published items changed since the last call, and the pending `Control1` requests.
    pub fn drain(&self) -> (bool, Vec<(Control, Sender<Reply>)>) {
        let mut buf = [0u8; 64];
        while matches!((&self.wake_rx).read(&mut buf), Ok(n) if n > 0) {}
        (
            self.host.changed.swap(false, Ordering::SeqCst),
            self.requests.try_iter().collect(),
        )
    }

    /// The `Control1` properties (active layer, layers, health).
    pub fn set_control_state(&self, s: ControlState) {
        let mut cur = self.host.state.lock().unwrap_or_else(|e| e.into_inner());
        if *cur != s {
            *cur = s;
            drop(cur);
            let _ = zbus::block_on(self.api.control_state_changed());
        }
    }

    /// Which contexts the last frame drew (`OnScreen`). Sent only when it changes.
    pub fn set_on_screen(&mut self, drawn: HashSet<ContextId>) {
        if drawn != self.on_screen {
            let _ = zbus::block_on(self.api.set_on_screen(&drawn));
            self.on_screen = drawn;
        }
    }

    pub fn set_display(&mut self, state: &str) {
        if state != self.display {
            let _ = zbus::block_on(self.api.set_display_state(state));
            self.display = state.to_owned();
        }
    }

    /// The user's preferences for published items (`[apps]`, `[providers]`, `[api]`). Call it at
    /// startup and on every config reload.
    pub fn set_policy(&self, policy: Policy) {
        self.api.state().policy = policy;
        self.host.changed();
    }

    /// Focus from our own compositor backend (GNOME/KDE push theirs over `Desktop1`).
    pub fn set_focus(&self, focus: Focus) {
        self.api.set_focus(focus);
    }

    pub fn interact(
        &self,
        context: ContextId,
        item: &str,
        what: sliver_api::registry::Interaction,
    ) {
        if let Err(e) = zbus::block_on(self.api.interact(context, item, what)) {
            eprintln!("sliver-agent: event to {item}: {e}");
        }
    }
}

/// `[apps."<app-id>"]`, `[providers."<id>"]` and `[api]` (customization.md §6b) as the API
/// registry's [`Policy`] (api.md §4).
pub fn policy_from_config(config: &sliver_config::Config) -> Policy {
    let app = |p: sliver_config::AppPolicy| AppPolicy {
        source: match (p.controls, p.pin) {
            (false, _) => AppSource::Disabled,
            (true, Some(provider)) => AppSource::Provider(provider),
            (true, None) => AppSource::Auto,
        },
        allow_modal: p.whole_bar,
    };
    let mut per_app: std::collections::HashMap<String, AppPolicy> = config
        .apps
        .keys()
        .filter(|k| k.as_str() != "*")
        .map(|k| {
            (
                k.trim_end_matches(".desktop").to_owned(),
                app(config.app_policy(k)),
            )
        })
        .collect();
    // The defaults every other app gets (`app_policy` of an unlisted id = `"*"` + built-ins).
    per_app.insert("*".into(), app(config.app_policy("*")));
    let mut ordered: Vec<(i64, &String)> = config
        .providers
        .iter()
        .filter_map(|(id, p)| p.order.map(|o| (o, id)))
        .collect();
    ordered.sort();
    Policy {
        allow_app: true,
        allow_target: config.api.target,
        per_app,
        hidden_providers: config
            .providers
            .iter()
            .filter(|(_, p)| !p.show)
            .map(|(id, _)| id.clone())
            .collect(),
        status_order: ordered.into_iter().map(|(_, id)| id.clone()).collect(),
        sandbox_status_allowed: config
            .providers
            .iter()
            .filter(|(_, p)| p.allow)
            .map(|(id, _)| id.clone())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_prefs_become_registry_policy() {
        let loaded = sliver_config::load_str(
            r#"
version = 1
[apps."*"]
whole-bar = false
[apps."org.mozilla.firefox.desktop"]
pin = "org.example.FxTabs"
[apps.foot]
controls = false
whole-bar = true
[providers."org.example.Noisy"]
show = false
[providers."org.flat.App"]
allow = true
order = 2
[providers."org.example.First"]
order = 1
[api]
target = false
"#,
            std::path::Path::new("."),
        );
        assert!(
            loaded.diagnostics.errors.is_empty(),
            "{:?}",
            loaded.diagnostics.errors
        );
        let p = policy_from_config(&loaded.config);
        assert!(!p.allow_target);
        let fx = p.for_app("org.mozilla.Firefox");
        assert_eq!(fx.source, AppSource::Provider("org.example.FxTabs".into()));
        assert!(!fx.allow_modal, "inherits whole-bar = false from \"*\"");
        let foot = p.for_app("FOOT");
        assert_eq!((foot.source, foot.allow_modal), (AppSource::Disabled, true));
        let other = p.for_app("kitty");
        assert_eq!((other.source, other.allow_modal), (AppSource::Auto, false));
        assert!(p.hidden_providers.contains("org.example.Noisy"));
        assert!(p.sandbox_status_allowed.contains("org.flat.App"));
        assert_eq!(p.status_order, vec!["org.example.First", "org.flat.App"]);
    }
}
