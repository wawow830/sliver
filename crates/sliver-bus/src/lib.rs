//! `sliver-bus`: D-Bus connections that leave an idle process asleep (invariant 3, T103).
//!
//! zbus 5 (async-io) connects Unix sockets and closes them through `blocking::unblock`. The
//! `blocking` crate then keeps an idle pool thread in a 500 ms `wait_timeout` loop that it only
//! leaves when the pool is over its limit (500), i.e. never: two wakeups a second, forever, in
//! every process that ever connected or dropped a connection. That was the agent's whole idle
//! floor (docs/hardware.md, "Idle wakeups").
//!
//! So this crate connects the socket itself (a plain `connect(2)` on a Unix socket doesn't block
//! for long) and hands zbus a socket whose write half shuts down directly on close (`shutdown(2)`
//! on a Unix socket never blocks). zbus never touches the pool. Addresses we can't connect
//! ourselves (tcp, launchd…) fall back to zbus's own connect.
//!
//! Use [`session`] / [`system`] (blocking) or [`session_async`] instead of
//! `zbus::Connection::session()` and friends, and [`address`] for an explicit address.

use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use async_io::Async;
use zbus::connection::socket::{BoxedSplit, ReadHalf, Split, WriteHalf};

/// The write half: zbus's own for `Async<UnixStream>`, except `close`.
#[derive(Debug)]
struct Write(Arc<Async<UnixStream>>);

#[async_trait::async_trait]
impl WriteHalf for Write {
    async fn sendmsg(&mut self, buffer: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
        WriteHalf::sendmsg(&mut self.0, buffer, fds).await
    }

    async fn close(&mut self) -> io::Result<()> {
        match self.0.get_ref().shutdown(std::net::Shutdown::Both) {
            Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(()),
            r => r,
        }
    }

    fn can_pass_unix_fd(&self) -> bool {
        true
    }

    async fn peer_credentials(&mut self) -> io::Result<zbus::fdo::ConnectionCredentials> {
        ReadHalf::peer_credentials(&mut self.0).await
    }
}

/// zbus's split for a connected stream, with our write half.
fn split(stream: UnixStream) -> io::Result<BoxedSplit> {
    let s = Arc::new(Async::new(stream)?);
    let read: Box<dyn ReadHalf> = Box::new(s.clone());
    let write: Box<dyn WriteHalf> = Box::new(Write(s));
    Ok(Split::new(read, write))
}

/// The value of `key` in one `transport:k=v,k=v` address, unescaped (`%2f` → `/`).
fn value(params: &str, key: &str) -> Option<Vec<u8>> {
    let raw = params
        .split(',')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))?;
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Connect to the first `unix:path=` / `unix:abstract=` entry of a D-Bus address list we can
/// reach. `None` when the list has no such entry (another transport).
fn connect_unix(addresses: &str) -> Option<io::Result<UnixStream>> {
    use std::os::unix::ffi::OsStrExt;
    let mut last = None;
    for a in addresses.split(';').filter(|a| !a.is_empty()) {
        let Some(params) = a.strip_prefix("unix:") else {
            continue;
        };
        let r = if let Some(path) = value(params, "path") {
            UnixStream::connect(std::ffi::OsStr::from_bytes(&path))
        } else if let Some(name) = value(params, "abstract") {
            use std::os::linux::net::SocketAddrExt;
            std::os::unix::net::SocketAddr::from_abstract_name(&name)
                .and_then(|addr| UnixStream::connect_addr(&addr))
        } else {
            continue;
        };
        match r {
            Ok(s) => return Some(Ok(s)),
            Err(e) => last = Some(Err(e)),
        }
    }
    last
}

fn session_address() -> String {
    std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .ok()
        .filter(|a| !a.is_empty())
        .or_else(|| {
            let dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
            Some(format!("unix:path={dir}/bus"))
        })
        .unwrap_or_default()
}

fn system_address() -> String {
    std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
        .ok()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "unix:path=/var/run/dbus/system_bus_socket".into())
}

/// A message-bus connection (it says Hello) to `address`, blocking API.
pub fn address(address: &str) -> zbus::Result<zbus::blocking::Connection> {
    match connect_unix(address) {
        Some(Ok(stream)) => zbus::blocking::connection::Builder::socket(split(stream)?).build(),
        Some(Err(e)) => Err(e.into()),
        // Not a Unix address: let zbus connect it (rare; costs the idle pool thread).
        None => zbus::blocking::connection::Builder::address(address)?.build(),
    }
}

/// [`address`], async API.
pub async fn address_async(address: &str) -> zbus::Result<zbus::Connection> {
    match connect_unix(address) {
        Some(Ok(stream)) => {
            zbus::connection::Builder::socket(split(stream)?)
                .build()
                .await
        }
        Some(Err(e)) => Err(e.into()),
        None => zbus::connection::Builder::address(address)?.build().await,
    }
}

/// The session bus (`DBUS_SESSION_BUS_ADDRESS`, else `$XDG_RUNTIME_DIR/bus`).
pub fn session() -> zbus::Result<zbus::blocking::Connection> {
    address(&session_address())
}

/// The system bus (`DBUS_SYSTEM_BUS_ADDRESS`, else the standard socket).
pub fn system() -> zbus::Result<zbus::blocking::Connection> {
    address(&system_address())
}

/// The session bus, async API.
pub async fn session_async() -> zbus::Result<zbus::Connection> {
    address_async(&session_address()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_values() {
        assert_eq!(
            value("path=/run/user/1000/bus,guid=abc", "path").unwrap(),
            b"/run/user/1000/bus"
        );
        assert_eq!(
            value("abstract=/tmp/dbus-x%2cy", "abstract").unwrap(),
            b"/tmp/dbus-x,y"
        );
        assert!(value("guid=1", "path").is_none());
        assert!(
            connect_unix("tcp:host=localhost,port=1").is_none(),
            "not ours"
        );
        assert!(
            connect_unix("unix:path=/nonexistent/sliver-bus-test")
                .unwrap()
                .is_err(),
            "ours, but nothing listening"
        );
    }

    /// Connect, call, drop, and the process never grows zbus's `blocking` pool thread.
    #[test]
    fn connects_calls_and_closes_without_the_pool_thread() {
        let Some(bus) = sliver_testbus::TestBus::start() else {
            eprintln!("dbus-run-session not installed; skipping");
            return;
        };
        for _ in 0..3 {
            let c = address(bus.address()).expect("connects");
            let id: String = c
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetId",
                    &(),
                )
                .unwrap()
                .body()
                .deserialize()
                .unwrap();
            assert!(!id.is_empty());
            assert!(c.unique_name().is_some(), "a bus connection (Hello)");
            drop(c);
        }
        let async_conn = zbus::block_on(address_async(bus.address())).expect("async connects");
        drop(async_conn);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let threads: Vec<String> = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
            .map(|s| s.trim().to_owned())
            .collect();
        assert!(
            !threads.iter().any(|t| t.starts_with("blocking")),
            "zbus's blocking pool started: {threads:?}"
        );
    }
}
