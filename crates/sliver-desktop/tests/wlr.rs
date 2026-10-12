//! The generic Wayland backend against a mock compositor (wayland-server in a thread) that
//! speaks wlr-foreign-toplevel-management and ext-workspace: the initial burst, live changes,
//! and the control path (focus, close, switch workspace) all go over a real Wayland socket.

use sliver_desktop::control::Action;
use sliver_desktop::wlr::{Watcher, send};
use sliver_desktop::{Backend, DesktopSource, DesktopState};
use std::os::fd::AsFd;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use wayland_server::protocol::wl_seat::{self, WlSeat};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, ListeningSocket, New,
    Resource,
};

struct Top {
    handle: ZwlrForeignToplevelHandleV1,
    name: String,
    activated: bool,
}

struct Ws {
    handle: ExtWorkspaceHandleV1,
    name: String,
    active: bool,
}

#[derive(Default)]
struct Mock {
    /// What exists (name, focused / name, active); handles are made when a client binds.
    windows: Vec<(String, bool)>,
    spaces: Vec<(String, bool)>,
    tops: Vec<Top>,
    workspaces: Vec<Ws>,
    manager: Option<ZwlrForeignToplevelManagerV1>,
    ws_manager: Option<ExtWorkspaceManagerV1>,
    log: Arc<Mutex<Vec<String>>>,
    pending_ws_commit: Option<String>,
}

struct Data;
impl ClientData for Data {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

fn state_bytes(activated: bool) -> Vec<u8> {
    if activated {
        2u32.to_ne_bytes().to_vec()
    } else {
        Vec::new()
    }
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for Mock {
    fn bind(
        mock: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let m = init.init(resource, ());
        for (name, activated) in mock.windows.clone() {
            let h = client
                .create_resource::<ZwlrForeignToplevelHandleV1, _, Mock>(
                    dh,
                    m.version(),
                    name.clone(),
                )
                .unwrap();
            m.toplevel(&h);
            h.title(format!("{name} title"));
            h.app_id(name.clone());
            h.state(state_bytes(activated));
            h.done();
            mock.tops.push(Top {
                handle: h,
                name,
                activated,
            });
        }
        mock.manager = Some(m);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Mock {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ZwlrForeignToplevelManagerV1,
        _: zwlr_foreign_toplevel_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, String> for Mock {
    fn request(
        mock: &mut Self,
        _: &Client,
        _: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        name: &String,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Request as R;
        match request {
            R::Activate { .. } => {
                mock.log.lock().unwrap().push(format!("activate {name}"));
                for t in &mut mock.tops {
                    t.activated = &t.name == name;
                    t.handle.state(state_bytes(t.activated));
                    t.handle.done();
                }
            }
            R::Close => {
                mock.log.lock().unwrap().push(format!("close {name}"));
                if let Some(i) = mock.tops.iter().position(|t| &t.name == name) {
                    mock.tops.remove(i).handle.closed();
                }
            }
            _ => {}
        }
    }
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for Mock {
    fn bind(
        mock: &mut Self,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let m = init.init(resource, ());
        let group = client
            .create_resource::<ExtWorkspaceGroupHandleV1, _, Mock>(dh, m.version(), ())
            .unwrap();
        m.workspace_group(&group);
        for (name, active) in mock.spaces.clone() {
            let h = client
                .create_resource::<ExtWorkspaceHandleV1, _, Mock>(dh, m.version(), name.clone())
                .unwrap();
            m.workspace(&h);
            h.name(name.clone());
            h.state(if active {
                ext_workspace_handle_v1::State::Active
            } else {
                ext_workspace_handle_v1::State::empty()
            });
            mock.workspaces.push(Ws {
                handle: h,
                name,
                active,
            });
        }
        m.done();
        mock.ws_manager = Some(m);
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for Mock {
    fn request(
        mock: &mut Self,
        _: &Client,
        m: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let ext_workspace_manager_v1::Request::Commit = request
            && let Some(name) = mock.pending_ws_commit.take()
        {
            mock.log.lock().unwrap().push(format!("workspace {name}"));
            for w in &mut mock.workspaces {
                w.active = w.name == name;
                w.handle.state(if w.active {
                    ext_workspace_handle_v1::State::Active
                } else {
                    ext_workspace_handle_v1::State::empty()
                });
            }
            m.done();
        }
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for Mock {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtWorkspaceGroupHandleV1,
        _: ext_workspace_group_handle_v1::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceHandleV1, String> for Mock {
    fn request(
        mock: &mut Self,
        _: &Client,
        _: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        name: &String,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let ext_workspace_handle_v1::Request::Activate = request {
            mock.pending_ws_commit = Some(name.clone());
        }
    }
}

impl GlobalDispatch<WlSeat, ()> for Mock {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<WlSeat>,
        _: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl Dispatch<WlSeat, ()> for Mock {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &WlSeat,
        _: wl_seat::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

enum Cmd {
    Open(&'static str),
}

/// Runs the mock compositor on `path` until the sender is dropped.
fn serve(
    path: std::path::PathBuf,
    log: Arc<Mutex<Vec<String>>>,
    rx: Receiver<Cmd>,
    ready: Sender<()>,
) {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let mut display: Display<Mock> = Display::new().unwrap();
    let dh = display.handle();
    dh.create_global::<Mock, ZwlrForeignToplevelManagerV1, _>(3, ());
    dh.create_global::<Mock, ExtWorkspaceManagerV1, _>(1, ());
    dh.create_global::<Mock, WlSeat, _>(1, ());
    let listener = ListeningSocket::bind_absolute(path).unwrap();
    let mut mock = Mock {
        log,
        windows: vec![("foot".into(), true)],
        spaces: vec![("1".into(), true), ("2".into(), false)],
        ..Default::default()
    };
    let _ = ready.send(());
    loop {
        loop {
            match rx.try_recv() {
                Ok(Cmd::Open(name)) => {
                    if let (Some(m), Some(client)) = (
                        mock.manager.clone(),
                        mock.manager.as_ref().and_then(|m| m.client()),
                    ) {
                        for t in &mut mock.tops {
                            t.activated = false;
                            t.handle.state(state_bytes(false));
                            t.handle.done();
                        }
                        let h = client
                            .create_resource::<ZwlrForeignToplevelHandleV1, _, Mock>(
                                &dh,
                                m.version(),
                                name.to_string(),
                            )
                            .unwrap();
                        m.toplevel(&h);
                        h.title(format!("{name} title"));
                        h.app_id(name.to_string());
                        h.state(state_bytes(true));
                        h.done();
                        mock.windows = vec![(name.into(), true)];
                        mock.tops.push(Top {
                            handle: h,
                            name: name.into(),
                            activated: true,
                        });
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(_) => return,
            }
        }
        while let Ok(Some(stream)) = listener.accept() {
            display
                .handle()
                .insert_client(stream, Arc::new(Data))
                .unwrap();
        }
        display.dispatch_clients(&mut mock).unwrap();
        display.flush_clients().unwrap();
        let backend_fd = display.backend().poll_fd().try_clone_to_owned().unwrap();
        let listener_fd = listener.as_fd().try_clone_to_owned().unwrap();
        let mut fds = [
            PollFd::new(&backend_fd, PollFlags::IN),
            PollFd::new(&listener_fd, PollFlags::IN),
        ];
        let _ = poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 30_000_000,
            }),
        );
    }
}

/// Collects the watcher's states on a thread (it sleeps in poll, which is also what serves the
/// control channel).
fn run_watcher(mut w: Watcher) -> Receiver<DesktopState> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(w.state().clone());
        while let Ok(Some(s)) = w.next_change() {
            if tx.send(s).is_err() {
                return;
            }
        }
    });
    rx
}

fn until(
    rx: &Receiver<DesktopState>,
    what: &str,
    f: impl Fn(&DesktopState) -> bool,
) -> DesktopState {
    let end = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = end.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(s) if f(&s) => return s,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for: {what}"),
        }
    }
}

#[test]
fn watcher_follows_a_mock_compositor_and_controls_it() {
    let dir = std::env::temp_dir().join(format!("sliver-wlr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("wl-mock");
    let log = Arc::new(Mutex::new(Vec::new()));
    let (cmd_tx, cmd_rx) = channel();
    let (ready_tx, ready_rx) = channel();
    let server = {
        let (sock, log) = (sock.clone(), log.clone());
        std::thread::spawn(move || serve(sock, log, cmd_rx, ready_tx))
    };
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // SAFETY: this is the only test in the file and nothing else reads the environment meanwhile.
    unsafe {
        std::env::set_var("WAYLAND_DISPLAY", &sock);
    }

    let watcher = Watcher::connect().expect("connect to the mock");
    let initial = watcher.state().clone();
    assert_eq!(initial.backend, Backend::Wlroots);
    assert_eq!(
        initial.window.as_ref().map(|w| w.class.as_str()),
        Some("foot")
    );
    assert_eq!(
        initial
            .workspaces
            .iter()
            .map(|w| w.name.as_str())
            .collect::<Vec<_>>(),
        ["1", "2"]
    );
    assert_eq!(initial.workspace.as_ref().unwrap().name, "1");
    let rx = run_watcher(watcher);
    let _ = until(&rx, "initial", |s| s.window.is_some());

    // a new window appears and takes focus
    cmd_tx.send(Cmd::Open("firefox")).unwrap();
    let s = until(&rx, "firefox focused", |s| {
        s.window.as_ref().is_some_and(|w| w.class == "firefox")
    });
    assert_eq!(s.windows.len(), 2);
    assert_eq!(s.windows[1].title, "firefox title");

    // control: focus the first window again
    let foot = s
        .windows
        .iter()
        .find(|w| w.class == "foot")
        .unwrap()
        .address
        .clone();
    send(&Action::FocusWindow { address: foot }).unwrap();
    until(&rx, "foot focused", |s| {
        s.window.as_ref().is_some_and(|w| w.class == "foot")
    });
    assert!(log.lock().unwrap().contains(&"activate foot".to_string()));

    // control: switch workspace
    let two = s.workspaces.iter().find(|w| w.name == "2").unwrap().clone();
    send(&Action::FocusWorkspace(two)).unwrap();
    let s = until(&rx, "workspace 2", |s| {
        s.workspace.as_ref().is_some_and(|w| w.name == "2")
    });
    assert!(s.workspaces.iter().any(|w| w.name == "2" && w.active));
    assert!(log.lock().unwrap().contains(&"workspace 2".to_string()));

    // control: close the focused window
    let foot = s
        .windows
        .iter()
        .find(|w| w.class == "foot")
        .unwrap()
        .address
        .clone();
    send(&Action::CloseWindow { address: foot }).unwrap();
    let s = until(&rx, "foot gone", |s| {
        s.windows.iter().all(|w| w.class != "foot")
    });
    assert_eq!(s.windows.len(), 1);
    assert!(log.lock().unwrap().contains(&"close foot".to_string()));

    drop(cmd_tx);
    let _ = server.join();
    let _ = std::fs::remove_dir_all(&dir);
}
