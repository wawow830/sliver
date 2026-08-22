use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

pub(crate) fn request_apply(path: &Path) -> Result<()> {
    let socket = supervisor_socket_path()?;
    request_apply_at(&socket, path)
}

pub(crate) fn request_apply_at(socket: &Path, path: &Path) -> Result<()> {
    let path = absolute_lexical(path)?;
    let bytes = path.as_os_str().as_bytes();
    ensure!(
        bytes.len() <= MAX_MESSAGE_BYTES,
        "config path is too long to send to the supervisor"
    );

    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to Sliver supervisor at {}", socket.display()))?;
    write_bytes(&mut stream, bytes)?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut status = [0u8; 1];
    stream
        .read_exact(&mut status)
        .context("reading supervisor reply status")?;
    let message = read_bytes(&mut stream).context("reading supervisor reply")?;
    match status[0] {
        0 if message.is_empty() => Ok(()),
        0 => bail!("supervisor returned an invalid success reply"),
        1 => bail!("{}", String::from_utf8_lossy(&message)),
        other => bail!("supervisor returned unknown status {other}"),
    }
}

pub(crate) fn read_request(stream: &mut UnixStream) -> Result<PathBuf> {
    let bytes = read_bytes(stream).context("reading apply request")?;
    ensure!(!bytes.is_empty(), "config path is empty");
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

pub(crate) fn write_reply(stream: &mut UnixStream, result: &Result<()>) -> Result<()> {
    match result {
        Ok(()) => {
            stream.write_all(&[0])?;
            write_bytes(stream, &[])
        }
        Err(error) => {
            stream.write_all(&[1])?;
            write_bytes(stream, format!("{error:#}").as_bytes())
        }
    }
}

pub(crate) fn supervisor_socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .context("XDG_RUNTIME_DIR is not set; no per-user supervisor is available")?;
    Ok(PathBuf::from(runtime)
        .join("sliver")
        .join("supervisor.sock"))
}

pub(crate) fn absolute_lexical(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("reading current directory")?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    ensure!(normalized.is_absolute(), "config path is not absolute");
    Ok(normalized)
}

fn write_bytes(stream: &mut UnixStream, bytes: &[u8]) -> Result<()> {
    let length = u32::try_from(bytes.len()).context("IPC message is too large")?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}

fn read_bytes(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(length <= MAX_MESSAGE_BYTES, "IPC message is too large");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
