use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

use crate::apply_ipc::absolute_lexical;
use crate::authorization::{AuthorizationGrant, SessionAuthorizer};
use crate::hardware::TouchBarHardware;
use crate::logind::{Logind, RealLogind};
use crate::lua_worker::{LuaWorker, StagedLuaWorker, StopReason};
use crate::path_state::{PathStateSnapshot, PreparedPathState};
use crate::peer_credentials::PeerCredentials;

struct ActiveConfig {
    worker: LuaWorker,
    _selected_path: PathBuf,
}

pub(crate) struct Supervisor<H: TouchBarHardware, L: Logind = RealLogind> {
    hardware: H,
    state_file: PathBuf,
    active: Option<ActiveConfig>,
    claimed: bool,
    authorizer: SessionAuthorizer<L>,
}

impl<H: TouchBarHardware> Supervisor<H, RealLogind> {
    pub(crate) fn new(hardware: H, state_file: PathBuf) -> Result<Self> {
        Self::new_with_logind(hardware, state_file, RealLogind)
    }
}

impl<H: TouchBarHardware, L: Logind> Supervisor<H, L> {
    pub(crate) fn new_with_logind(mut hardware: H, state_file: PathBuf, logind: L) -> Result<Self> {
        hardware.claim()?;
        Ok(Self {
            hardware,
            state_file,
            active: None,
            claimed: true,
            authorizer: SessionAuthorizer::new(logind),
        })
    }

    #[cfg(test)]
    pub(crate) fn apply(&mut self, requested_path: &Path) -> Result<()> {
        self.apply_candidate(requested_path, None)
    }

    fn apply_authorized(&mut self, requested_path: &Path, peer: PeerCredentials) -> Result<()> {
        let grant = self.authorizer.authorize(peer)?;
        self.apply_candidate(requested_path, Some((peer, grant)))
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

        let StagedLuaWorker { worker, frame } = LuaWorker::stage(&selected_path)?;
        let previous_path_state = PathStateSnapshot::capture(&self.state_file)?;
        let path_state = PreparedPathState::prepare(&self.state_file, &selected_path)?;
        if let Some((peer, grant)) = authorization {
            self.authorizer.recheck(peer, &grant)?;
        }
        path_state.commit()?;
        if let Err(presentation_error) = self.hardware.present(&frame) {
            if let Err(rollback_error) = previous_path_state.restore(&self.state_file) {
                return Err(presentation_error).context(format!(
                    "restoring selected path after presentation failed also failed: {rollback_error:#}"
                ));
            }
            return Err(presentation_error);
        }

        let replaced = self.active.replace(ActiveConfig {
            worker,
            _selected_path: selected_path,
        });
        if let Some(replaced) = replaced {
            if let Err(error) = replaced.worker.shutdown(StopReason::Replaced) {
                eprintln!("replaced Lua worker did not stop cleanly: {error:#}");
            }
        }
        Ok(())
    }

    pub(crate) fn shutdown(mut self) -> Result<()> {
        let stop_result = match self.active.take() {
            Some(active) => active.worker.shutdown(StopReason::Shutdown),
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

pub(crate) fn serve<H: TouchBarHardware, L: Logind>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H, L>,
) -> Result<()> {
    for connection in listener.incoming() {
        let mut stream = connection.context("accepting apply request")?;
        serve_connection(&mut stream, supervisor)?;
    }
    Ok(())
}

fn serve_connection<H: TouchBarHardware, L: Logind>(
    stream: &mut UnixStream,
    supervisor: &mut Supervisor<H, L>,
) -> Result<()> {
    let result = crate::peer_credentials::read(stream)
        .and_then(|peer| crate::apply_ipc::read_request(stream).map(|path| (path, peer)))
        .and_then(|(path, peer)| supervisor.apply_authorized(&path, peer));
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
        FakeAction, FakeTouchBar, HardwareEvent, LogicalFrame, ModifierState, TouchBarHardware,
    };
    use crate::logind::{ActiveSession, FakeLogind, Session};

    use super::{serve_connection, Supervisor};

    struct FailingPresentHardware {
        inner: FakeTouchBar,
        state_file: std::path::PathBuf,
        fail_next_present: bool,
        state_seen_at_failure: Vec<u8>,
    }

    impl FailingPresentHardware {
        fn new(state_file: std::path::PathBuf) -> Self {
            Self {
                inner: FakeTouchBar::new(),
                state_file,
                fail_next_present: false,
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

        fn set_backlight(&mut self, level: f64) -> Result<()> {
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
        assert_eq!(supervisor.hardware().inner.presented_frames().len(), 1);
        assert_eq!(
            supervisor.hardware().inner.presented_frames()[0].rgba_at(10, 10),
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
        let logind = FakeLogind::new();
        let uid = unsafe { libc::getuid() };
        let pid = std::process::id() as libc::pid_t;
        logind.set_session(
            pid,
            Some(Session {
                id: "seat-session".into(),
                uid,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "seat-session".into(),
                uid,
            }),
        );
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
        let pid = std::process::id() as libc::pid_t;
        let uid = unsafe { libc::getuid() };
        let logind = FakeLogind::new();
        logind.set_session(
            pid,
            Some(Session {
                id: "seat-session".into(),
                uid,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "seat-session".into(),
                uid,
            }),
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
        client.join().expect("apply client panicked")?;

        assert_eq!(
            std::fs::read(&state_file)?,
            source.as_os_str().as_encoded_bytes()
        );
        assert_eq!(supervisor.hardware().presented_frames().len(), 1);
        assert_eq!(
            supervisor.hardware().presented_frames()[0].rgba_at(10, 10),
            [255, 0, 0, 255]
        );
        supervisor.shutdown()?;
        Ok(())
    }

    #[test]
    fn session_change_during_staging_cancels_the_candidate_before_commit() -> Result<()> {
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
        let uid = unsafe { libc::getuid() };
        let pid = std::process::id() as libc::pid_t;
        let logind = FakeLogind::new();
        logind.set_session(
            pid,
            Some(Session {
                id: "old-session".into(),
                uid,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "old-session".into(),
                uid,
            }),
        );
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
    fn queued_apply_is_cancelled_after_the_active_session_changes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let socket = directory.path().join("supervisor.sock");
        let listener = UnixListener::bind(&socket)?;
        let marker = directory.path().join("staging");
        let gate = directory.path().join("release");
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
        let second = directory.path().join("second.lua");
        std::fs::write(
            &second,
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
        let state_file = directory.path().join("state/sliver/config-path");
        let uid = unsafe { libc::getuid() };
        let pid = std::process::id() as libc::pid_t;
        let logind = FakeLogind::new();
        logind.set_session(
            pid,
            Some(Session {
                id: "old-session".into(),
                uid,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "old-session".into(),
                uid,
            }),
        );
        let server_logind = logind.clone();
        let server_state = state_file.clone();
        let server = thread::spawn(move || -> Result<Supervisor<FakeTouchBar, FakeLogind>> {
            let mut supervisor =
                Supervisor::new_with_logind(FakeTouchBar::new(), server_state, server_logind)?;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept()?;
                serve_connection(&mut stream, &mut supervisor)?;
            }
            Ok(supervisor)
        });
        let first_socket = socket.clone();
        let first_client_source = first.clone();
        let first_client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&first_socket, &first_client_source)
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        anyhow::ensure!(marker.exists(), "first candidate never entered staging");

        let second_socket = socket.clone();
        let second_client_source = second.clone();
        let second_client = thread::spawn(move || {
            crate::apply_ipc::request_apply_at(&second_socket, &second_client_source)
        });
        thread::sleep(Duration::from_millis(50));
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "new-session".into(),
                uid,
            }),
        );
        std::fs::write(&gate, "continue")?;

        let first_error = first_client
            .join()
            .expect("first apply client panicked")
            .expect_err("staged apply committed after session change");
        let second_error = second_client
            .join()
            .expect("second apply client panicked")
            .expect_err("queued apply committed after session change");
        assert!(format!("{first_error:#}").contains("not the active session"));
        assert!(format!("{second_error:#}").contains("not the active session"));
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
        let uid = unsafe { libc::getuid() };
        let pid = std::process::id() as libc::pid_t;
        let logind = FakeLogind::new();
        logind.set_session(
            pid,
            Some(Session {
                id: "old-session".into(),
                uid,
                seat: Some("seat0".into()),
                remote: false,
                active: true,
            }),
        );
        logind.set_active(
            "seat0",
            Some(ActiveSession {
                id: "old-session".into(),
                uid,
            }),
        );
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
        assert!(format!("{rejected:#}").contains("not the active session"));

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
