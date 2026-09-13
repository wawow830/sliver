# Private trusted startup provisioning

`crates/sliverd/src/diagnostic_provisioning.rs` implements another software slice
of #23 under [approved P1](native-performance-policy-approval.md). It prepares
role-bound broker/supervisor/worker recording capabilities. **It is not installed
service integration, complete build provenance, native evidence, or a release
pass.** The only startup-related change outside the new module is its private
`lib.rs` declaration. Ordinary services do not call it; no file, environment
variable, public CLI option, Lua option, or broker payload activates capture in
this slice.

## Interface and ownership

The interface has two sides:

- `Declaration::acquire_startup(deadline)` reads the fixed private candidate
  location `/run/sliver-diagnostic/startup`, returning `None` for absent
  provisioning. Existing unsafe or malformed provisioning is an error, not Off.
- `Declaration::acquire_fd(OwnedFd, deadline)` accepts an explicitly delivered,
  owned declaration-file capability through private Rust code. There is no
  numeric inherited-FD convention or environment lookup. This does not implement
  that initial FD delivery mechanism.
- The declared root process constructs `Launcher::new(declaration, deadline)`.
  It preallocates exactly three role-aware collectors. `send(role, exact_plan,
  connected_stream, deadline)` authenticates the recipient and attempts that
  role's ticket once. Failed attempts also consume the attempt; no retry or
  replacement silently overwrites a source. `binding`, `plan`, and `snapshot`
  retain the immutable declaration separately from writable raw storage.
- A recipient consumes its independently acquired declaration with
  `receive(role, exact_plan, connected_stream, deadline)`. Success returns
  `StartupCapture`, exposing read-only `binding()` and `plan()`. Its consuming
  `into_writer()` returns `(MappedWriter, CaptureLevel)` for the role-specific
  adapter. A/B select minimal recording; C selects detailed recording. The
  adapter must still install and calibrate minimal timing before workload work.

The channel must already be connected through future private launcher/startup
plumbing. This module does not bind/listen/connect a production socket or launch
processes. It does not replace the existing worker control/bootstrap protocol.
The caller owns startup sequencing, error retention, process cleanup and source
closure. Dropping a registry neither stops a process nor invents closure.

The run-level storage limit is **65,536 total records across all three sources**,
not that many records per source: at most 33,555,968 mapped bytes, including three
512-byte headers. Each source has at least one record. These are resource bounds,
not a claim that any particular capacity suffices for a complete P1 case.
Overflow remains failure. The trusted launcher must own one registry per attempt;
this is not a global persistent replay database. Reacquiring the declaration or
constructing multiple registries is not prevented across independent trusted
launcher instances. The file is not unlinked or modified by acquisition.

## Conservative file acquisition

Production authority is always UID 0. The path walk starts from a held `/` FD;
every directory is opened relative to its predecessor with `O_NOFOLLOW`,
`O_DIRECTORY`, and `O_CLOEXEC`. Each must be root-owned, without group/other write
or special mode bits. Sticky writable directories are not an exception. Thus
`/run`, the private directory, and the leaf cannot be redirected through an
untrusted path component. Only missing components yield Off; a symlink, including
a dangling leaf symlink, is rejected.

The leaf is initially pinned with `O_PATH | O_NOFOLLOW | O_CLOEXEC`. Its type is
checked **before opening it for I/O**, so even an unsafe FIFO/device entry cannot
block on a FIFO writer or invoke a device open operation. Only a validated
regular inode is reopened through the kernel's `/proc/self/fd/<held-fd>` link.
That deliberate kernel-FD reopen is not following a caller-selected pathname
symlink; the pinned and reopened metadata must match.

The held declaration must be root-owned, regular, singly linked, non-executable,
without special bits or group/other write permission, and 1–4,096 bytes long.
The readable descriptor must be read-only, not `O_PATH`. Bounded `pread` reads
from offset zero without changing a shared file offset. Device/inode, ownership,
mode, link count, size, mtime and ctime are checked around acquisition. The
explicit FD entry performs the same held-file validation, but an FD has no path
ancestry or no-follow history to authenticate. Neither route claims resistance
to malicious root modifying the inode or its metadata. Root is the trust anchor.

No directories, files, permissions, system units, or installed services are
created or changed by production acquisition.

## Exact declaration and process binding

The private version-1 declaration is bounded UTF-8 with exactly eight lines,
single spaces, canonical decimal integers and one final newline:

```text
SLIVER-PROVISIONING-1
policy cbe2b5da809eed7cc4b3406b222baf43e1e34a2d
boot <current-host-boot-uuid>
plan <run> <generation> <T-ns> <30-or-60> <A-or-B-or-C> <causal-0-or-1>
launcher <pid> <uid> <gid> <proc-start-ticks>
worker <pid> <uid> <gid> <proc-start-ticks> <record-capacity>
broker <pid> <uid> <gid> <proc-start-ticks> <record-capacity>
supervisor <pid> <uid> <gid> <proc-start-ticks> <record-capacity>
```

The placeholders are explanatory, not a provisioned file or an installed public
configuration format. No generator/operator workflow is installed here.

The policy reference is the immutable approved candidate revision, not a hash
verification of the installed policy/fixture/observer/build. The boot UUID must
match the current host. Existing `FixtureBootstrap`/`Plan` validation supplies
identity bounds, permitted rates/modes, causal-rate restrictions, integer-range
checks and fixed S/E derivation. Equality checks include **every** plan field:
run, generation, T, rate, mode and causal flag. The epoch must still be more than
five seconds and at most ten minutes ahead at authorization/receipt. This is a
conservative startup admission limit, not shortened P1 warmup or proof warmup
has occurred. Warmup closure remains the fixture/coordinator's responsibility.

The launcher and all three source PIDs must be distinct. Declared source IDs are
not accepted merely because they appear in a root-owned file:

1. The launcher checks its own actual PID/effective UID/GID and `/proc` process
   start ticks; the recipient checks its own identity against its requested role.
2. Both sides check the connected peer's kernel `SO_PEERCRED` PID/UID/GID.
3. Both acquire `SO_PEERPIDFD` for the **actual socket peer**, check it is alive,
   compare its `/proc/<pid>/stat` start ticks, then check liveness again. Unlike
   opening a pidfd by a stale credential PID, this cannot bind a new process
   which reused an exited socket peer's PID.
4. The recipient compares the complete launcher declaration bytes with its own
   validated snapshot before accepting any storage FD. The one-byte rights
   envelope must name its exact role; role-aware raw validation checks the
   mapping version, type, seals, one-writer claim, layout and role. Capacity must
   also match the immutable declaration.

**`SO_PEERPIDFD` is required** (Linux support began in 6.5). Unsupported kernels
fail closed; there is no PID-only or ignored-authorization fallback. Tests use
the real kernel credential/pidfd checks. PID namespaces and procfs must describe
the same local host processes; no cross-namespace translation is implemented.

These checks bind a connection and ticket to a declared process lifetime. They
do not detect exec within that lifetime, attest executable bytes, enforce service
cgroups, establish the user's active local session, or prevent trusted processes
from deliberately delegating a descriptor. Root and the reviewed local launcher
and participating processes remain trusted. In particular this does **not** add
a cross-UID `/proc/<pid>/exe` check or weaken [ADR0004](../adr/0004-broker-and-user-supervisor-handoff.md).
The broker must retain its existing peer/service-cgroup/session and per-request
hardware ownership checks independently of diagnostic authorization.

## Bounded transport and failure

A deadline is mandatory and clamped to two seconds on acquisition. The resulting
declaration retains it; later launcher/recipient stages can shorten but cannot
renew it. All three launcher sends share that declaration budget. File reads,
process-stat reads, declaration storage and ancillary buffers have fixed bounds;
nonblocking socket I/O and `poll` use the same remaining deadline, including
partial declaration reads/writes. No additional worker startup/graceful-stop
allowance or autonomous producer is introduced.

This bounds user-space allocation, retry and wait behavior, not kernel stalls:
regular-file/procfs operations and mapping prefault syscalls are checked around
execution but cannot be preempted by this function. Deployment must use trusted
local `/run`/procfs and preserve the existing outer process watchdog; this is not
a hard real-time filesystem guarantee.

The wire exchange is the exact declaration bytes followed by one role byte with
one `SCM_RIGHTS` FD. Both declaration and FD reads use `recvmsg`, rejecting even
ancillary rights at an unexpected declaration position. Rights are received with
`MSG_CMSG_CLOEXEC`; missing, extra, unsupported and truncated ancillary data are
errors. Both `SCM_RIGHTS` and unexpected `SCM_PIDFD` descriptors are owned before
validation; the latter remains invalid but must close on rejection even when
`SO_PASSPIDFD` is enabled. Linux's ancillary value 4 is defined locally because
libc 0.2.189 does not export `SCM_PIDFD`. The kernel closes excess truncated rights. No writable mapping header grants
process, role, policy or build authority. Successful FD send is not a recipient
acknowledgement or evidence of calibration/recording/closure; collector snapshots
and future lifecycle accounting must establish those separately.

## Verification and remaining work

Focused command:

```sh
cargo test -p sliverd --lib diagnostic_provisioning::tests -- --test-threads=1
```

All 16 tests pass without root, installed services, hardware or ignored tests.
They cover absent versus malformed provisioning; directory/leaf links, hardlinks,
unsafe modes, wrong owner, FIFO/socket/directory leaves, size and descriptor-access
bounds; strict policy/boot/plan/process declarations and total storage bounds;
every plan field and epoch checks; real peer PID/UID/GID/start-time authorization;
partial-read and nonrenewable deadlines; single-use failed tickets; missing,
extra, truncated, unexpected and wrong-role ancillary data. Pipe EOF checks prove
rejected descriptor duplicates close, without racy global FD-count assertions.
An isolated test child additionally enables real `SO_PASSPIDFD`, proves that its
unexpected pidfd is owned and closed, and checks repeated rejection for FD leaks.
The regression first failed with an unowned installed pidfd, then passed after
adopt-and-reject handling; the red log is retained with the local verification
artifacts as `provisioning-scm-pidfd-red.log`, with the passing full focused suite
in `provisioning-green.log` (verification directory
`/home/wawow/sliver-release-verification/p1-native-sources.Q0z0d8/`).

Actual disposable test-executable children exercise all three roles in A/B/C,
retaining independent mapped records through process exit. The authenticated
broker recipient feeds `StartupCapture` directly to `BrokerCapture::from_startup`
and wraps fake hardware: claim, a complete-frame present, release, producer drop,
then explicit source finish. Parent snapshots check actual calibration, all-mode
successful minimal present timing and summary closure, C-only decoded plan/frame
identity and the exact shared adapter-return timestamp, and release before source
closure. Further real-child
cases reject changed declarations, recipient/launcher start times, mapping
capacities, storage roles, envelope roles, missing rights and an exited peer.
A `cfg(test)`-only authority constructor uses the unprivileged temporary-directory
owner as the trust anchor; production constructors always require root. No
production credential check is skipped. Worker/supervisor children record
explicitly artificial calibration-shaped test records; the broker samples a fake
adapter operation. Neither is measured native P1 work or overhead evidence.

Still required: trusted installed launcher/channel/initial-FD delivery, suspended
startup/worker staging and complete broker/supervisor lifecycle integration;
independent service-cgroup/session, executable/RPM and policy/fixture/observer
hash verification; frame-mapping binding; retained failed-preflight ledgers;
source-complete normalized input/edge/cohort reconciliation and cleanup; the
complete native A/B/C/C/B/A overhead battery; versioned native artifacts and
release-verifier gates. The existing [worker transport](native-performance-worker-transport.md),
[fixture](native-performance-fixture.md) and [coordinator](native-performance-coordinator.md)
remain separate prerequisites, not completed installed integration. Exact-commit
RPM verification and a fresh authorized complete #17/#24 hardware/rollback
transaction remain necessary. Approved budgets and old failed ledgers are unchanged.
