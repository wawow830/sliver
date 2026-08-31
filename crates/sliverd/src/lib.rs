mod apply_ipc;
mod authorization;
mod broker_ipc;
mod config_selection;
mod default_source;
mod frame_canvas;
mod frame_slots;
mod hardware;
mod logind;
mod lua_canvas;
mod lua_image;
#[cfg(test)]
mod lua_integration_tests;
mod lua_worker;
mod m2_hardware;
mod path_state;
mod peer_credentials;
mod recovery;
mod supervisor;
mod system_log;

use anyhow::{bail, Context, Result};

pub(crate) const DISPLAY_WIDTH: usize = 2008;
pub(crate) const DISPLAY_HEIGHT: usize = 60;
pub(crate) const DISPLAY_WIDTH_F64: f64 = DISPLAY_WIDTH as f64;
pub(crate) const DISPLAY_HEIGHT_F64: f64 = DISPLAY_HEIGHT as f64;

/// Send one explicit Lua config to the running per-user supervisor.
#[doc(hidden)]
pub fn apply_config(path: &std::path::Path) -> Result<()> {
    apply_ipc::request_apply(path)
}

/// Ask the running per-user supervisor to select the embedded Lua default.
#[doc(hidden)]
pub fn apply_default() -> Result<()> {
    apply_ipc::request_default()
}

/// Run one disposable Lua worker process.
#[doc(hidden)]
pub fn lua_worker_main() -> Result<()> {
    lua_worker::worker_process_main()
}

/// Run the system hardware broker process.
#[doc(hidden)]
pub fn broker_main() -> Result<()> {
    let result = broker_ipc::broker_main();
    if let Err(error) = &result {
        system_log::broker_error(format!("broker service failed: {error:#}"));
    }
    result
}

/// Run the per-user supervisor process.
pub fn supervisor_main() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let running = Arc::new(AtomicBool::new(true));
    let signal_running = running.clone();
    ctrlc::set_handler(move || signal_running.store(false, Ordering::Release))?;
    run_supervisor_loop(&running, std::time::Duration::from_secs(1), || {
        supervisor_main_inner(running.clone())
    })
}

fn run_supervisor_loop<F>(
    running: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    retry_delay: std::time::Duration,
    mut start: F,
) -> Result<()>
where
    F: FnMut() -> Result<()>,
{
    loop {
        if !running.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let result = start();
        if is_transient_supervisor_error(&result) {
            std::thread::sleep(retry_delay);
            continue;
        }
        if let Err(error) = &result {
            system_log::supervisor_error(format!("supervisor service failed: {error:#}"));
        }
        return result;
    }
}

#[derive(Debug)]
struct WaitForActiveSession;

#[derive(Debug)]
struct WaitForHardware;

impl std::fmt::Display for WaitForHardware {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("waiting for Touch Bar hardware")
    }
}

impl std::error::Error for WaitForHardware {}

impl std::fmt::Display for WaitForActiveSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("waiting for an active local user session")
    }
}

impl std::error::Error for WaitForActiveSession {}

fn is_transient_supervisor_error(result: &Result<()>) -> bool {
    result.as_ref().err().is_some_and(|error| {
        error.downcast_ref::<WaitForActiveSession>().is_some()
            || error.downcast_ref::<WaitForHardware>().is_some()
            || error.chain().any(|cause| {
                cause.downcast_ref::<WaitForActiveSession>().is_some()
                    || cause.downcast_ref::<WaitForHardware>().is_some()
            })
    })
}

fn supervisor_main_inner(running: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    let socket = apply_ipc::supervisor_socket_path()?;
    let socket_directory = socket
        .parent()
        .context("supervisor socket path has no parent directory")?;
    std::fs::create_dir_all(socket_directory).with_context(|| {
        format!(
            "creating supervisor socket directory {}",
            socket_directory.display()
        )
    })?;
    std::fs::set_permissions(socket_directory, std::fs::Permissions::from_mode(0o700))?;
    if socket.exists() {
        match UnixStream::connect(&socket) {
            Ok(_) => bail!(
                "a Sliver supervisor is already listening at {}",
                socket.display()
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(&socket)?;
            }
            Err(error) => return Err(error).context("checking existing supervisor socket"),
        }
    }
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("binding supervisor socket {}", socket.display()))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

    let state_file = selected_path_state_file()?;
    #[cfg(test)]
    let mut supervisor = supervisor::Supervisor::new_with_startup_candidate(
        broker_ipc::BrokerHardware::new(),
        state_file.clone(),
        crate::logind::RealLogind::default(),
        Some(default_source::source()),
    )?;
    #[cfg(not(test))]
    let mut supervisor = supervisor::Supervisor::new_with_startup_candidate(
        broker_ipc::BrokerHardware::new(),
        state_file,
        crate::logind::RealLogind::default(),
    )?;
    let serve_result = supervisor::serve_until(listener, &mut supervisor, running);
    let session_revoked = supervisor.hardware().session_revoked();
    let handoff_result = if session_revoked {
        let reason = supervisor.hardware().revoked_stop_reason();
        let handoff = supervisor.handoff_owner_with_reason(reason);
        let acknowledgement = supervisor.hardware_mut().logout_complete();
        match (handoff, acknowledgement) {
            (Err(error), Err(acknowledgement_error)) => Err(error).context(format!(
                "logout acknowledgement also failed: {acknowledgement_error:#}"
            )),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    } else {
        Ok(())
    };
    let graceful_service_stop = serve_result.is_ok();
    let service_result = if session_revoked {
        Err(anyhow::anyhow!("active user session ended")).context(WaitForActiveSession)
    } else {
        serve_result
    };
    let shutdown_result = if session_revoked || graceful_service_stop {
        supervisor.shutdown_for_logout()
    } else {
        supervisor.shutdown()
    };
    let _ = std::fs::remove_file(&socket);
    let service_result = match (service_result, handoff_result) {
        (Err(error), Err(handoff_error)) => {
            Err(error).context(format!("owner handoff also failed: {handoff_error:#}"))
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    };
    match (service_result, shutdown_result) {
        (Err(error), Err(shutdown_error)) => Err(error).context(format!(
            "supervisor shutdown also failed: {shutdown_error:#}"
        )),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn selected_path_state_file() -> Result<std::path::PathBuf> {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(std::path::PathBuf::from(state_home).join("sliver/config-path"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(std::path::PathBuf::from(home).join(".local/state/sliver/config-path"))
}

/// Run the optional raw-buffer calibration tool.
#[cfg(feature = "calibration")]
pub fn calibration_main() -> Result<()> {
    m2_hardware::calibration()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_for_session_error_restarts_supervisor_startup() {
        let result: Result<()> =
            Err(anyhow::anyhow!("claim was deferred").context(WaitForActiveSession));

        assert!(is_transient_supervisor_error(&result));
    }

    #[test]
    fn supervisor_retries_a_transient_startup_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let attempts = Arc::new(AtomicUsize::new(0));
        let result = run_supervisor_loop(&running, std::time::Duration::ZERO, {
            let attempts = attempts.clone();
            let running = running.clone();
            move || {
                let attempt = attempts.fetch_add(1, Ordering::Relaxed);
                if attempt == 0 {
                    Err(anyhow::anyhow!("claim was deferred").context(WaitForActiveSession))
                } else {
                    running.store(false, std::sync::atomic::Ordering::Release);
                    Ok(())
                }
            }
        });

        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
    }
}
