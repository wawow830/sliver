use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, ensure, Context, Result};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct PreparedPathState {
    state_file: PathBuf,
    temp_file: Option<PathBuf>,
}

impl PreparedPathState {
    pub(crate) fn prepare(state_file: &Path, selected_path: &Path) -> Result<Self> {
        ensure!(
            state_file.file_name().is_some(),
            "selected-path state file must name a file: {}",
            state_file.display()
        );
        ensure!(
            selected_path.is_absolute(),
            "selected path must be absolute: {}",
            selected_path.display()
        );

        let directory = state_directory(state_file);
        fs::create_dir_all(directory).with_context(|| {
            format!(
                "creating selected-path state directory {}",
                directory.display()
            )
        })?;

        let contents = selected_path.as_os_str().as_bytes();
        let (mut file, temp_file) = create_temp_file(state_file, directory)?;
        let result = (|| -> Result<()> {
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .with_context(|| format!("setting mode 0600 on {}", temp_file.display()))?;
            file.write_all(contents)
                .with_context(|| format!("writing selected path to {}", temp_file.display()))?;
            file.sync_all()
                .with_context(|| format!("syncing selected-path state {}", temp_file.display()))?;
            Ok(())
        })();

        if let Err(error) = result {
            let _ = fs::remove_file(&temp_file);
            return Err(error);
        }

        Ok(Self {
            state_file: state_file.to_path_buf(),
            temp_file: Some(temp_file),
        })
    }

    pub(crate) fn commit(mut self) -> Result<()> {
        let temp_file = self
            .temp_file
            .as_ref()
            .context("selected-path state was already committed")?
            .clone();
        fs::rename(&temp_file, &self.state_file).with_context(|| {
            format!(
                "committing selected-path state {} as {}",
                temp_file.display(),
                self.state_file.display()
            )
        })?;
        self.temp_file.take();

        if let Err(error) = sync_directory(state_directory(&self.state_file)) {
            eprintln!("selected-path directory sync failed after commit: {error:#}");
        }
        Ok(())
    }
}

impl Drop for PreparedPathState {
    fn drop(&mut self) {
        if let Some(temp_file) = self.temp_file.take() {
            let _ = fs::remove_file(temp_file);
        }
    }
}

fn state_directory(state_file: &Path) -> &Path {
    state_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn create_temp_file(state_file: &Path, directory: &Path) -> Result<(File, PathBuf)> {
    let basename = state_file
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "state".into());
    let process_id = std::process::id();

    for _ in 0..u64::MAX {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_file = directory.join(format!(".{basename}.tmp.{process_id}.{counter}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_file)
        {
            Ok(file) => return Ok((file, temp_file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                bail!(
                    "creating unique selected-path temp file {}: {}",
                    temp_file.display(),
                    error
                );
            }
        }
    }

    bail!(
        "exhausted selected-path temp file names in {}",
        directory.display()
    )
}

fn sync_directory(directory: &Path) -> Result<()> {
    let directory_file = match File::open(directory) {
        Ok(file) => file,
        Err(error) if directory_sync_unsupported(&error) => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "opening selected-path state directory {}",
                    directory.display()
                )
            });
        }
    };

    match directory_file.sync_all() {
        Ok(()) => Ok(()),
        Err(error) if directory_sync_unsupported(&error) => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "syncing selected-path state directory {}",
                directory.display()
            )
        }),
    }
}

fn directory_sync_unsupported(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
    ) {
        return true;
    }

    match error.raw_os_error() {
        Some(code) => code == libc::EINVAL || code == libc::ENOTSUP || code == libc::EOPNOTSUPP,
        None => false,
    }
}
