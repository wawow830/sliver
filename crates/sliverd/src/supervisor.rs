use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

use crate::apply_ipc::absolute_lexical;
use crate::hardware::{LogicalFrame, TouchBarHardware};
use crate::lua_worker::{LuaWorker, StagedLuaWorker, StopReason};
use crate::path_state::PreparedPathState;

struct ActiveConfig {
    worker: LuaWorker,
    frame: LogicalFrame,
    _selected_path: PathBuf,
}

pub(crate) struct Supervisor<H: TouchBarHardware> {
    hardware: H,
    state_file: PathBuf,
    active: Option<ActiveConfig>,
    claimed: bool,
}

impl<H: TouchBarHardware> Supervisor<H> {
    pub(crate) fn new(mut hardware: H, state_file: PathBuf) -> Result<Self> {
        hardware.claim()?;
        Ok(Self {
            hardware,
            state_file,
            active: None,
            claimed: true,
        })
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

        let StagedLuaWorker { worker, frame } = LuaWorker::stage(&selected_path)?;
        let path_state = PreparedPathState::prepare(&self.state_file, &selected_path)?;
        self.hardware.present(&frame)?;
        if let Err(error) = path_state.commit() {
            if let Some(active) = &self.active {
                self.hardware.present(&active.frame).with_context(|| {
                    format!("restoring active frame after state commit failed: {error:#}")
                })?;
            }
            return Err(error);
        }

        let replaced = self.active.replace(ActiveConfig {
            worker,
            frame,
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
}

pub(crate) fn serve<H: TouchBarHardware>(
    listener: UnixListener,
    supervisor: &mut Supervisor<H>,
) -> Result<()> {
    for connection in listener.incoming() {
        let mut stream = connection.context("accepting apply request")?;
        serve_connection(&mut stream, supervisor)?;
    }
    Ok(())
}

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

    use anyhow::{Context, Result};

    use crate::hardware::FakeTouchBar;

    use super::Supervisor;

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
