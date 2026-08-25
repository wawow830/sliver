# Linux worker process control research for issue #11

> **Status: historical research.** This note records historical research. Its recommendations are not implementation requirements. A later architecture review superseded them. Implementers should follow the reviewed decision in [issue #11](https://github.com/wawow830/sliver/issues/11) and ADR 0003 once committed.

## Scope and starting point

Issue #1 makes Lua a full user-code environment. It explicitly permits the standard library, Lua 5.4 C modules, files, processes, networking, D-Bus, and graphical-session access, while requiring the worker to stay away from broker hardware. It also sets the two-second callback deadline, 500 ms graceful-stop deadline, descendant cleanup, 512 MiB memory limit, and 64-process limit. [R1]

Issue #11 turns those requirements into acceptance criteria. The worker must be disposable. A failed or unresponsive worker must not hold the hardware owner hostage, and a failed cleanup callback must not become another failure mode. [R11]

This research starts from commit `ad3725ab42aa542bc13a2ba3e574290e67b58a88`. The current implementation is still thread-based:

- `LuaWorker` owns a Rust `JoinHandle` and an unbounded `std::sync::mpsc::channel`; staging starts an owner thread rather than a process. [RL1]
- `render_at_with_input`, `drive`, and `shutdown` wait on reply receivers. The 50 ms bound in `render_at_with_input` is consulted only after its reply receiver returns, and covers shared-slot publication retries, not a callback that never returns. `shutdown` has no timeout before joining the owner thread. [RL2] [RL3] [RL4]
- The Lua hook is `HookTriggers::EVERY_LINE` and records a source line. It does not enforce elapsed time. [RL5]
- `FrameSlots` uses an anonymous `MmapMut`, an `Arc`, a `Mutex`, and a `Condvar`. Its three slots are process-local objects. `FrameWriter::drop` returns an abandoned `WRITING` slot to `FREE`, but a process dying cannot run that destructor. [RF1] [RF2] [RF3]
- The supervisor checks thread liveness during hardware polling, but `drive_active_worker` calls the worker synchronously. If the owner thread is inside Lua, C, or a blocking process call, the supervisor cannot reach its next liveness check. [RS1] [RS2]
- Recovery already has the right ownership boundary: `RecoverySession` and the compiled F1-F12 row live outside Lua, and the session records whether its hidden owner is healthy. That boundary should remain. [RR1] [RR2]
- The existing apply socket has a length-prefixed stream and a 1 MiB message cap; the acceptor has a 16-request synchronous queue. The worker command channel is a different, unbounded in-process channel, and the touch queue has no hard capacity. [RI1] [RS3] [RS4]

The historical conclusion was blunt: a watchdog thread around the current `LuaWorker` would still share the process with arbitrary C code and a potentially corrupted Lua runtime. The research therefore recommended a process boundary rather than a more careful `JoinHandle`; a later architecture review superseded that recommendation.

## Facts

### Lua and callback interruption

`mlua` 0.12.0 documents `Lua::unsafe_new` as loading all standard libraries, allowing C modules, and providing no safety guarantees for the created state. That is the intended configuration in this repository, so a loaded module is native code in the worker process, not a sandboxed Lua value. [ML1] [ML2]

The mlua hook API is cooperative. Its documentation says the hook is called periodically as Lua code executes, and that returning an error can implement a limited execution limit based on instruction triggers. Lua 5.4's `debug.sethook` likewise describes line, call, return, and instruction-count events. [ML3] [L1]

Lua's VM calls a C function through `(*f)(L)` after setting up the C call frame. The instruction hook runs in the Lua interpreter loop, not inside the body of that native C function. The Lua source also uses `longjmp` for errors and yields across C boundaries. A hook is useful for cutting off a pure Lua loop, but it is not an interrupt for a C function that is blocked inside `read`, `ioctl`, a library lock, a decoder, or another native call. [L2] [L3] [L4]

Lua's own `os.clock` is CPU time from the underlying C `clock` function, not a wall-clock watchdog. A process waiting in a system call is therefore the wrong place to measure the callback deadline. [L-CLOCK]

On POSIX Linux, a signal can remain pending while a thread blocks it, and a process-directed signal may be delivered to any eligible unblocked thread. A blocked system call may either return `EINTR` or be restarted, depending on the interface and signal disposition. `SIGKILL` and `SIGSTOP` cannot be caught, blocked, or ignored. [M-SIG]

That gives the useful boundary:

| Workload | In-process hook | Reliable external action |
| --- | --- | --- |
| Pure Lua loop | A host-installed instruction hook can usually raise a Lua error at an instruction boundary. It is an optimization, not the authority. [ML3] [L1] | The supervisor kills the worker process if the external deadline expires. [M-SIG] [M-PFD2] |
| Blocking C module | No periodic Lua instruction hook runs inside the C body. [L2] [L3] | Kill the worker process. Do not try to throw a Lua error from a signal handler. [M-SIG] [L2] |
| `os.execute` or `io.popen` | The Lua call is waiting in a C library process operation. [L-OS] [L-IO] | Kill the worker and its cgroup, including the command shell and descendants. [M-SYSTEM] [M-POPEN] [K-CGROUP] |
| No heartbeat | No cooperative code is making progress. [ML3] | The parent-owned timer expires and the parent kills by cgroup and pidfd. [M-PFD1] [K-CGROUP] |

The safe rule is that a Lua error is acceptable only while the worker has returned to the Lua host loop. Once the worker is classified as hung, it receives no cleanup callback. `SIGTERM` can be useful as a soft signal in other contexts, but it is not a 2-second guarantee for arbitrary native code. `SIGKILL` is the hard boundary, and it still leaves reaping and descendant cleanup to the parent. [M-SIG] [M-WAIT] [L2]

### Lua process calls and descendants

Lua 5.4 defines `os.execute` as equivalent to the ISO C `system` function and passes the command to an operating-system shell. The Lua 5.4 POSIX source maps it directly to `system(cmd)`. Linux `system(3)` describes a forked child that executes `/bin/sh -c command` and says `system()` waits for the command to complete. [L-OS] [LSourceOS] [M-SYSTEM]

Lua 5.4's POSIX `io.popen` maps to `popen` and `pclose`. Linux `popen(3)` describes a pipe, a fork, a shell invocation, and a `pclose()` that waits for the associated process. Lua also says that automatically closing a `popen` handle during garbage collection happens at an unpredictable time. [L-IO] [LSourceIO] [M-POPEN]

`system()` blocks `SIGCHLD` and ignores `SIGINT` and `SIGQUIT` in its caller while the command runs. A worker-local SIGCHLD strategy therefore cannot be the supervisor's child-reaping mechanism. A command shell also inherits any worker descriptor that is still open across exec. [M-SYSTEM] [M-FORK] [M-OPEN]

`PR_SET_PDEATHSIG` is cleared for the child of `fork(2)`. Setting it on the worker therefore does not make an `os.execute` shell or an arbitrary native-module child inherit the same protection. Cgroup containment is the mechanism that covers those children. [M-PRDEATH] [M-SYSTEM]

### Process identity, signals, waiting, and reaping

A pidfd is a file descriptor referring to one process. `pidfd_open` sets close-on-exec, its result can be monitored with `poll` or `epoll`, and the descriptor becomes readable when the process exits and becomes a zombie. A child pidfd can be waited on with `waitid`. [M-PFD1]

`pidfd_send_signal` addresses the process referred to by the descriptor rather than a numeric PID. The man page calls out the PID-reuse race avoided by this stable reference. It does not, by itself, recursively signal descendants. [M-PFD2]

`waitid(P_PIDFD, ...)` is available since Linux 5.4. `WNOHANG` can return success without a waitable child, so callers must inspect `siginfo_t.si_pid`; `WNOWAIT` observes a status without consuming it. A terminated child that is not waited for remains a zombie and consumes a process-table slot. [M-WAIT]

Rust documents `Child::kill` as forcing the child process to exit, and as SIGKILL on Unix. `Child` has no `Drop` implementation that waits for the child, so dropping the handle does not provide cleanup. Use the handle for launch bookkeeping, then use pidfd, cgroup, and explicit waits for lifecycle control. [R-CHILD] [M-PFD2] [K-CGROUP]

The supervisor can set `PR_SET_CHILD_SUBREAPER` before creating workers. Orphaned descendants are then reparented to the nearest living subreaper, which receives `SIGCHLD` and can wait on them. The subreaper attribute is not inherited through `fork`, so it must be set on the supervisor itself before the worker tree exists. [M-SUBREAPER]

`PR_SET_PDEATHSIG` sends a chosen signal to the calling process when its parent thread dies. The setting is cleared in a fork child, can be cleared by credential changes, and is cleared when executing a set-user-ID, set-group-ID, or capability-bearing binary. The parent is defined as the creating thread, not necessarily the whole multithreaded process. [M-PRDEATH]

Use `PR_SET_PDEATHSIG(SIGKILL)` as a backup against a supervisor failure, but set it in the worker after final credentials are in place, check `getppid()` against the expected supervisor PID, and retain the cgroup kill path. The check closes the ordinary fork/setup race, while the cgroup is the authoritative containment mechanism if setup is interrupted.

A process group is not a process tree. `kill(-pgid, sig)` targets the members of one process group. A fork child inherits its parent's process-group ID, but a process that is not a group leader can call `setsid()` and become the leader of a new session and process group. Native code can also create other groups. [M-KILL] [M-PG] [M-SID]

A new process group is still worth creating. It is a useful diagnostic and a fallback for a soft signal. It is not sufficient for issue #11 because it does not provide hierarchical resource accounting, does not catch a child that calls `setsid`, and does not solve reaping. [M-KILL] [M-SID]

### Cgroups and systemd

The kernel's cgroup v2 documentation says that cgroups form a hierarchy, every process belongs to one cgroup, and a forked child is born into the forking process's cgroup. The `pids` controller counts kernel task IDs, and `pids.max` limits the number of processes in a cgroup and its descendants. A fork or clone that would violate the policy fails with `EAGAIN`. [K-CGROUP]

`memory.current` includes the cgroup and its descendants. `memory.max` is a hard memory limit, although the kernel documents that usage can temporarily exceed it under some conditions before reclaim or an in-cgroup OOM kill. `memory.oom.group=1` makes the OOM killer treat the cgroup and its descendants as one workload. [K-CGROUP]

Writing `1` to cgroup v2's `cgroup.kill` kills the cgroup and all descendant cgroups with `SIGKILL`. The kernel documentation explicitly says that cgroup-tree killing handles concurrent forks and is protected against migrations. This is the property process groups lack. [K-CGROUP]

The same documentation says that a cgroup's `populated` state includes live processes in its descendants and can be polled, but exited processes remain associated with the cgroup until reaped and zombies do not appear in `cgroup.procs`. A cgroup-empty signal is not a substitute for `waitid`. [K-CGROUP] [M-WAIT]

Cgroup killing does not retroactively capture a process that escaped before the kill. The kernel's delegation rules provide containment when the delegatee can write its own subtree but cannot write the `cgroup.procs` file of the common ancestor outside that subtree. Do not give the worker a parent-cgroup fd or permissions to move processes across that boundary. [K-CGROUP]

systemd maps `MemoryMax=` to `memory.max` and `TasksMax=` to `pids.max`. `TasksMax` counts kernel threads and userspace processes, with each thread counted separately. `Delegate=yes` lets a unit create and manage a private cgroup subhierarchy, while hierarchical limits remain imposed by the parent. [SD-RES] [K-CGROUP]

systemd's `KillMode=control-group` kills all remaining processes in a unit's control group on stop, and its normal stop sequence eventually sends `SIGKILL` after `TimeoutStopSec=` if processes remain. That is useful for service-level cleanup, but the supervisor still needs its own 500 ms timer and direct per-worker cgroup handle. A service-manager stop request must not make the hardware owner wait indefinitely. [SD-KILL]

For the device boundary, systemd's `PrivateDevices=yes` creates a private minimal `/dev` containing API pseudo-devices but no physical devices and sets `DevicePolicy=closed`. `DevicePolicy=closed` permits standard pseudo-devices while denying physical devices unless explicitly allowed. This blocks future device opens; closing inherited descriptors alone does not. [SD-EXEC] [SD-RES]

Supplementary groups are inherited from the parent and preserved across exec. Dropping them requires `CAP_SETGID`; an ordinary per-user supervisor cannot assume it can remove the user's `input` or `video` groups. A privileged launcher or a systemd-managed worker unit must therefore enforce the no-device-group requirement. [M-GROUPS] [M-FORK]

### File descriptors and exec

`fork(2)` copies the parent's open file descriptors into the child, referring to the same open file descriptions. `O_CLOEXEC` sets close-on-exec atomically; the Linux man page says this is essential in multithreaded programs because a separate `fcntl(F_SETFD)` can race with fork plus exec. [M-FORK] [M-OPEN]

`close_range` can close a whole descriptor range in one system call. `CLOSE_RANGE_UNSHARE` first unshares the descriptor table to avoid races with other threads, while `CLOSE_RANGE_CLOEXEC` only marks descriptors for closure at the next exec. Therefore a worker that executes Lua in the post-fork child must use actual close operations before it runs Lua, not only `CLOSE_RANGE_CLOEXEC`. [M-CLOSE]

Rust's `CommandExt::pre_exec` runs after fork and before exec. Rust warns that allocation, environment access, mutex acquisition, and other ordinary operations are not guaranteed safe there because other parent threads may still exist. The hook must contain only a small, audited set of raw setup calls such as `prctl`, `dup2`, `close_range`, and an immediate failure path. [R-CMD]

UNIX domain sockets can pass file descriptors with `SCM_RIGHTS`. `MSG_CMSG_CLOEXEC` makes received descriptors close-on-exec, and `recvmsg(2)` warns that an undersized ancillary-data buffer can truncate an `SCM_RIGHTS` list. The receiver must check control-message truncation and accept exactly the descriptors the protocol expects. [M-UNIX] [M-RECV]

### Control and input transport

An AF_UNIX `SOCK_SEQPACKET` socket is connection-oriented, reliable, ordered, and preserves message boundaries. A `SOCK_STREAM` socket is a byte stream and does not preserve record boundaries. `sendmsg` reports `EMSGSIZE` if an atomic packet is too large. [M-UNIX] [M-SOCKET] [M-SEND]

Nonblocking socket operations that would block normally return `EAGAIN` and can be retried after `poll` or `epoll`. Linux documents that `SO_SNDBUF` and `SO_RCVBUF` are kernel buffer settings, that Linux doubles the requested values for bookkeeping, and that `SO_RCVBUF` has no effect for UNIX domain sockets. A byte count guessed from the requested option is not a complete memory bound. [M-SOCKET7] [M-UNIX]

Rust's `mpsc::channel` has an "infinite buffer" and sends do not block. `sync_channel` is bounded, but sends block after the bound is full. Neither is a cross-process protocol. [R-CHAN] [R-SYNC]

The current length-prefixed stream can be made safe, but every receiver must parse partial reads, reject a length above a fixed maximum, bound the number of outstanding messages, and treat an EOF in the middle of a frame as a protocol failure. `SOCK_SEQPACKET` removes the partial-frame state from this worker-specific protocol, which is why it is the better Linux-only choice here. [RI1] [M-SOCKET]

### Shared memory, seals, and atomics

`memfd_create` returns an anonymous RAM-backed file descriptor that can be resized and memory-mapped. `MFD_CLOEXEC` sets close-on-exec, and `MFD_ALLOW_SEALING` permits file seals. The memfd man page specifically describes passing the descriptor over a UNIX domain socket and then mapping it in the second process. [M-MEMFD]

`F_SEAL_GROW` and `F_SEAL_SHRINK` can freeze the frame-file size. `F_SEAL_WRITE` is not compatible with a later writable worker mapping; the memfd documentation says it requires writable mappings to be removed first. `F_SEAL_FUTURE_WRITE` blocks future writable mappings while keeping existing writable mappings, which is also the wrong ordering if the worker has not mapped the file yet. Leave pixel contents writable and seal only the size and seal set. [M-MEMFD] [M-FCNTL]

`mmap(MAP_SHARED)` makes updates visible to other processes mapping the same region. It does not make a multi-byte frame copy atomic. A shared-memory protocol needs an atomic state word and an ordering rule around publication. [M-MMAP]

Rust's `AtomicU64` is documented as an integer type safely shared between threads and has an 8-byte alignment in the cited standard-library source. Rust's `Release` store makes previous writes visible to an `Acquire` load that reads the value; `Acquire` orders later reads after that store. The Rust documentation promises thread sharing, not a portable cross-process ABI, so the mapped layout must be treated as a Linux-specific `repr(C)` protocol and tested on the target architecture. [R-ATOMIC] [R-ORDER]

Linux's futex documentation describes a 32-bit word in shared memory as the process-shared synchronization primitive. The word must be four-byte aligned, and the non-private futex operations are the ones for inter-process sharing. The same documentation's example uses C11 atomics with a shared mapping. A process-shared futex is available if a producer must sleep for a slot, but a parent event loop and three slots do not need a process-shared `Condvar`. [M-FUTEX]

The current `Arc`, `Mutex`, `Condvar`, and `MmapMut` wrappers must not be copied into a shared mapping. They contain process-local ownership and waiting state. The mapped ABI should contain fixed-width integers, atomics, and pixels only; each process owns its local map wrapper. This follows from the current code's process-local wrappers and the Linux shared-mapping and futex contracts. [RF1] [M-MMAP] [M-FUTEX]

## Historical recommendation for Sliver (superseded)

This section records a historical design recommendation. It is not an implementation requirement and was superseded by a later architecture review.

### Make the worker an exec'ed process

Keep the supervisor as the only hardware owner. Replace the Lua owner thread with a small worker executable or an internal `--lua-worker` mode that is always reached through `execve`. The worker process owns the Lua VM and its single callback loop. The supervisor owns the worker's pidfd, cgroup, IPC endpoints, frame mapping, and generation number.

Use one of these two launch paths:

1. A raw Linux launcher using `clone3` with `CLONE_PIDFD` and `CLONE_INTO_CGROUP`. `CLONE_PIDFD` returns the pidfd at creation, and `CLONE_INTO_CGROUP` places the child in a v2 cgroup at creation. Both flags are documented by `clone(2)`. This avoids the race in "fork, let the child run, then write its PID to the cgroup." [M-CLONE]
2. A systemd-managed per-worker service or scope whose cgroup and limits exist before `ExecStart`. Give the supervisor service `Delegate=yes` if it creates the per-worker subtree itself. Use a system service or another privileged launcher when the worker must run as the active user's UID without that user's hardware groups. [SD-RES] [M-GROUPS]

Do not make `Command::spawn` followed by a best-effort cgroup migration the only containment step. A child can fork before the migration, and cgroup documentation says already-existing descendants are not moved when a process is migrated. [K-CGROUP]

Set `PR_SET_CHILD_SUBREAPER` once in the supervisor before any worker launch. Set `PR_SET_PDEATHSIG(SIGKILL)` in the final worker process, then verify that its parent is still the expected supervisor. If credentials or a set-ID exec clear the setting, the cgroup remains the safety mechanism. Put each worker, including staged candidates and stopping old workers, in its own cgroup. [M-SUBREAPER] [M-PRDEATH] [K-CGROUP]

### Use a bounded packet protocol

Use two AF_UNIX `SOCK_SEQPACKET` connections per worker, one for supervisor-to-worker commands and one for worker-to-supervisor events. A single full-duplex socket is also possible, but separate directions make ownership and backpressure easier to audit. Use `libc` or a small local wrapper because the standard `UnixStream` API is a stream abstraction, while the desired transport is Linux `SOCK_SEQPACKET`. [M-SOCKET] [R-UNIX]

Every packet should have a fixed header containing:

```text
magic = "SLVR"
protocol_version
worker_generation
message_kind
sequence
request_id
payload_length
```

Choose a small maximum packet size, for example 64 KiB, and reject larger packets before allocation. Keep at most one command in flight for staging/render/stop and a fixed number of input packets in the supervisor. Do not use child-provided timestamps for liveness.

The supervisor should coalesce only move events, by contact ID. Down, up, cancel, Fn, modifier, and other non-droppable transitions must not be put into an unbounded `Vec`. If a non-droppable event cannot be sent without exceeding the fixed queue, record an input-transport overflow, kill the worker, and enter recovery. A move that is replaced before it is sent is the only event that may be dropped. Worker key-effect packets should also have a fixed operation count; exceeding it is a worker protocol failure.

Set nonblocking mode and use `EAGAIN` as a state transition, not as permission to grow a queue. The bound is the application queue plus the kernel's finite socket buffers, with the actual configured buffer values recorded in diagnostics. The worker must never block in a heartbeat write while holding a Lua callback boundary open. [M-SOCKET7] [R-CHAN]

Use `SCM_RIGHTS` only for the frame memfd during bootstrap. The control socket is the only worker IPC descriptor. Reject any unexpected file descriptor, `MSG_CTRUNC`, wrong generation, duplicate sequence, or malformed length. Use `MSG_CMSG_CLOEXEC` when receiving the memfd. [M-UNIX] [M-RECV]

A framed `SOCK_STREAM` fallback is acceptable only if Linux `SOCK_SEQPACKET` is unavailable. Its frame header must be read with a state machine, its maximum length must be checked before allocating, and a partial EOF must be treated as worker death. The current apply protocol is a useful parser pattern, but it is not a reason to use an unbounded worker channel. [RI1] [M-SOCKET]

### Define heartbeat as owner-loop progress

The worker must not use a second thread to send heartbeats. A heartbeat thread would continue to report health while the Lua owner thread is blocked in a native callback, which is exactly the false-positive issue #11 must avoid.

Use these worker-to-supervisor events:

- `CALL_BEGIN(request_id, callback_kind)` immediately before the host invokes `start`, `render`, `touch`, `key`, `visibility`, a timer callback, or `stop`.
- `CALL_END(request_id)` only after that callback returns.
- `FRAME_READY(slot, frame_sequence)` after the slot's state changes to `READY`.
- `HEARTBEAT(sequence, last_completed_request_id, state)` only when the owner loop is between callbacks and back in its command/poll loop.
- `STOP_DONE(request_id)` only after the requested `stop` callback returns and the worker has started its final exit path.

The parent accepts a heartbeat only when the generation matches and the sequence is strictly newer than the last accepted sequence. A heartbeat can update the idle progress time, but it can never extend an active callback deadline. `CALL_BEGIN` is telemetry and a deadline boundary, not a claim that the Lua callback has already returned. The worker must send `CALL_BEGIN` with a nonblocking operation before entering the callback; if the packet cannot be queued, it fails the worker instead of entering Lua without an observable boundary.

Use a parent-owned suspend-aware monotonic clock, preferably Linux `CLOCK_BOOTTIME` if the two-second safety interval should include suspend time. Linux documents that `CLOCK_MONOTONIC` excludes suspend while `CLOCK_BOOTTIME` includes it; Rust 1.89 documents that `Instant` does not specify suspend behavior across platforms and versions. [M-CLOCK] [R-INSTANT]

The parent arms a no-progress deadline when it dispatches a command and caps its event-loop wait by the earliest deadline. `CALL_BEGIN` identifies the callback for diagnostics, but the parent does not start the safety clock from a child-provided timestamp. Keep a command that can enter Lua bounded to one callback, or apply the two-second deadline to the whole bounded input batch. This is conservative for callbacks later in a batch, but it cannot allow a callback to run late. A callback that does not return by two seconds causes a hard worker failure. An idle worker that stops producing valid heartbeat sequence numbers for two seconds causes the same hard failure. The parent never waits on a worker reply with an unbounded `recv`.

A valid packet from the old generation after replacement is ignored or treated as a protocol error. The old control endpoint is closed, the old pidfd remains until reaped, and no old event can change the new worker's frame, brightness, recovery ownership, or selected path.

### Stop and failure state machine

Use explicit worker states: `Starting`, `Healthy`, `Stopping`, `Hung`, `Dead`, and `Reaped`.

- `Starting` has a two-second no-progress deadline for load/start/first-frame work. If it crashes, blocks, violates the protocol, or misses that deadline, kill its cgroup and reap it. Do not call `stop` on a candidate that failed staging.
- `Healthy` may receive `STOP(replaced)`, `STOP(logout)`, or `STOP(shutdown)` only for the intentional lifecycle transitions named by issue #1. Send no new input after entering `Stopping`. [R1] [R11]
- `Stopping` has a 500 ms deadline measured by the supervisor. The worker runs `stop(reason)` once. If `STOP_DONE` and process exit arrive in time, reap normally. If the callback errors, the process fails. If the callback hangs, the process is classified `Hung`. In both cases, send no second cleanup callback.
- `Hung` and `Dead` never receive `stop`, contact-cancel callbacks, or any other Lua callback. Write `1` to the worker cgroup's `cgroup.kill`, wait for the pidfd, reap direct and adopted children, reclaim or discard the frame mapping, release synthetic keys in the broker, and enter fixed recovery. [K-CGROUP] [M-PFD1] [M-WAIT]
- After a successful commit, the old worker can be stopped in the background. The new frame and broker ownership must not wait for the old worker's cleanup. If old cleanup misses 500 ms, kill its cgroup and continue with the new worker.
- A healthy worker hidden by the compiled recovery row remains alive and continues to use the same heartbeat contract. If it stops heartbeating while hidden, kill it and mark the recovery owner unhealthy. A failed worker is never restarted automatically. [R1] [RR1]

A pidfd readiness event is not itself a reaping operation. Use `waitid(P_PIDFD, ..., WEXITED|WNOHANG)` for the direct worker, then drain adopted descendants with `waitid` while `si_pid` reports a waitable child. Keep the supervisor as the only process reaper for this tree, and do not set `SIGCHLD` to `SIG_IGN` or install another asynchronous waiter. [M-PFD1] [M-WAIT] [M-SUBREAPER]

### Frame slots across processes

Create one memfd per worker with `MFD_CLOEXEC|MFD_ALLOW_SEALING`, `ftruncate` it to a checked fixed size, initialize the header and three slots, then add `F_SEAL_GROW|F_SEAL_SHRINK|F_SEAL_SEAL`. Map it `MAP_SHARED` in the supervisor and pass the fd to the worker with `SCM_RIGHTS`. Do not pass pointers, Rust `Arc`s, mutexes, condition variables, or mapping addresses. [M-MEMFD] [M-FCNTL] [M-MMAP] [M-UNIX]

Use a fixed layout like this:

```text
header:
    magic, protocol version, width, height, stride, slot_bytes
    worker generation
    frame sequence counter
three slot metadata records:
    atomic state: FREE, WRITING, READY, READING, RECLAIMING
    atomic or state-protected sequence
    presentation time and delta as checked integer nanoseconds
three pixel regions:
    exactly slot_bytes each
```

The worker is the only producer. It claims `FREE`, or atomically replaces an older `READY` slot, with a compare-and-exchange to `WRITING`. It copies all pixels and metadata, then publishes `READY` with a `Release` store. The supervisor loads the state with `Acquire` and reads a slot only after it has claimed `READY` as `READING`. The Release/Acquire pair is the publication barrier; a producer that dies while copying cannot publish a partial slot. [R-ORDER] [M-FUTEX]

The supervisor chooses the highest sequence among `READY` slots, claims it with `READY -> READING`, and copies the pixels before `READING -> FREE`. It may reclaim older `READY` slots after a `READY -> RECLAIMING` compare-and-exchange and a sequence check. A frame published after the initial scan is not touched by that scan; if it remains `READY`, the next pass can select it. A producer racing to reuse a slot loses or wins the same compare-and-exchange, so the broker never reads a slot in `WRITING`. This is the process version of the current slot-selection protocol, without its process-local locks. [RF3] [M-MMAP] [R-ORDER]

When the pidfd says the worker has exited, and only then, scan the mapping. Any `WRITING` slot is abandoned and can be reset to `FREE`; `READY` slots from that generation are discarded for recovery rather than presented as a new live frame. If the supervisor is already copying a `READING` slot, it finishes or abandons the copy without allowing the old generation to affect the new worker. The safest candidate-failure path is to close the old mapping and memfd after reaping, then create a fresh generation.

The generation field matters even though the memfd is per-worker. It prevents a late notification, stale test handle, or accidental mapping reuse from being interpreted as a frame from the current worker. Do not reclaim a `WRITING` slot merely because a heartbeat is late. Kill first, observe pidfd exit, then reclaim. That ordering closes the write-versus-recovery race.

Use the control socket's `FRAME_READY` packet only as a wakeup and metadata hint. The supervisor can rescan the three atomic states after any packet, timer, or hardware poll. This means a lost notification cannot make a complete frame permanently invisible, while a full event socket still becomes a bounded worker transport failure.

### Historical descriptor and device boundary proposal (superseded)

Create every supervisor descriptor with an atomic close-on-exec flag where the syscall supports it. In the child setup, retain only a bootstrap control fd, close all other descriptors with `close_range`, and exec the worker. Clear close-on-exec only on the intentionally retained bootstrap fd. After the worker receives the frame memfd, the received fd must be close-on-exec. No DRM, evdev, uinput, backlight, broker, or pidfd descriptor crosses the boundary. [M-OPEN] [M-CLOSE] [M-RECV]

Descriptor closure is necessary but does not stop a native module from opening `/dev` later. Put the worker in a systemd-managed unit with `PrivateDevices=yes` or an equivalent device policy, and run it with no hardware supplementary groups. Keep the user's home, XDG environment, network, and D-Bus settings outside this device restriction. If the per-user manager cannot provide the required device and cgroup policy, use a root-managed per-worker launcher or fail closed rather than silently running the worker with the supervisor's device groups. [SD-EXEC] [SD-RES] [M-GROUPS]

`NoNewPrivileges` is a useful additional setting because Linux preserves it across fork and exec and it prevents exec from granting privileges that were not already available. It does not revoke already-held descriptors, group membership, or ordinary user permissions, so it is not a replacement for the fd and device steps above. [M-NNP] [M-FORK]

The worker's ordinary file, network, process, D-Bus, and graphical-session operations should go through Lua and the worker's own credentials. The broker should see only normalized packets and completed frames. Native code that opens a file or socket remains part of the worker cgroup and is killed with it. [K-CGROUP] [M-SYSTEM]

### Limits and systemd assumptions

Apply these limits to each worker cgroup, not to the supervisor:

```text
memory.max       = 512M
memory.oom.group = 1
pids.max         = 64
```

`pids.max` is a task limit, so a native module that creates many threads consumes it too. That is stricter than counting only traditional processes and is the useful interpretation for a worker that can load arbitrary C modules. Keep these values in the systemd unit or the delegated cgroup parent so administrator drop-ins can override them, never in Lua or the apply request. [K-CGROUP] [SD-RES]

The minimum advertised Linux baseline should be:

| Facility | Minimum relevant version or condition |
| --- | --- |
| `pidfd_open` | Linux 5.3; `waitid(P_PIDFD)` requires Linux 5.4. [M-PFD1] [M-WAIT] |
| `pidfd_send_signal` | Linux 5.1. [M-PFD2] |
| `clone3` worker launch | `CLONE_PIDFD` is documented since Linux 5.2; `CLONE_INTO_CGROUP` since Linux 5.7. [M-CLONE] |
| `close_range` | Linux 5.9; `CLOSE_RANGE_CLOEXEC` since Linux 5.11. [M-CLOSE] |
| `memfd_create` | Linux 3.17. [M-MEMFD] |
| AF_UNIX `SOCK_SEQPACKET` | Linux 2.6.4; `MSG_CMSG_CLOEXEC` is documented since Linux 2.6.23. [M-UNIX] [M-RECV] |
| cgroup kill | Unified cgroup v2 with `cgroup.kill`; the kernel documentation entry was added for Linux 5.14. The worker cgroup must be a non-threaded cgroup because `cgroup.kill` is not available for threaded cgroups. [K-CGROUP] [K-CGROUP-KILL] |
| cgroup controllers | The unified hierarchy must expose `memory` and `pids`, and the worker subtree must be delegated or owned by the launcher. [K-CGROUP] |
| systemd path | `Delegate=` exists since systemd 218, `TasksMax=` since 227, `MemoryMax=` since 231, and `KillMode=control-group` since 187 in the cited systemd 258 manuals. [SD-RES] [SD-KILL] |

On Fedora Asahi, verify these conditions at startup instead of assuming the distribution or kernel configuration:

```sh
mountpoint -q /sys/fs/cgroup
 grep -qw memory /sys/fs/cgroup/cgroup.controllers
 grep -qw pids /sys/fs/cgroup/cgroup.controllers
 test -e /sys/fs/cgroup/cgroup.kill
 systemd --version
```

The frame layout should map at offset zero and use explicit 4-byte and 8-byte alignment for its atomic fields. It should not assume a 4 KiB page size. If the target's Rust atomic implementation is not suitable for a mapped field, use a small C11-atomic or raw Linux futex wrapper and reject the target at build or startup; do not silently replace the state word with a plain integer. [M-FUTEX] [R-ATOMIC] [M-MMAP]

If any of pidfds, unified cgroup v2 with `cgroup.kill`, or the required device policy is unavailable, the safe result is an installation or startup error. Falling back to a thread, a process-group-only kill, or an unbounded IPC queue would violate issue #11's contract.

## Races that the implementation must test

1. **PID reuse.** Create and rapidly replace workers. Signal and wait through pidfds, never a stored PID. [M-PFD2]
2. **Pidfd acquisition.** Either use `CLONE_PIDFD`, or ensure no SIGCHLD handler or other thread can reap the child before `pidfd_open`. [M-PFD1] [M-CLONE]
3. **Cgroup placement.** Fork a child immediately during worker startup. Verify it is in the worker cgroup before it can create another descendant. [M-CLONE] [K-CGROUP]
4. **Process-group escape.** Have a descendant call `setsid` and double-fork. Verify `cgroup.kill` still removes it and the supervisor reaps it. [M-SID] [K-CGROUP] [M-SUBREAPER]
5. **PDEATHSIG setup.** Kill the supervisor during each worker setup step, including before and after exec. Verify the cgroup and the parent-death fallback leave no worker. [M-PRDEATH] [K-CGROUP]
6. **Blocked callbacks.** Exercise a pure Lua loop, a native blocking function, `os.execute("sleep 60")`, `io.popen`, and a worker that stops sending heartbeats. Verify recovery starts without a stop callback in every hard-failure case. [L-OS] [L-IO] [ML3] [M-SIG]
7. **Graceful stop.** Check all three reasons, a successful stop, a stop callback that loops, a stop callback that errors, and a worker that exits between `STOP` and `STOP_DONE`. Verify the 500 ms boundary and that no second callback is sent. [R1] [R11]
8. **Frame death points.** Kill the worker before slot claim, during pixel copy, after metadata, after `READY`, and while the broker has `READING`. Verify no partial frame is presented and all stale generation slots are reclaimed or discarded. [M-MMAP] [R-ORDER]
9. **Newest-ready races.** Publish frames faster than presentation and publish a newer frame during the broker's selection and reclamation scans. Verify the selected frame is complete and the newer frame is selected next. [RF3]
10. **IPC bounds.** Fill the command socket, output socket, and input queue with moves and non-droppable events. Verify bounded allocation, move coalescing, and recovery on non-droppable overflow. [M-SOCKET7] [R-SYNC]
11. **Descriptor audit.** Inspect worker descriptors before Lua starts, run `os.execute`, load a C module, and attempt hardware opens. Verify no broker/device fd is inherited, no control or frame fd leaks across exec, and device policy rejects later physical opens. [M-FORK] [M-CLOSE] [M-RECV] [SD-EXEC]
12. **Resource limits.** Fork and thread until `pids.max`, allocate until `memory.max`, and verify the entire worker tree is reported as one failure and removed. [K-CGROUP] [SD-RES]

## Primary source index

The citations above use only the following primary sources. The Linux pages were read from Linux man-pages 6.18. The source paths are listed here so a moving documentation mirror does not hide which interface owns each claim.

| Citation IDs | Primary source path and version |
| --- | --- |
| `R1`, `R11` | GitHub issues #1 and #11 in repository `wawow830/sliver`; repo implementation links are pinned to commit `ad3725ab42aa542bc13a2ba3e574290e67b58a88`. |
| `RL*`, `RF*`, `RS*`, `RR*`, `RI*` | Repository paths under `crates/sliverd/src/`, pinned to the commit above. |
| `ML1`-`ML3` | mlua crate 0.12.0, `src/state.rs`, generated API docs for `Lua::unsafe_new` and `Lua::set_hook`. |
| `L1`, `L-OS`, `L-IO` | Lua 5.4 reference manual, `manual.html`, sections `debug.sethook`, `os.execute`, and `io.popen`. |
| `L2`, `L3`, `L4`, `LSourceOS`, `LSourceIO` | Lua 5.4 source tree, `ldo.c`, `lvm.c`, `loslib.c`, `liolib.c`, and the error-handling section of the manual. |
| `M-PFD1`-`M-NNP` | Linux man-pages 6.18, paths `man2/pidfd_open.2`, `pidfd_send_signal.2`, `wait.2`, `clone.2`, `PR_SET_PDEATHSIG.2const`, `PR_SET_CHILD_SUBREAPER.2const`, `kill.2`, `fork.2`, `setpgid.2`, `setsid.2`, `close_range.2`, `open.2`, `socket.2`, `recvmsg.2`, `sendmsg.2`, `memfd_create.2`, `fcntl.2`, `mmap.2`, `futex.2`, `clock_gettime.2`, `getgroups.2`, and `PR_SET_NO_NEW_PRIVS.2const`, plus `man7/signal.7`, `unix.7`, and `socket.7`, and `man3/system.3` and `popen.3`. |
| `K-CGROUP`, `K-CGROUP-V612` | Linux kernel authoritative documentation, `Documentation/admin-guide/cgroup-v2.rst`, current HTML and pinned v6.12 source tree. |
| `K-CGROUP-KILL` | Linux kernel v6.12 source history for `Documentation/admin-guide/cgroup-v2.rst`, commit adding `cgroup.kill`. |
| `SD-RES`, `SD-KILL`, `SD-EXEC` | systemd 258 manuals, `systemd.resource-control`, `systemd.kill`, and `systemd.exec`. |
| `R-CHILD`, `R-CMD`, `R-CHAN`, `R-SYNC`, `R-ORDER`, `R-ATOMIC`, `R-INSTANT`, `R-UNIX` | Rust standard library 1.89.0, `std/process.rs`, `std/os/unix/process.rs`, `std/sync/mpsc`, `std/sync/atomic`, `std/time`, and `std/os/unix/net`. |

[R1]: https://github.com/wawow830/sliver/issues/1
[R11]: https://github.com/wawow830/sliver/issues/11
[RL1]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/lua_worker.rs#L173-L180
[RL2]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/lua_worker.rs#L345-L367
[RL3]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/lua_worker.rs#L467-L483
[RL4]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/lua_worker.rs#L520-L540
[RL5]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/lua_worker.rs#L892-L900
[RF1]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/frame_slots.rs#L108-L137
[RF2]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/frame_slots.rs#L169-L185
[RF3]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/frame_slots.rs#L343-L516
[RS1]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/supervisor.rs#L939-L1000
[RS2]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/supervisor.rs#L1051-L1080
[RS3]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/supervisor.rs#L64-L115
[RS4]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/supervisor.rs#L1316-L1375
[RR1]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/recovery.rs#L50-L77
[RR2]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/recovery.rs#L142-L172
[RI1]: https://github.com/wawow830/sliver/blob/ad3725ab42aa542bc13a2ba3e574290e67b58a88/crates/sliverd/src/apply_ipc.rs#L1-L114

[ML1]: https://docs.rs/mlua/0.12.0/mlua/struct.Lua.html#method.unsafe_new
[ML2]: https://github.com/mlua-rs/mlua/blob/v0.12.0/src/state.rs#L349-L351
[ML3]: https://docs.rs/mlua/0.12.0/mlua/struct.Lua.html#method.set_hook
[L1]: https://www.lua.org/manual/5.4/manual.html#pdf-debug.sethook
[L2]: https://www.lua.org/source/5.4/ldo.c.html#luaD_call
[L3]: https://www.lua.org/source/5.4/lvm.c.html#vmfetch
[L4]: https://www.lua.org/manual/5.4/manual.html#4.4
[L-OS]: https://www.lua.org/manual/5.4/manual.html#pdf-os.execute
[L-IO]: https://www.lua.org/manual/5.4/manual.html#pdf-io.popen
[L-CLOCK]: https://www.lua.org/manual/5.4/manual.html#pdf-os.clock
[LSourceOS]: https://www.lua.org/source/5.4/loslib.c.html#os_execute
[LSourceIO]: https://www.lua.org/source/5.4/liolib.c.html#io_popen

[M-PFD1]: https://man7.org/linux/man-pages/man2/pidfd_open.2.html
[M-PFD2]: https://man7.org/linux/man-pages/man2/pidfd_send_signal.2.html
[M-WAIT]: https://man7.org/linux/man-pages/man2/wait.2.html
[M-CLONE]: https://man7.org/linux/man-pages/man2/clone.2.html
[M-PRDEATH]: https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html
[M-SUBREAPER]: https://man7.org/linux/man-pages/man2/PR_SET_CHILD_SUBREAPER.2const.html
[M-SIG]: https://man7.org/linux/man-pages/man7/signal.7.html
[M-KILL]: https://man7.org/linux/man-pages/man2/kill.2.html
[M-FORK]: https://man7.org/linux/man-pages/man2/fork.2.html
[M-PG]: https://man7.org/linux/man-pages/man2/setpgid.2.html
[M-SID]: https://man7.org/linux/man-pages/man2/setsid.2.html
[M-CLOSE]: https://man7.org/linux/man-pages/man2/close_range.2.html
[M-OPEN]: https://man7.org/linux/man-pages/man2/open.2.html
[M-UNIX]: https://man7.org/linux/man-pages/man7/unix.7.html
[M-SOCKET]: https://man7.org/linux/man-pages/man2/socket.2.html
[M-SOCKET7]: https://man7.org/linux/man-pages/man7/socket.7.html
[M-SEND]: https://man7.org/linux/man-pages/man2/sendmsg.2.html
[M-RECV]: https://man7.org/linux/man-pages/man2/recvmsg.2.html
[M-MEMFD]: https://man7.org/linux/man-pages/man2/memfd_create.2.html
[M-FCNTL]: https://man7.org/linux/man-pages/man2/fcntl.2.html
[M-MMAP]: https://man7.org/linux/man-pages/man2/mmap.2.html
[M-FUTEX]: https://man7.org/linux/man-pages/man2/futex.2.html
[M-CLOCK]: https://man7.org/linux/man-pages/man2/clock_gettime.2.html
[M-SYSTEM]: https://man7.org/linux/man-pages/man3/system.3.html
[M-POPEN]: https://man7.org/linux/man-pages/man3/popen.3.html
[M-GROUPS]: https://man7.org/linux/man-pages/man2/getgroups.2.html
[M-NNP]: https://man7.org/linux/man-pages/man2/PR_SET_NO_NEW_PRIVS.2const.html

[K-CGROUP]: https://docs.kernel.org/admin-guide/cgroup-v2.html
[K-CGROUP-V612]: https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/Documentation/admin-guide/cgroup-v2.rst?h=v6.12
[K-CGROUP-KILL]: https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/commit/Documentation/admin-guide/cgroup-v2.rst?h=v6.12&id=340272b04036f2b833a7094eca5c15e5ed8e184c
[SD-RES]: https://www.freedesktop.org/software/systemd/man/258/systemd.resource-control.html
[SD-KILL]: https://www.freedesktop.org/software/systemd/man/258/systemd.kill.html
[SD-EXEC]: https://www.freedesktop.org/software/systemd/man/258/systemd.exec.html

[R-CHILD]: https://doc.rust-lang.org/1.89.0/std/process/struct.Child.html
[R-CMD]: https://doc.rust-lang.org/1.89.0/std/os/unix/process/trait.CommandExt.html
[R-CHAN]: https://doc.rust-lang.org/1.89.0/std/sync/mpsc/fn.channel.html
[R-SYNC]: https://doc.rust-lang.org/1.89.0/std/sync/mpsc/fn.sync_channel.html
[R-ORDER]: https://doc.rust-lang.org/1.89.0/std/sync/atomic/enum.Ordering.html
[R-ATOMIC]: https://doc.rust-lang.org/1.89.0/std/sync/atomic/struct.AtomicU64.html
[R-INSTANT]: https://doc.rust-lang.org/1.89.0/std/time/struct.Instant.html
[R-UNIX]: https://doc.rust-lang.org/1.89.0/std/os/unix/net/struct.UnixStream.html
