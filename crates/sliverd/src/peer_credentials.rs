use std::mem;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;

use anyhow::{ensure, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PeerCredentials {
    pub(crate) pid: libc::pid_t,
    pub(crate) uid: libc::uid_t,
    pub(crate) gid: libc::gid_t,
}

pub(crate) fn read(stream: &UnixStream) -> Result<PeerCredentials> {
    let mut credentials = mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    ensure!(
        result == 0,
        "reading Unix peer credentials: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        length as usize >= mem::size_of::<libc::ucred>(),
        "Unix peer credentials were truncated"
    );

    let credentials = unsafe { credentials.assume_init() };
    Ok(PeerCredentials {
        pid: credentials.pid,
        uid: credentials.uid,
        gid: credentials.gid,
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::thread;

    use super::*;

    #[test]
    fn reads_kernel_credentials_from_a_connected_unix_stream() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let listener = UnixListener::bind(directory.path().join("peer.sock"))?;
        let client = thread::spawn({
            let path = directory.path().join("peer.sock");
            move || UnixStream::connect(path)
        });
        let (stream, _) = listener.accept()?;
        let peer = read(&stream)?;
        let _client = client.join().expect("client thread panicked")?;

        assert_eq!(peer.pid, std::process::id() as libc::pid_t);
        assert_eq!(peer.uid, unsafe { libc::getuid() });
        assert_eq!(peer.gid, unsafe { libc::getgid() });
        Ok(())
    }
}
