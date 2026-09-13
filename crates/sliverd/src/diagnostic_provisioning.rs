//! Private startup capabilities, not installed activation or build attestation.
//! Root declarations and kernel socket identities authorize bounded, single-use
//! role tickets. Writable raw headers never supply authority. See the companion
//! research note for the trust assumptions and intentionally missing integration.
#![allow(dead_code)] // No installed startup caller until lifecycle integration is reviewed.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};

use crate::diagnostic_capture_transport::{
    CaptureLevel, Collector, FixtureBootstrap, MappedWriter, RawReport, SourceRole,
};
use crate::diagnostic_fixture::Mode;

const POLICY: &str = "cbe2b5da809eed7cc4b3406b222baf43e1e34a2d";
const MAX_DECLARATION: usize = 4096;
const MAX_TOTAL_RECORDS: usize = 65_536;
const ACQUISITION_LIMIT: Duration = Duration::from_secs(2);
const MIN_LEAD_NS: u64 = 5_000_000_000;
const MAX_LEAD_NS: u64 = 600_000_000_000;
const ROLES: [SourceRole; 3] = [
    SourceRole::Worker,
    SourceRole::Broker,
    SourceRole::Supervisor,
];

/// Immutable *declared* process identity. This is not executable/RPM provenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: i32,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) start_ticks: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct SourceBinding {
    role: SourceRole,
    process: ProcessIdentity,
    capacity: usize,
}
impl SourceBinding {
    pub(crate) fn role(&self) -> SourceRole {
        self.role
    }
    pub(crate) fn process(&self) -> ProcessIdentity {
        self.process
    }
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Only validated file acquisition constructs this value. No public parser or
/// caller-supplied authority UID. Its bytes remain outside source-writable maps.
pub(crate) struct Declaration {
    bytes: Vec<u8>,
    plan: FixtureBootstrap,
    launcher: ProcessIdentity,
    sources: [SourceBinding; 3],
    authority: Authority,
    acquisition_deadline: Instant,
}

#[derive(Clone, Copy)]
struct Authority {
    uid: u32,
}
impl Authority {
    const ROOT: Self = Self { uid: 0 };
    // Same checks, but an unprivileged temp-directory owner is the test anchor.
    // This constructor does not exist in production.
    #[cfg(test)]
    fn test_owner() -> Self {
        Self {
            uid: unsafe { libc::geteuid() },
        }
    }
}

struct Budget(Instant);
impl Budget {
    fn new(deadline: Instant) -> Result<Self> {
        let budget = Self(deadline.min(Instant::now() + ACQUISITION_LIMIT));
        budget.check()?;
        Ok(budget)
    }
    fn check(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.0,
            "startup acquisition deadline expired"
        );
        Ok(())
    }
    fn wait(&self, fd: &impl AsRawFd, events: i16) -> Result<()> {
        loop {
            self.check()?;
            let remaining = self.0.saturating_duration_since(Instant::now());
            let timeout = remaining
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32;
            let mut poll = libc::pollfd {
                fd: fd.as_raw_fd(),
                events,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut poll, 1, timeout) };
            if result < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::last_os_error()).context("waiting for startup channel");
            }
            self.check()?;
            if result > 0 {
                ensure!(
                    poll.revents & libc::POLLNVAL == 0,
                    "invalid startup channel"
                );
                // Let recv/send report EOF/errors, including HUP with buffered data.
                return Ok(());
            }
        }
    }
}

impl Declaration {
    /// The ONLY path-based production entry. Every component is opened relative
    /// to a held directory with O_NOFOLLOW. Missing provisioning is Off; an
    /// existing unsafe/malformed entry is an error, never a fallback to Off.
    /// This function is not called by ordinary services in this slice.
    pub(crate) fn acquire_startup(deadline: Instant) -> Result<Option<Self>> {
        let budget = Budget::new(deadline)?;
        let root = open_at(libc::AT_FDCWD, "/", true)?;
        check_directory(&root, Authority::ROOT)?;
        Self::acquire_beneath(
            root,
            &["run", "sliver-diagnostic", "startup"],
            Authority::ROOT,
            &budget,
        )
    }

    /// Explicit private capability entry, not an inherited numeric-FD or env
    /// interface. The caller owns FD delivery. We validate the held regular file;
    /// an FD has no path ancestry or no-follow history to authenticate.
    pub(crate) fn acquire_fd(fd: OwnedFd, deadline: Instant) -> Result<Self> {
        Self::read_fd(fd, Authority::ROOT, &Budget::new(deadline)?)
    }

    pub(crate) fn plan(&self) -> &FixtureBootstrap {
        &self.plan
    }
    pub(crate) fn binding(&self, role: SourceRole) -> &SourceBinding {
        &self.sources[index(role)]
    }

    fn acquire_beneath(
        mut directory: OwnedFd,
        parts: &[&str],
        authority: Authority,
        budget: &Budget,
    ) -> Result<Option<Self>> {
        check_directory(&directory, authority)?;
        ensure!(
            !parts.is_empty() && parts.len() <= 8,
            "invalid startup path depth"
        );
        for (position, part) in parts.iter().enumerate() {
            budget.check()?;
            ensure!(
                !part.is_empty() && !matches!(*part, "." | "..") && !part.contains('/'),
                "invalid startup path component"
            );
            let is_directory = position + 1 != parts.len();
            let fd = match open_at(directory.as_raw_fd(), part, is_directory) {
                Ok(fd) => fd,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("opening private startup declaration"),
            };
            if is_directory {
                check_directory(&fd, authority)?;
                directory = fd;
            } else {
                // O_PATH pins the leaf without opening a FIFO/device. Only a
                // checked regular file is reopened through its held descriptor.
                let pinned = stat(&fd)?;
                check_declaration_stat(&pinned, authority)?;
                budget.check()?;
                let read = File::open(format!("/proc/self/fd/{}", fd.as_raw_fd()))?;
                ensure!(
                    stable_file(&pinned, &stat(&read)?),
                    "pinned declaration changed"
                );
                return Self::read_fd(read.into(), authority, budget).map(Some);
            }
        }
        unreachable!()
    }

    fn read_fd(fd: OwnedFd, authority: Authority, budget: &Budget) -> Result<Self> {
        budget.check()?;
        let before = stat(&fd)?;
        check_declaration_stat(&before, authority)?;
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        ensure!(
            flags >= 0 && flags & libc::O_ACCMODE == libc::O_RDONLY && flags & libc::O_PATH == 0,
            "declaration FD must be readable and read-only"
        );
        let mut bytes = vec![0; before.st_size as usize];
        let mut offset = 0;
        while offset < bytes.len() {
            budget.check()?;
            let count = unsafe {
                libc::pread(
                    fd.as_raw_fd(),
                    bytes[offset..].as_mut_ptr().cast(),
                    bytes.len() - offset,
                    offset as libc::off_t,
                )
            };
            if count < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::last_os_error()).context("reading startup declaration");
            }
            ensure!(count > 0, "declaration truncated during read");
            offset += count as usize;
        }
        let after = stat(&fd)?;
        ensure!(
            stable_file(&before, &after),
            "declaration changed during acquisition"
        );
        let mut declaration = Self::parse(bytes, authority)?;
        declaration.acquisition_deadline = budget.0;
        budget.check()?;
        Ok(declaration)
    }

    fn parse(bytes: Vec<u8>, authority: Authority) -> Result<Self> {
        ensure!(bytes.len() <= MAX_DECLARATION, "declaration too large");
        let text = std::str::from_utf8(&bytes)?;
        ensure!(text.ends_with('\n'), "declaration needs final newline");
        let lines: Vec<_> = text[..text.len() - 1].split('\n').collect();
        ensure!(
            lines.len() == 8 && lines[0] == "SLIVER-PROVISIONING-1",
            "unsupported declaration layout"
        );
        ensure!(
            lines[1] == format!("policy {POLICY}"),
            "unapproved provisioning policy"
        );
        ensure!(
            lines[2] == format!("boot {}", boot_id()?),
            "startup declaration belongs to another boot"
        );
        let fields = fields_fn(lines[3], "plan", 7)?;
        let mut plan = FixtureBootstrap::new(
            fields[1],
            fields[2],
            number(fields[3])?,
            number(fields[4])?,
            match fields[5] {
                "A" => Mode::A,
                "B" => Mode::B,
                "C" => Mode::C,
                _ => bail!("invalid capture mode"),
            },
        )?;
        plan.causal = match fields[6] {
            "0" => false,
            "1" => true,
            _ => bail!("invalid causal flag"),
        };
        plan.plan()?;
        let launcher = parse_process(&fields_fn(lines[4], "launcher", 5)?[1..])?;
        ensure!(
            launcher.uid == authority.uid,
            "launcher is not the trusted owner"
        );
        let mut sources = Vec::with_capacity(3);
        let mut total = 0usize;
        for (role, line) in ROLES.into_iter().zip(&lines[5..]) {
            let values = fields_fn(line, role_name(role), 6)?;
            let process = parse_process(&values[1..5])?;
            let capacity: usize = number(values[5])?;
            ensure!(
                (1..=MAX_TOTAL_RECORDS).contains(&capacity),
                "source capacity outside bound"
            );
            total = total
                .checked_add(capacity)
                .context("source budget overflow")?;
            ensure!(
                process.pid != launcher.pid
                    && sources
                        .iter()
                        .all(|s: &SourceBinding| s.process.pid != process.pid),
                "roles require distinct processes"
            );
            sources.push(SourceBinding {
                role,
                process,
                capacity,
            });
        }
        ensure!(
            total <= MAX_TOTAL_RECORDS,
            "total run storage exceeds 65536 records"
        );
        Ok(Self {
            bytes,
            plan,
            launcher,
            sources: sources.try_into().ok().context("missing source roles")?,
            authority,
            acquisition_deadline: Instant::now() + ACQUISITION_LIMIT,
        })
    }

    fn check_plan(&self, expected: &FixtureBootstrap) -> Result<()> {
        ensure!(&self.plan == expected, "startup fixture plan mismatch");
        let now = crate::diagnostic_observer::clock_ns(false)?;
        let lead = self
            .plan
            .start_ns
            .checked_sub(now)
            .context("startup epoch is stale")?;
        ensure!(
            lead > MIN_LEAD_NS && lead <= MAX_LEAD_NS,
            "startup epoch must be >5s and <=10min ahead"
        );
        Ok(())
    }

    /// Consumes one declaration and one already-connected private launcher
    /// channel. No source mapping is accepted before both process identities and
    /// the exact declaration have been checked. No source strings are trusted.
    pub(crate) fn receive(
        self,
        role: SourceRole,
        expected: &FixtureBootstrap,
        stream: UnixStream,
        deadline: Instant,
    ) -> Result<StartupCapture> {
        let budget = Budget::new(deadline.min(self.acquisition_deadline))?;
        self.check_plan(expected)?;
        let binding = self.binding(role).clone();
        ensure!(
            current_process()? == binding.process,
            "startup recipient process mismatch"
        );
        let peer = authenticate_peer(&stream, self.launcher, &budget)?;
        let mut announcement = vec![0; self.bytes.len()];
        read_exact_channel(&stream, &mut announcement, &budget)?;
        ensure!(announcement == self.bytes, "launcher declaration mismatch");
        let fd = receive_right(&stream, role, &budget)?;
        peer.check_alive()?;
        self.check_plan(expected)?;
        let writer = MappedWriter::receive_for_role(fd, role)?;
        ensure!(
            writer.capacity() == binding.capacity,
            "source mapping capacity mismatch"
        );
        budget.check()?;
        Ok(StartupCapture {
            declaration: self,
            binding,
            writer,
        })
    }
}

/// A role/process/plan-bound writer, not proof of source health or native work.
/// Callers must install minimal timing and calibrate before any workload.
pub(crate) struct StartupCapture {
    declaration: Declaration,
    binding: SourceBinding,
    writer: MappedWriter,
}
impl StartupCapture {
    pub(crate) fn plan(&self) -> &FixtureBootstrap {
        &self.declaration.plan
    }
    pub(crate) fn binding(&self) -> &SourceBinding {
        &self.binding
    }
    pub(crate) fn into_writer(self) -> (MappedWriter, CaptureLevel) {
        (self.writer, self.declaration.plan.capture_level())
    }
}

/// The trusted launcher's registry. Exactly three sources are preallocated,
/// total <=32 MiB + three headers. Each ticket is attempted once, including
/// failed attempts. No replacement PID or retry can silently replace a source.
/// The same trusted launcher must own the sole registry for an attempt.
pub(crate) struct Launcher {
    declaration: Declaration,
    collectors: [Collector; 3],
    attempted: [bool; 3],
}
impl Launcher {
    pub(crate) fn new(declaration: Declaration, deadline: Instant) -> Result<Self> {
        let budget = Budget::new(deadline.min(declaration.acquisition_deadline))?;
        ensure!(
            current_process()? == declaration.launcher,
            "not the declared trusted launcher"
        );
        ensure!(
            declaration.launcher.uid == declaration.authority.uid,
            "untrusted launcher"
        );
        declaration.check_plan(&declaration.plan)?;
        let mut collectors = Vec::with_capacity(3);
        for source in &declaration.sources {
            budget.check()?;
            collectors.push(Collector::for_role(source.capacity, source.role)?);
        }
        budget.check()?;
        Ok(Self {
            declaration,
            collectors: collectors.try_into().ok().context("missing collectors")?,
            attempted: [false; 3],
        })
    }
    pub(crate) fn send(
        &mut self,
        role: SourceRole,
        expected: &FixtureBootstrap,
        stream: UnixStream,
        deadline: Instant,
    ) -> Result<()> {
        let slot = index(role);
        ensure!(!self.attempted[slot], "source ticket already attempted");
        self.attempted[slot] = true;
        let budget = Budget::new(deadline.min(self.declaration.acquisition_deadline))?;
        self.declaration.check_plan(expected)?;
        ensure!(
            current_process()? == self.declaration.launcher,
            "launcher process changed"
        );
        let peer = authenticate_peer(&stream, self.declaration.binding(role).process, &budget)?;
        let fd = self.collectors[slot].take_storage(role)?;
        write_all_channel(&stream, &self.declaration.bytes, &budget)?;
        peer.check_alive()?;
        send_right(&stream, role, &fd, &budget)?;
        budget.check()?;
        Ok(())
    }
    pub(crate) fn binding(&self, role: SourceRole) -> &SourceBinding {
        self.declaration.binding(role)
    }
    pub(crate) fn plan(&self) -> &FixtureBootstrap {
        self.declaration.plan()
    }
    pub(crate) fn snapshot(&self, role: SourceRole) -> Result<RawReport> {
        self.collectors[index(role)].snapshot()
    }
}

fn index(role: SourceRole) -> usize {
    match role {
        SourceRole::Worker => 0,
        SourceRole::Broker => 1,
        SourceRole::Supervisor => 2,
    }
}
fn role_name(role: SourceRole) -> &'static str {
    match role {
        SourceRole::Worker => "worker",
        SourceRole::Broker => "broker",
        SourceRole::Supervisor => "supervisor",
    }
}
fn fields_fn<'a>(line: &'a str, label: &str, count: usize) -> Result<Vec<&'a str>> {
    let values: Vec<_> = line.split(' ').collect();
    ensure!(
        values.len() == count && values[0] == label && values.iter().all(|s| !s.is_empty()),
        "invalid {label} declaration"
    );
    Ok(values)
}
fn number<T: std::str::FromStr + ToString>(text: &str) -> Result<T> {
    let value = text
        .parse::<T>()
        .map_err(|_| anyhow::anyhow!("invalid declaration integer"))?;
    ensure!(
        value.to_string() == text,
        "noncanonical declaration integer"
    );
    Ok(value)
}
fn parse_process(values: &[&str]) -> Result<ProcessIdentity> {
    let process = ProcessIdentity {
        pid: number(values[0])?,
        uid: number(values[1])?,
        gid: number(values[2])?,
        start_ticks: number(values[3])?,
    };
    ensure!(
        process.pid > 0 && process.start_ticks > 0,
        "invalid process identity"
    );
    Ok(process)
}
fn open_at(directory: i32, name: &str, is_directory: bool) -> io::Result<OwnedFd> {
    let name = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let flags = libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | if is_directory {
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_DIRECTORY
        } else {
            libc::O_PATH
        };
    let raw = unsafe { libc::openat(directory, name.as_ptr(), flags) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
fn stat(fd: &impl AsRawFd) -> Result<libc::stat> {
    let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
    ensure!(
        unsafe { libc::fstat(fd.as_raw_fd(), value.as_mut_ptr()) } == 0,
        "stat startup capability: {}",
        io::Error::last_os_error()
    );
    Ok(unsafe { value.assume_init() })
}
fn check_directory(fd: &OwnedFd, authority: Authority) -> Result<()> {
    let value = stat(fd)?;
    ensure!(
        value.st_mode & libc::S_IFMT == libc::S_IFDIR
            && value.st_uid == authority.uid
            && value.st_mode & 0o7022 == 0,
        "unsafe startup directory"
    );
    Ok(())
}
fn check_declaration_stat(value: &libc::stat, authority: Authority) -> Result<()> {
    ensure!(
        value.st_mode & libc::S_IFMT == libc::S_IFREG,
        "declaration is not a regular file"
    );
    ensure!(
        value.st_uid == authority.uid,
        "declaration owner is not trusted"
    );
    ensure!(
        value.st_mode & 0o7133 == 0,
        "unsafe declaration permissions"
    );
    ensure!(
        value.st_nlink == 1,
        "declaration must have exactly one link"
    );
    ensure!(
        (1..=MAX_DECLARATION as i64).contains(&value.st_size),
        "declaration size outside bound"
    );
    Ok(())
}
fn stable_file(a: &libc::stat, b: &libc::stat) -> bool {
    a.st_dev == b.st_dev
        && a.st_ino == b.st_ino
        && a.st_mode == b.st_mode
        && a.st_uid == b.st_uid
        && a.st_gid == b.st_gid
        && a.st_nlink == b.st_nlink
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}

fn boot_id() -> Result<String> {
    let mut bytes = Vec::with_capacity(38);
    File::open("/proc/sys/kernel/random/boot_id")?
        .take(38)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() == 37 && bytes[36] == b'\n',
        "invalid host boot identity"
    );
    let text = std::str::from_utf8(&bytes[..36])?;
    ensure!(
        text.bytes()
            .enumerate()
            .all(|(i, b)| if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit() && !b.is_ascii_uppercase()
            }),
        "invalid host boot identity"
    );
    Ok(text.into())
}

fn current_process() -> Result<ProcessIdentity> {
    let pid = std::process::id() as i32;
    Ok(ProcessIdentity {
        pid,
        uid: unsafe { libc::geteuid() },
        gid: unsafe { libc::getegid() },
        start_ticks: process_start(pid)?,
    })
}
fn process_start(pid: i32) -> Result<u64> {
    ensure!(pid > 0, "invalid process PID");
    let mut bytes = Vec::with_capacity(4097);
    File::open(format!("/proc/{pid}/stat"))?
        .take(4097)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "process stat exceeds bound");
    let text = std::str::from_utf8(&bytes)?;
    // comm may contain spaces, newlines and ')'; the final ')' closes field 2.
    let (_, tail) = text.rsplit_once(')').context("malformed process stat")?;
    number(
        tail.split_ascii_whitespace()
            .nth(19)
            .context("missing process start time")?,
    )
}

struct Peer(OwnedFd);
impl Peer {
    fn check_alive(&self) -> Result<()> {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        ensure!(
            unsafe { libc::poll(&mut poll, 1, 0) } == 0,
            "startup peer exited or pidfd invalid"
        );
        Ok(())
    }
}
fn authenticate_peer(
    stream: &UnixStream,
    expected: ProcessIdentity,
    budget: &Budget,
) -> Result<Peer> {
    budget.check()?;
    let credentials = crate::peer_credentials::read(stream)?;
    ensure!(
        credentials.pid == expected.pid
            && credentials.uid == expected.uid
            && credentials.gid == expected.gid,
        "startup peer credentials mismatch"
    );
    // Unlike pidfd_open(SO_PEERCRED.pid), this refers to the actual socket peer,
    // not a new process that reused its PID. No unsafe old-kernel fallback.
    let mut raw = -1i32;
    let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&mut raw as *mut i32).cast(),
            &mut length,
        )
    };
    ensure!(
        result == 0 && raw >= 0,
        "SO_PEERPIDFD required for startup binding: {}",
        io::Error::last_os_error()
    );
    let peer = Peer(unsafe { OwnedFd::from_raw_fd(raw) });
    ensure!(
        length as usize == std::mem::size_of::<i32>(),
        "invalid peer pidfd length"
    );
    peer.check_alive()?;
    ensure!(
        process_start(expected.pid)? == expected.start_ticks,
        "startup peer process start mismatch"
    );
    peer.check_alive()?;
    budget.check()?;
    Ok(peer)
}

fn retry(error: io::Error) -> Result<()> {
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    ) {
        return Ok(());
    }
    Err(error).context("startup channel I/O")
}
fn write_all_channel(stream: &UnixStream, bytes: &[u8], budget: &Budget) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        budget.check()?;
        let count = unsafe {
            libc::send(
                stream.as_raw_fd(),
                bytes[offset..].as_ptr().cast(),
                bytes.len() - offset,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if count < 0 {
            retry(io::Error::last_os_error())?;
            budget.wait(stream, libc::POLLOUT)?;
            continue;
        }
        ensure!(
            count > 0,
            "startup channel closed while sending declaration"
        );
        offset += count as usize;
    }
    Ok(())
}
fn read_exact_channel(stream: &UnixStream, bytes: &mut [u8], budget: &Budget) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        budget.check()?;
        // Use recvmsg even for declaration bytes: never silently discard rights
        // attached at an unexpected position in the stream.
        let (count, fds, invalid) = recv_message(stream, &mut bytes[offset..])?;
        if count < 0 {
            budget.wait(stream, libc::POLLIN)?;
            continue;
        }
        ensure!(
            !invalid && fds.is_empty(),
            "unexpected declaration ancillary data"
        );
        ensure!(count > 0, "startup channel closed during declaration");
        offset += count as usize;
    }
    Ok(())
}
fn send_right(stream: &UnixStream, role: SourceRole, fd: &OwnedFd, budget: &Budget) -> Result<()> {
    let mut tag = index(role) as u8 + 1;
    let mut iov = libc::iovec {
        iov_base: (&mut tag as *mut u8).cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 4];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen =
        unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize;
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as usize;
        libc::CMSG_DATA(header)
            .cast::<i32>()
            .write_unaligned(fd.as_raw_fd());
    }
    loop {
        budget.check()?;
        let count = unsafe {
            libc::sendmsg(
                stream.as_raw_fd(),
                &message,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if count < 0 {
            retry(io::Error::last_os_error())?;
            budget.wait(stream, libc::POLLOUT)?;
            continue;
        }
        ensure!(count == 1, "startup descriptor send closed");
        return Ok(());
    }
}
fn receive_right(stream: &UnixStream, role: SourceRole, budget: &Budget) -> Result<OwnedFd> {
    loop {
        budget.check()?;
        let mut byte = [0];
        let (count, mut fds, invalid) = recv_message(stream, &mut byte)?;
        if count < 0 {
            budget.wait(stream, libc::POLLIN)?;
            continue;
        }
        ensure!(
            count == 1 && !invalid && byte[0] == index(role) as u8 + 1 && fds.len() == 1,
            "invalid startup descriptor envelope"
        );
        return Ok(fds.pop().expect("one validated FD"));
    }
}
// Linux include/linux/socket.h UAPI ancillary type, not exported by libc 0.2.
// SO_PASSPIDFD installs a descriptor just like SCM_RIGHTS, even though this
// private protocol never accepts it as a capability.
const SCM_PIDFD: i32 = 4;

/// Always own/close all delivered rights before rejecting ancillary data.
fn recv_message(stream: &UnixStream, bytes: &mut [u8]) -> Result<(isize, Vec<OwnedFd>, bool)> {
    let mut control = [0usize; 4];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let count = unsafe {
        libc::recvmsg(
            stream.as_raw_fd(),
            &mut message,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if count < 0 {
        retry(io::Error::last_os_error())?;
        return Ok((-1, Vec::new(), false));
    }
    let mut fds = Vec::with_capacity(8);
    let mut invalid = message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0;
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level != libc::SOL_SOCKET
                || !matches!((*header).cmsg_type, libc::SCM_RIGHTS | SCM_PIDFD)
            {
                invalid = true;
            } else {
                if (*header).cmsg_type == SCM_PIDFD {
                    invalid = true;
                }
                let length = (*header)
                    .cmsg_len
                    .saturating_sub(libc::CMSG_LEN(0) as usize);
                if !length.is_multiple_of(std::mem::size_of::<i32>()) {
                    invalid = true;
                }
                for i in 0..length / std::mem::size_of::<i32>() {
                    fds.push(OwnedFd::from_raw_fd(
                        libc::CMSG_DATA(header)
                            .cast::<i32>()
                            .add(i)
                            .read_unaligned(),
                    ));
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    Ok((count, fds, invalid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::process::{Child, Command};

    fn deadline() -> Instant {
        Instant::now() + ACQUISITION_LIMIT
    }
    fn plan(mode: Mode) -> FixtureBootstrap {
        FixtureBootstrap::new(
            "provisioning-test",
            "generation-1",
            crate::diagnostic_observer::clock_ns(false).unwrap() + 30_000_000_000,
            30,
            mode,
        )
        .unwrap()
    }
    fn fake_sources() -> [ProcessIdentity; 3] {
        let me = current_process().unwrap();
        [0, 1, 2].map(|i| ProcessIdentity {
            pid: i32::MAX - i,
            ..me
        })
    }
    fn declaration_bytes(
        plan: &FixtureBootstrap,
        processes: [ProcessIdentity; 3],
        capacities: [usize; 3],
    ) -> Vec<u8> {
        let launcher = current_process().unwrap();
        let mut text = format!(
            "SLIVER-PROVISIONING-1\npolicy {POLICY}\nboot {}\nplan {} {} {} {} {} {}\nlauncher {} {} {} {}\n",
            boot_id().unwrap(),
            plan.run,
            plan.generation,
            plan.start_ns,
            plan.rate,
            match plan.mode {
                Mode::A => "A",
                Mode::B => "B",
                Mode::C => "C",
            },
            u8::from(plan.causal),
            launcher.pid,
            launcher.uid,
            launcher.gid,
            launcher.start_ticks
        );
        for ((role, process), capacity) in ROLES.into_iter().zip(processes).zip(capacities) {
            text.push_str(&format!(
                "{} {} {} {} {} {}\n",
                role_name(role),
                process.pid,
                process.uid,
                process.gid,
                process.start_ticks,
                capacity
            ));
        }
        text.into_bytes()
    }
    fn put(directory: &std::path::Path, bytes: &[u8]) -> Result<()> {
        std::fs::write(directory.join("startup"), bytes)?;
        std::fs::set_permissions(
            directory.join("startup"),
            std::fs::Permissions::from_mode(0o600),
        )?;
        Ok(())
    }
    fn acquire(directory: &std::path::Path) -> Result<Option<Declaration>> {
        Declaration::acquire_beneath(
            open_at(libc::AT_FDCWD, directory.to_str().unwrap(), true)?,
            &["startup"],
            Authority::test_owner(),
            &Budget::new(deadline())?,
        )
    }
    fn valid_declaration(mode: Mode) -> Declaration {
        Declaration::parse(
            declaration_bytes(&plan(mode), fake_sources(), [64; 3]),
            Authority::test_owner(),
        )
        .unwrap()
    }

    #[test]
    fn absent_is_off_but_existing_invalid_is_not_off() -> Result<()> {
        let directory = tempfile::tempdir()?;
        assert!(acquire(directory.path())?.is_none());
        put(directory.path(), b"not a declaration")?;
        assert!(acquire(directory.path()).is_err());
        put(
            directory.path(),
            &declaration_bytes(&plan(Mode::C), fake_sources(), [64; 3]),
        )?;
        let declaration = acquire(directory.path())?.unwrap();
        assert_eq!(declaration.plan.mode, Mode::C);
        assert_eq!(declaration.binding(SourceRole::Broker).capacity(), 64);
        Ok(())
    }

    #[test]
    fn no_follow_rejects_leaf_and_directory_links_and_unsafe_ancestors() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        put(
            other.path(),
            &declaration_bytes(&plan(Mode::A), fake_sources(), [1; 3]),
        )?;
        symlink(
            other.path().join("startup"),
            directory.path().join("startup"),
        )?;
        assert!(acquire(directory.path()).is_err());
        std::fs::remove_file(directory.path().join("startup"))?;
        symlink(other.path(), directory.path().join("sub"))?;
        let beneath = |parts: &[&str]| {
            Declaration::acquire_beneath(
                open_at(libc::AT_FDCWD, directory.path().to_str().unwrap(), true)?,
                parts,
                Authority::test_owner(),
                &Budget::new(deadline())?,
            )
        };
        assert!(beneath(&["sub", "startup"]).is_err());
        assert!(beneath(&["..", "startup"]).is_err());
        assert!(beneath(&["sub/startup"]).is_err());
        assert!(beneath(&[""]).is_err());
        assert!(beneath(&["missing", "startup"])?.is_none());
        for mode in [0o777, 0o770, 0o1777, 0o2700] {
            std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(mode))?;
            assert!(acquire(directory.path()).is_err());
        }
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    #[test]
    fn leaf_rejects_hardlinks_permissions_fifo_socket_directory_and_sizes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let bytes = declaration_bytes(&plan(Mode::A), fake_sources(), [1; 3]);
        put(directory.path(), &bytes)?;
        for mode in [0o620, 0o602, 0o700, 0o4600] {
            std::fs::set_permissions(
                directory.path().join("startup"),
                std::fs::Permissions::from_mode(mode),
            )?;
            assert!(acquire(directory.path()).is_err());
        }
        put(directory.path(), &bytes)?;
        std::fs::hard_link(
            directory.path().join("startup"),
            directory.path().join("alias"),
        )?;
        assert!(acquire(directory.path()).is_err());
        std::fs::remove_file(directory.path().join("alias"))?;
        for size in [0, MAX_DECLARATION + 1] {
            put(directory.path(), &vec![b'x'; size])?;
            assert!(acquire(directory.path()).is_err());
        }
        std::fs::remove_file(directory.path().join("startup"))?;
        let path = CString::new(directory.path().join("startup").to_str().unwrap())?;
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let before = Instant::now();
        assert!(acquire(directory.path()).is_err());
        assert!(before.elapsed() < Duration::from_secs(1));
        std::fs::remove_file(directory.path().join("startup"))?;
        let _socket = UnixListener::bind(directory.path().join("startup"))?;
        assert!(acquire(directory.path()).is_err());
        std::fs::remove_file(directory.path().join("startup"))?;
        std::fs::create_dir(directory.path().join("startup"))?;
        assert!(acquire(directory.path()).is_err());
        Ok(())
    }

    #[test]
    fn fd_acquisition_checks_owner_access_and_reads_from_zero_without_seeking() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let bytes = declaration_bytes(&plan(Mode::B), fake_sources(), [64; 3]);
        put(directory.path(), &bytes)?;
        let path = directory.path().join("startup");
        let mut file = File::open(&path)?;
        let mut prefix = [0; 7];
        file.read_exact(&mut prefix)?;
        let fd: OwnedFd = file.into();
        let clone = fd.try_clone()?;
        let declaration =
            Declaration::read_fd(fd, Authority::test_owner(), &Budget::new(deadline())?)?;
        assert_eq!(declaration.bytes, bytes);
        assert_eq!(
            unsafe { libc::lseek(clone.as_raw_fd(), 0, libc::SEEK_CUR) },
            7
        );
        let writable = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?;
        assert!(Declaration::read_fd(
            writable.into(),
            Authority::test_owner(),
            &Budget::new(deadline())?
        )
        .is_err());
        assert!(Declaration::read_fd(
            open_at(libc::AT_FDCWD, path.to_str().unwrap(), false)?,
            Authority::test_owner(),
            &Budget::new(deadline())?
        )
        .is_err());
        let wrong = Authority {
            uid: Authority::test_owner().uid ^ 1,
        };
        assert!(
            Declaration::read_fd(File::open(&path)?.into(), wrong, &Budget::new(deadline())?)
                .is_err()
        );
        if unsafe { libc::geteuid() } != 0 {
            assert!(Declaration::acquire_fd(File::open(&path)?.into(), deadline()).is_err());
        }
        let mut before = stat(&File::open(&path)?)?;
        let after = before;
        before.st_ctime_nsec ^= 1;
        assert!(!stable_file(&before, &after));
        Ok(())
    }

    #[test]
    fn declaration_is_strict_policy_boot_plan_process_and_total_budget() {
        let fixture = plan(Mode::C);
        let original =
            String::from_utf8(declaration_bytes(&fixture, fake_sources(), [64; 3])).unwrap();
        for bytes in [
            original.replace("PROVISIONING-1", "PROVISIONING-2"),
            original.replace(POLICY, "wrong"),
            original.replace(&boot_id().unwrap(), "00000000-0000-0000-0000-000000000000"),
            original.replace("plan ", "plan  "),
            original.replace(" 30 C 0\n", " 60 C 1\n"),
            original.replace(" 30 C 0\n", " 31 C 0\n"),
            original.replace(" 30 C 0\n", " 30 D 0\n"),
            original.replace(" 30 C 0\n", " 30 C 2\n"),
            original.replace(" 64\n", " 065\n"),
            original.replace(" 64\n", " 0\n"),
            original.replace("worker ", "supervisor "),
            original.replace("generation-1", "bad/name"),
            original.replace("generation-1", &"a".repeat(129)),
            original.replace(&fixture.start_ns.to_string(), &u64::MAX.to_string()),
            original.trim_end().to_owned(),
            format!("{original}extra\n"),
        ] {
            assert!(Declaration::parse(bytes.into_bytes(), Authority::test_owner()).is_err());
        }
        let mut processes = fake_sources();
        processes[1] = processes[0];
        assert!(Declaration::parse(
            declaration_bytes(&fixture, processes, [1; 3]),
            Authority::test_owner()
        )
        .is_err());
        processes = fake_sources();
        processes[0] = current_process().unwrap();
        assert!(Declaration::parse(
            declaration_bytes(&fixture, processes, [1; 3]),
            Authority::test_owner()
        )
        .is_err());
        assert!(Declaration::parse(
            declaration_bytes(&fixture, fake_sources(), [21_845, 21_845, 21_846]),
            Authority::test_owner()
        )
        .is_ok());
        assert!(Declaration::parse(
            declaration_bytes(&fixture, fake_sources(), [21_845, 21_845, 21_847]),
            Authority::test_owner()
        )
        .is_err());
        assert!(
            Declaration::parse(vec![b'x'; MAX_DECLARATION + 1], Authority::test_owner()).is_err()
        );
        assert!(Declaration::parse(Vec::new(), Authority::test_owner()).is_err());
    }

    #[test]
    fn every_plan_field_and_epoch_is_checked() {
        let declaration = valid_declaration(Mode::C);
        let original = declaration.plan.clone();
        assert!(declaration.check_plan(&original).is_ok());
        for field in 0..6 {
            let mut changed = original.clone();
            match field {
                0 => changed.run.push('x'),
                1 => changed.generation.push('x'),
                2 => changed.start_ns += 1,
                3 => changed.rate = 60,
                4 => changed.mode = Mode::B,
                5 => changed.causal = true,
                _ => unreachable!(),
            }
            assert!(declaration.check_plan(&changed).is_err());
        }
        for lead in [0, MIN_LEAD_NS, MAX_LEAD_NS + 1_000_000_000] {
            let mut declaration = valid_declaration(Mode::A);
            declaration.plan.start_ns = crate::diagnostic_observer::clock_ns(false).unwrap() + lead;
            assert!(declaration.check_plan(&declaration.plan).is_err());
        }
    }

    #[test]
    fn socket_authentication_checks_all_kernel_identity_fields() -> Result<()> {
        let (socket, _other) = UnixStream::pair()?;
        let me = current_process()?;
        let budget = Budget::new(deadline())?;
        let peer = authenticate_peer(&socket, me, &budget)?;
        assert!(unsafe { libc::fcntl(peer.0.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
        for field in 0..4 {
            let mut wrong = me;
            match field {
                0 => wrong.pid += 1,
                1 => wrong.uid ^= 1,
                2 => wrong.gid ^= 1,
                3 => wrong.start_ticks += 1,
                _ => unreachable!(),
            }
            assert!(authenticate_peer(&socket, wrong, &budget).is_err());
        }
        Ok(())
    }

    #[test]
    fn deadline_is_shared_by_partial_payload_and_rights_and_clamped() -> Result<()> {
        let budget = Budget::new(Instant::now() + Duration::from_secs(100))?;
        assert!(budget.0 <= Instant::now() + ACQUISITION_LIMIT);
        assert!(Budget::new(Instant::now()).is_err());
        let (a, b) = UnixStream::pair()?;
        let budget = Budget::new(Instant::now() + Duration::from_millis(40))?;
        write_all_channel(&a, b"x", &budget)?;
        let mut byte = [0];
        read_exact_channel(&b, &mut byte, &budget)?;
        let start = Instant::now();
        assert!(receive_right(&b, SourceRole::Broker, &budget).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        // A partial declaration does not renew the same deadline either.
        let (a, b) = UnixStream::pair()?;
        write_all_channel(&a, b"x", &Budget::new(deadline())?)?;
        assert!(read_exact_channel(
            &b,
            &mut [0; 2],
            &Budget::new(Instant::now() + Duration::from_millis(20))?
        )
        .is_err());
        drop(a);
        assert!(receive_right(&b, SourceRole::Broker, &Budget::new(deadline())?).is_err());
        Ok(())
    }

    fn send_many(socket: &UnixStream, fds: &[i32], tag: u8) {
        let mut tag = tag;
        let mut iov = libc::iovec {
            iov_base: (&mut tag as *mut u8).cast(),
            iov_len: 1,
        };
        let mut control = [0usize; 32];
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        if !fds.is_empty() {
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen =
                unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
            unsafe {
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as usize;
                std::ptr::copy_nonoverlapping(
                    fds.as_ptr(),
                    libc::CMSG_DATA(header).cast(),
                    fds.len(),
                );
            }
        }
        assert_eq!(
            unsafe { libc::sendmsg(socket.as_raw_fd(), &message, libc::MSG_NOSIGNAL) },
            1
        );
    }

    #[test]
    fn rights_reject_missing_extra_truncated_wrong_role_and_unexpected_position() -> Result<()> {
        let mut collector = Collector::for_role(64, SourceRole::Broker)?;
        let fd = collector.take_storage(SourceRole::Broker)?;
        for count in [0, 2, 16] {
            let (a, b) = UnixStream::pair()?;
            send_many(&a, &vec![fd.as_raw_fd(); count], 2);
            assert!(receive_right(&b, SourceRole::Broker, &Budget::new(deadline())?).is_err());
        }
        let (a, b) = UnixStream::pair()?;
        send_right(&a, SourceRole::Worker, &fd, &Budget::new(deadline())?)?;
        assert!(receive_right(&b, SourceRole::Broker, &Budget::new(deadline())?).is_err());
        let (a, b) = UnixStream::pair()?;
        send_right(&a, SourceRole::Broker, &fd, &Budget::new(deadline())?)?;
        assert!(read_exact_channel(&b, &mut [0], &Budget::new(deadline())?).is_err());
        // A correctly framed FD still needs the raw transport's role/type/seal checks.
        let (a, b) = UnixStream::pair()?;
        send_right(&a, SourceRole::Supervisor, &fd, &Budget::new(deadline())?)?;
        let received = receive_right(&b, SourceRole::Supervisor, &Budget::new(deadline())?)?;
        assert!(
            unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC != 0
        );
        assert!(MappedWriter::receive_for_role(received, SourceRole::Supervisor).is_err());
        let file = tempfile::tempfile()?;
        assert!(MappedWriter::receive_for_role(file.into(), SourceRole::Broker).is_err());
        Ok(())
    }

    #[test]
    fn rejected_ancillary_closes_every_delivered_descriptor() -> Result<()> {
        for count in [2, 16] {
            let mut pipe = [-1; 2];
            assert_eq!(
                unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
                0
            );
            let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
            let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
            let (a, b) = UnixStream::pair()?;
            send_many(&a, &vec![write.as_raw_fd(); count], 2);
            drop(write);
            assert!(receive_right(&b, SourceRole::Broker, &Budget::new(deadline())?).is_err());
            let mut byte = 0u8;
            // EAGAIN would mean some write descriptor leaked. EOF proves all
            // received duplicates (and the kernel-truncated remainder) closed.
            assert_eq!(
                unsafe { libc::read(read.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) },
                0
            );
        }
        let (a, b) = UnixStream::pair()?;
        let enabled = 1i32;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    b.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PASSCRED,
                    (&enabled as *const i32).cast(),
                    std::mem::size_of_val(&enabled) as libc::socklen_t,
                )
            },
            0
        );
        send_many(&a, &[], 2);
        assert!(receive_right(&b, SourceRole::Broker, &Budget::new(deadline())?).is_err());
        Ok(())
    }

    #[test]
    fn scm_pidfd_is_owned_and_closed_on_rejection() -> Result<()> {
        // Isolate descriptor-number checks from unrelated parallel tests.
        const CHILD: &str = "SLIVER_PROVISIONING_TEST_PIDFD_CLEANUP";
        if std::env::var_os(CHILD).is_none() {
            let mut child = ChildGuard(Command::new(std::env::current_exe()?)
                .args(["--exact", "diagnostic_provisioning::tests::scm_pidfd_is_owned_and_closed_on_rejection", "--nocapture"])
                .env(CHILD, "1").spawn()?);
            return child.wait();
        }
        let (a, b) = UnixStream::pair()?;
        let enabled = 1i32;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    b.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PASSPIDFD,
                    (&enabled as *const i32).cast(),
                    std::mem::size_of_val(&enabled) as libc::socklen_t,
                )
            },
            0
        );
        send_many(&a, &[], 2);
        let (count, fds, invalid) = recv_message(&b, &mut [0])?;
        assert_eq!(count, 1);
        assert!(invalid, "SCM_PIDFD must not become an accepted capability");
        assert_eq!(
            fds.len(),
            1,
            "installed ancillary pidfd must be owned before rejection"
        );
        let descriptor = fds[0].as_raw_fd();
        assert!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) } >= 0);
        drop(fds);
        assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        let before = std::fs::read_dir("/proc/self/fd")?.count();
        for _ in 0..16 {
            send_many(&a, &[], 2);
            assert!(receive_right(&b, SourceRole::Broker, &Budget::new(deadline())?).is_err());
        }
        assert_eq!(std::fs::read_dir("/proc/self/fd")?.count(), before);
        Ok(())
    }

    #[test]
    fn acquisition_deadline_cannot_be_renewed_by_a_later_stage() -> Result<()> {
        let mut declaration = valid_declaration(Mode::C);
        declaration.acquisition_deadline = Instant::now();
        assert!(Launcher::new(declaration, deadline()).is_err());
        let mut declaration = valid_declaration(Mode::C);
        declaration.acquisition_deadline = Instant::now();
        let fixture = declaration.plan.clone();
        let (a, _b) = UnixStream::pair()?;
        assert!(declaration
            .receive(SourceRole::Broker, &fixture, a, deadline())
            .err()
            .unwrap()
            .to_string()
            .contains("deadline"));
        Ok(())
    }

    #[test]
    fn failed_ticket_is_not_reissued_or_rebound() -> Result<()> {
        let declaration = valid_declaration(Mode::C);
        let fixture = declaration.plan.clone();
        let mut launcher = Launcher::new(declaration, deadline())?;
        let (a, _b) = UnixStream::pair()?;
        assert!(launcher
            .send(SourceRole::Broker, &fixture, a, deadline())
            .is_err());
        let (a, _b) = UnixStream::pair()?;
        let error = launcher
            .send(SourceRole::Broker, &fixture, a, deadline())
            .unwrap_err();
        assert!(error.to_string().contains("already attempted"));
        assert!(!launcher.snapshot(SourceRole::Broker)?.initialized);
        let declaration = valid_declaration(Mode::C);
        let (a, _b) = UnixStream::pair()?;
        assert!(declaration
            .receive(SourceRole::Broker, &fixture, a, deadline())
            .is_err());
        Ok(())
    }

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    impl ChildGuard {
        fn wait(&mut self) -> Result<()> {
            let end = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = self.0.try_wait()? {
                    ensure!(status.success(), "recipient child failed");
                    return Ok(());
                }
                ensure!(Instant::now() < end, "recipient child timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    const CHILD_DIRECTORY: &str = "SLIVER_PROVISIONING_TEST_DIRECTORY";
    const CHILD_ROLE: &str = "SLIVER_PROVISIONING_TEST_ROLE";
    const CHILD_FAILURE: &str = "SLIVER_PROVISIONING_TEST_FAILURE";

    #[test]
    fn recipient_child() -> Result<()> {
        // Test executable dispatch only. No production environment activation.
        let Some(directory) = std::env::var_os(CHILD_DIRECTORY) else {
            return Ok(());
        };
        let directory = std::path::PathBuf::from(directory);
        let role = match std::env::var(CHILD_ROLE)?.as_str() {
            "worker" => SourceRole::Worker,
            "broker" => SourceRole::Broker,
            "supervisor" => SourceRole::Supervisor,
            _ => bail!("bad test role"),
        };
        let mut stream = UnixStream::connect(directory.join(role_name(role)))?;
        let failure = std::env::var(CHILD_FAILURE).unwrap_or_default();
        if failure == "exit" {
            return Ok(());
        }
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.read_exact(&mut [0])?; // parent has written the exact actual child IDs
        let declaration = acquire(&directory)?.context("missing test declaration")?;
        let fixture = declaration.plan.clone();
        let result = declaration.receive(role, &fixture, stream, deadline());
        if !failure.is_empty() {
            let error = result
                .err()
                .context("bad startup capability was accepted")?
                .to_string();
            ensure!(
                error.contains(&failure),
                "wrong rejection: expected {failure}, got {error}"
            );
            return Ok(());
        }
        let capture = result?;
        assert_eq!(capture.binding().role(), role);
        assert_eq!(capture.binding().process(), current_process()?);
        assert_eq!(capture.plan(), &fixture);
        if role == SourceRole::Broker {
            use crate::hardware::TouchBarHardware;
            let source = crate::diagnostic_broker::BrokerCapture::from_startup(capture)?;
            let mut hardware = crate::diagnostic_hardware::ObservedHardware::for_broker(
                crate::hardware::FakeTouchBar::new(),
                source.clone(),
            );
            hardware.claim()?;
            let canvas = crate::frame_canvas::FrameCanvas::new()?;
            if fixture.mode != Mode::A {
                let draw = (|| -> mlua::Result<()> {
                    let lua = mlua::Lua::new();
                    let marker: mlua::Table = lua
                        .load(&include_bytes!("../../../scripts/native-performance-marker.lua")[..])
                        .eval()?;
                    let identity = lua.create_table()?;
                    identity.set("run_id", fixture.run.as_str())?;
                    identity.set("generation", fixture.generation.as_str())?;
                    identity.set("frame_id", 1)?;
                    marker.get::<mlua::Function>("draw")?.call::<()>((
                        lua.create_userdata(crate::lua_canvas::Canvas::new(canvas.context()))?,
                        identity,
                    ))
                })();
                draw.map_err(|error| anyhow::anyhow!(error.to_string()))?;
            }
            hardware.present(&canvas.finish()?)?;
            hardware.release()?;
            drop(hardware);
            source.finish()?;
            return Ok(());
        }
        let (writer, level) = capture.into_writer();
        assert_eq!(level, fixture.capture_level());
        let capture = crate::diagnostic_observer::Capture::from_mapped(writer)?;
        capture
            .record(crate::diagnostic_observer::EventKind::MinimalCalibration { cost_ns: [1; 32] });
        capture.finish();
        Ok(())
    }

    #[test]
    fn actual_recipient_rejects_wrong_declaration_process_role_and_capacity() -> Result<()> {
        for (case, expected_error) in [
            ("declaration", "launcher declaration mismatch"),
            ("capacity", "source mapping capacity mismatch"),
            ("storage-role", "expected role"),
            ("envelope-role", "invalid startup descriptor envelope"),
            ("missing", "invalid startup descriptor envelope"),
            ("recipient", "recipient process mismatch"),
            ("launcher", "peer process start mismatch"),
            ("exit", "exit"),
        ] {
            let directory = tempfile::tempdir()?;
            let listener = UnixListener::bind(directory.path().join("broker"))?;
            listener.set_nonblocking(true)?;
            let child = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "diagnostic_provisioning::tests::recipient_child",
                    "--nocapture",
                ])
                .env(CHILD_DIRECTORY, directory.path())
                .env(CHILD_ROLE, "broker")
                .env(CHILD_FAILURE, expected_error)
                .spawn()?;
            let mut child = ChildGuard(child);
            let budget = Budget::new(deadline())?;
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        budget.wait(&listener, libc::POLLIN)?
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let peer = crate::peer_credentials::read(&stream)?;
            if case == "exit" {
                child.wait()?;
                assert!(authenticate_peer(
                    &stream,
                    ProcessIdentity {
                        pid: peer.pid,
                        uid: peer.uid,
                        gid: peer.gid,
                        start_ticks: 1
                    },
                    &budget
                )
                .is_err());
                continue;
            }
            let mut processes = fake_sources();
            processes[1] = ProcessIdentity {
                pid: peer.pid,
                uid: peer.uid,
                gid: peer.gid,
                start_ticks: process_start(peer.pid)?,
            };
            if case == "recipient" {
                processes[1].start_ticks += 1;
            }
            let mut bytes = declaration_bytes(&plan(Mode::C), processes, [64; 3]);
            if case == "launcher" {
                let text = String::from_utf8(bytes)?;
                let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
                let me = current_process()?;
                lines[4] = format!(
                    "launcher {} {} {} {}",
                    me.pid,
                    me.uid,
                    me.gid,
                    me.start_ticks + 1
                );
                bytes = (lines.join("\n") + "\n").into_bytes();
            }
            put(directory.path(), &bytes)?;
            stream.write_all(&[1])?;
            if !matches!(case, "recipient" | "launcher") {
                if case == "declaration" {
                    let position = bytes
                        .windows(12)
                        .position(|window| window == b"generation-1")
                        .unwrap();
                    bytes[position + 11] = b'2';
                }
                write_all_channel(&stream, &bytes, &budget)?;
                if case != "missing" && case != "declaration" {
                    let role = if case == "storage-role" {
                        SourceRole::Worker
                    } else {
                        SourceRole::Broker
                    };
                    let mut collector =
                        Collector::for_role(if case == "capacity" { 65 } else { 64 }, role)?;
                    let fd = collector.take_storage(role)?;
                    send_right(
                        &stream,
                        if case == "envelope-role" {
                            SourceRole::Supervisor
                        } else {
                            SourceRole::Broker
                        },
                        &fd,
                        &budget,
                    )?;
                }
            }
            drop(stream);
            child.wait()?;
        }
        Ok(())
    }

    #[test]
    fn actual_child_handoff_binds_each_role_and_mode_and_preserves_raw_sources() -> Result<()> {
        for mode in [Mode::A, Mode::B, Mode::C] {
            let directory = tempfile::tempdir()?;
            let mut children = Vec::new();
            let mut streams = Vec::new();
            let mut identities = Vec::new();
            for role in ROLES {
                let listener = UnixListener::bind(directory.path().join(role_name(role)))?;
                listener.set_nonblocking(true)?;
                let child = Command::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "diagnostic_provisioning::tests::recipient_child",
                        "--nocapture",
                    ])
                    .env(CHILD_DIRECTORY, directory.path())
                    .env(CHILD_ROLE, role_name(role))
                    .spawn()?;
                children.push(ChildGuard(child));
                let budget = Budget::new(deadline())?;
                let stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            budget.wait(&listener, libc::POLLIN)?
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                let peer = crate::peer_credentials::read(&stream)?;
                identities.push(ProcessIdentity {
                    pid: peer.pid,
                    uid: peer.uid,
                    gid: peer.gid,
                    start_ticks: process_start(peer.pid)?,
                });
                streams.push(stream);
            }
            let fixture = plan(mode);
            put(
                directory.path(),
                &declaration_bytes(&fixture, identities.try_into().unwrap(), [64; 3]),
            )?;
            let mut launcher = Launcher::new(acquire(directory.path())?.unwrap(), deadline())?;
            for (role, mut stream) in ROLES.into_iter().zip(streams) {
                stream.write_all(&[1])?;
                launcher.send(role, &fixture, stream, deadline())?;
            }
            for child in &mut children {
                child.wait()?;
            }
            for role in ROLES {
                let raw = launcher.snapshot(role)?;
                assert!(raw.initialized && raw.metadata_consistent && raw.report.closed);
                assert_eq!(raw.report.lost, 0);
                assert_eq!(raw.report.failed_operations, 0);
                assert_eq!(raw.report.decode_failures, 0);
                assert_eq!(raw.report.clock_failures, 0);
                if role == SourceRole::Broker {
                    use crate::diagnostic_broker::Operation;
                    use crate::diagnostic_observer::EventKind;
                    use crate::diagnostic_timing::{SpanKind, SpanStatus};
                    let records = &raw.report.records;
                    assert!(
                        matches!(&records[0].kind, EventKind::BrokerConfigured { mode: actual, start_ns, rate: 30, causal: false } if *actual == mode && *start_ns == fixture.start_ns)
                    );
                    assert_eq!(records.iter().filter(|r| matches!(r.kind, EventKind::MinimalSpan(ref s) if s.kind == SpanKind::Calibration)).count(), 32);
                    assert_eq!(
                        records
                            .iter()
                            .filter(|r| matches!(r.kind, EventKind::MinimalCalibration { .. }))
                            .count(),
                        1
                    );
                    let spans: Vec<_> = records
                        .iter()
                        .filter_map(|r| match &r.kind {
                            EventKind::MinimalSpan(s) if s.kind == SpanKind::Present => Some(s),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(spans.len(), 1);
                    assert_eq!(spans[0].status, SpanStatus::Succeeded);
                    assert!(spans[0].start_ns <= spans[0].end_ns);
                    let summaries: Vec<_> = records
                        .iter()
                        .filter_map(|r| match &r.kind {
                            EventKind::MinimalSummary(summary) => Some(summary),
                            _ => None,
                        })
                        .collect();
                    assert_eq!(summaries.len(), 1);
                    let summary = summaries[0];
                    assert_eq!(summary.started_spans, summary.completed_spans);
                    assert_eq!(
                        summary.open_spans
                            + summary.failed_spans
                            + summary.lost
                            + summary.after_close
                            + summary.clock_failures,
                        0
                    );
                    assert!(summary.closed_at_ns.is_some());
                    let release = records
                        .iter()
                        .position(|r| {
                            matches!(
                                r.kind,
                                EventKind::BrokerOperation {
                                    operation: Operation::Release,
                                    success: true
                                }
                            )
                        })
                        .unwrap();
                    let closed = records
                        .iter()
                        .position(|r| {
                            matches!(
                                r.kind,
                                EventKind::BrokerOperation {
                                    operation: Operation::SourceClosed,
                                    success: true
                                }
                            )
                        })
                        .unwrap();
                    assert!(release < closed);
                    if mode == Mode::C {
                        assert!(records.iter().any(|r| matches!(&r.kind, EventKind::PresentEntered { marker: Ok(marker), .. } if marker.run_id() == fixture.run.as_bytes() && marker.generation() == fixture.generation.as_bytes() && marker.frame_id() == 1)));
                        let returned = records
                            .iter()
                            .find(|r| {
                                matches!(r.kind, EventKind::PresentReturned { success: true, .. })
                            })
                            .unwrap();
                        assert_eq!(returned.at_ns, spans[0].end_ns);
                    } else {
                        assert!(!records.iter().any(|r| matches!(
                            r.kind,
                            EventKind::PresentEntered { .. } | EventKind::PresentReturned { .. }
                        )));
                    }
                } else {
                    assert_eq!(raw.report.records.len(), 1);
                    assert!(matches!(
                        raw.report.records[0].kind,
                        crate::diagnostic_observer::EventKind::MinimalCalibration {
                            cost_ns: [1, ..]
                        }
                    ));
                }
            }
        }
        Ok(())
    }
}
