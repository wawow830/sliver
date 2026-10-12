//! `SOCK_SEQPACKET` transport: one message per packet, fds via `SCM_RIGHTS` (`docs/api.md` §6).
//!
//! Blocking by default. Use [`Conn::set_nonblocking`] and the [`AsFd`] impl to drive it from an
//! event loop (epoll/calloop). Then `recv` returns `ErrorKind::WouldBlock` when nothing is queued.

use crate::proto::{CodecError, MAX_PACKET, Message, decode, encode};
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketAddrUnix, SocketFlags, SocketType,
};
use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;

/// Most fds accepted with one packet. Extra fds are closed and the packet is reported malformed.
const MAX_FDS: usize = 4;

/// One received packet. `msg` may be an error (unknown type, malformed) while the connection
/// stays usable. `fds` are owned and closed on drop.
#[derive(Debug)]
pub struct Packet<M> {
    pub msg: Result<M, CodecError>,
    pub fds: Vec<OwnedFd>,
}

/// Credentials of the connected peer (`SO_PEERCRED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// One end of an IPC connection.
#[derive(Debug)]
pub struct Conn {
    fd: OwnedFd,
}

impl Conn {
    fn socket() -> io::Result<OwnedFd> {
        Ok(rustix::net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?)
    }

    /// Connect to sliverd (normally [`crate::proto::SOCKET_PATH`]).
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Conn> {
        let fd = Self::socket()?;
        rustix::net::connect(&fd, &SocketAddrUnix::new(path.as_ref())?)?;
        Ok(Conn { fd })
    }

    /// A connected pair, for tests and the in-process fake daemon.
    pub fn pair() -> io::Result<(Conn, Conn)> {
        let (a, b) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        Ok((Conn { fd: a }, Conn { fd: b }))
    }

    pub fn from_fd(fd: OwnedFd) -> Conn {
        Conn { fd }
    }

    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        Ok(rustix::io::ioctl_fionbio(&self.fd, on)?)
    }

    pub fn peer_cred(&self) -> io::Result<PeerCred> {
        let c = rustix::net::sockopt::socket_peercred(&self.fd)?;
        Ok(PeerCred {
            pid: c.pid.as_raw_nonzero().get(),
            uid: c.uid.as_raw(),
            gid: c.gid.as_raw(),
        })
    }

    /// Send one message, optionally with one fd.
    pub fn send<M: Message>(&self, msg: &M) -> io::Result<()> {
        self.send_with_fd(msg, None)
    }

    pub fn send_with_fd<M: Message>(&self, msg: &M, fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
        let buf = encode(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let fds: Vec<BorrowedFd<'_>> = fd.into_iter().collect();
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(&fds)) {
            return Err(io::Error::other("ancillary buffer too small"));
        }
        let sent = rustix::net::sendmsg(
            &self.fd,
            &[IoSlice::new(&buf)],
            &mut control,
            SendFlags::NOSIGNAL,
        )?;
        if sent != buf.len() {
            return Err(io::Error::other("short packet send"));
        }
        Ok(())
    }

    /// Receive one packet. `Ok(None)` = the peer closed (or reset) the connection.
    pub fn recv<M: Message>(&self) -> io::Result<Option<Packet<M>>> {
        let mut buf = vec![0u8; MAX_PACKET];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let r = match rustix::net::recvmsg(
            &self.fd,
            &mut [IoSliceMut::new(&mut buf)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Ok(r) => r,
            // A peer that closes with unread data in its queue resets the connection. That's
            // a hangup like any other.
            Err(rustix::io::Errno::CONNRESET) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut fds = Vec::new();
        for m in control.drain() {
            if let RecvAncillaryMessage::ScmRights(it) = m {
                fds.extend(it);
            }
        }
        if r.bytes == 0 && fds.is_empty() && !r.flags.contains(ReturnFlags::TRUNC) {
            return Ok(None);
        }
        let msg = if r.flags.contains(ReturnFlags::TRUNC) {
            Err(CodecError::TooLarge(r.bytes.max(MAX_PACKET + 1)))
        } else if r.flags.contains(ReturnFlags::CTRUNC) {
            Err(CodecError::Malformed(format!("more than {MAX_FDS} fds")))
        } else {
            decode(&buf[..r.bytes])
        };
        Ok(Some(Packet { msg, fds }))
    }
}

impl AsFd for Conn {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// sliverd's listening socket.
#[derive(Debug)]
pub struct Listener {
    fd: OwnedFd,
}

impl Listener {
    /// Bind at `path` (replacing a stale socket, never another kind of file) with mode 0666.
    /// Authorisation happens per connection via [`Conn::peer_cred`] + logind.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Listener> {
        let path = path.as_ref();
        match std::fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let fd = Conn::socket()?;
        rustix::net::bind(&fd, &SocketAddrUnix::new(path)?)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        rustix::net::listen(&fd, 8)?;
        Ok(Listener { fd })
    }

    /// Adopt a socket passed by systemd socket activation.
    pub fn from_fd(fd: OwnedFd) -> Listener {
        Listener { fd }
    }

    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        Ok(rustix::io::ioctl_fionbio(&self.fd, on)?)
    }

    pub fn accept(&self) -> io::Result<Conn> {
        let fd = rustix::net::accept_with(&self.fd, SocketFlags::CLOEXEC)?;
        Ok(Conn { fd })
    }
}

impl AsFd for Listener {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{AgentMsg, DaemonMsg, KeyMode, PROTO};
    use std::io::{Read, Seek, Write};

    #[test]
    fn message_boundaries_and_fds() {
        let (a, b) = Conn::pair().unwrap();
        a.send(&AgentMsg::Hello {
            proto: PROTO,
            agent: "t".into(),
        })
        .unwrap();
        a.send(&AgentMsg::Wake).unwrap();

        let mut f = std::fs::File::from(
            rustix::fs::memfd_create("t", rustix::fs::MemfdFlags::CLOEXEC).unwrap(),
        );
        f.write_all(b"hi").unwrap();
        a.send_with_fd(&AgentMsg::DetachBuffer { id: 1 }, Some(f.as_fd()))
            .unwrap();

        let p = b.recv::<AgentMsg>().unwrap().unwrap();
        assert_eq!(
            p.msg.unwrap(),
            AgentMsg::Hello {
                proto: PROTO,
                agent: "t".into()
            }
        );
        assert!(p.fds.is_empty());
        assert_eq!(
            b.recv::<AgentMsg>().unwrap().unwrap().msg.unwrap(),
            AgentMsg::Wake
        );
        let p = b.recv::<AgentMsg>().unwrap().unwrap();
        assert_eq!(p.fds.len(), 1);
        let mut g = std::fs::File::from(p.fds.into_iter().next().unwrap());
        g.rewind().unwrap();
        let mut s = String::new();
        g.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hi");
    }

    #[test]
    fn bad_packets_keep_the_connection_usable() {
        let (a, b) = Conn::pair().unwrap();
        rustix::net::send(&a, b"{\"t\":\"nope\"}", SendFlags::empty()).unwrap();
        rustix::net::send(&a, &vec![b' '; MAX_PACKET + 10], SendFlags::empty()).unwrap();
        a.send(&DaemonMsg::Ping { nonce: 1 }).unwrap();
        let e = b.recv::<DaemonMsg>().unwrap().unwrap().msg.unwrap_err();
        assert_eq!(e, CodecError::UnknownMessage("nope".into()));
        let e = b.recv::<DaemonMsg>().unwrap().unwrap().msg.unwrap_err();
        assert!(matches!(e, CodecError::TooLarge(_)));
        assert_eq!(
            b.recv::<DaemonMsg>().unwrap().unwrap().msg.unwrap(),
            DaemonMsg::Ping { nonce: 1 }
        );
    }

    #[test]
    fn eof_and_wouldblock() {
        let (a, b) = Conn::pair().unwrap();
        b.set_nonblocking(true).unwrap();
        let e = b.recv::<AgentMsg>().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        drop(a);
        assert!(b.recv::<AgentMsg>().unwrap().is_none());
        // Closing with unread data resets the peer. That's also a hangup.
        let (a, b) = Conn::pair().unwrap();
        b.send(&DaemonMsg::Ping { nonce: 1 }).unwrap();
        drop(a);
        assert!(b.recv::<AgentMsg>().unwrap().is_none());
    }

    #[test]
    fn oversized_send_is_refused_locally() {
        let (a, _b) = Conn::pair().unwrap();
        let m = AgentMsg::EmitKeys {
            keys: vec![30; MAX_PACKET],
            mode: KeyMode::Tap,
        };
        assert_eq!(a.send(&m).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn listener_roundtrip_and_peercred() {
        let dir = std::env::temp_dir().join(format!("sliver-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");
        let l = Listener::bind(&path).unwrap();
        let _again = Listener::bind(&path).unwrap(); // stale socket replaced
        drop(_again);
        let l2 = Listener::bind(&path).unwrap();
        drop(l);
        let c = Conn::connect(&path).unwrap();
        let s = l2.accept().unwrap();
        assert_eq!(
            s.peer_cred().unwrap().uid,
            rustix::process::getuid().as_raw()
        );
        c.send(&AgentMsg::Bye).unwrap();
        assert_eq!(
            s.recv::<AgentMsg>().unwrap().unwrap().msg.unwrap(),
            AgentMsg::Bye
        );
        // Never clobbers a regular file.
        let file = dir.join("not-a-socket");
        std::fs::write(&file, "x").unwrap();
        assert!(Listener::bind(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
