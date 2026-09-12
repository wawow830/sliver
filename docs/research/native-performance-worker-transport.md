# Private worker observation transport

This extends the [private observer](native-performance-rust-observer.md) under
[approved P1](native-performance-policy-approval.md). It is **raw software
collection, not authenticated native evidence or release acceptance**. The
installed service startup path still does not provision capture.

## Actual process seam

`diagnostic_capture_transport.rs` owns bounded mapped storage, explicit record
encoding and descriptor transfer. `ProcessWorker` test staging provisions one
worker-only source. The real worker receives that storage through its existing
private control socket **before `Runtime::load`**, so source evaluation and
startup observations do not disappear.

A single ancillary-bearing bootstrap byte precedes the existing framed source
packet. Ordinary staging sends Off; requested test capture selects detailed-plus-
minimal or minimal-only recording and sends exactly one storage FD with
`SCM_RIGHTS`. The actual bootstrap level is retained as raw tag26. This works after either launch mechanism's existing
connection: arbitrary FDs do not have to survive `systemd-run` or test-child
inherited-descriptor cleanup. No numeric descriptor, environment variable, Lua
option or broker identity field enables observation.

The receiver uses `MSG_CMSG_CLOEXEC`, rejects missing/extra/truncated/unsupported
ancillary data, and closes received descriptors on failure. The one-byte
preamble cannot partially transfer a second copy of the rights. Preamble,
bootstrap packet and READY share the parent's existing two-second request
budget; collection adds no new startup or graceful-stop allowance. The worker
waits under the parent's watchdog and socket-EOF ownership, not a second timer
started at HELLO: systemd cgroup discovery can precede the parent's request.
Source loading remains inside the parent-enforced startup deadline.

The private protocol changes on both ends together. This is not compatibility
with an older worker binary; packaging must build and deploy matched binaries.

## Storage and encoding

One `Collector` supplies one consumable worker ticket. Storage is bounded to
1–65,536 records of 512 encoded bytes plus a 512-byte header (at most
33,554,944 bytes per source). Total sources/run memory must additionally be
bounded by the future launcher; arbitrarily constructing collectors is not a
run-level bound.

- A memfd is created and size-sealed against growth/shrink and later seal changes.
  The receiver checks type, write access, seals, role, version and exact size.
- Backing storage is allocated/touched before recording. The receiver also
  prefaults its own writable mapping after exclusive claim, using atomic no-op
  writes, not plain writes racing a collector snapshot.
- All mapped accesses use aligned atomic words. The writer appends fixed,
  explicitly encoded records and publishes only completed records. Source-local
  sequence, loss/error metadata and bounded revision checks describe snapshots.
  No Rust `Vec`, pointer, mutex or enum representation crosses the mapping.
- The record codec has explicit tags, little-endian fields, bounded lengths and
  zero padding; floating scheduler values preserve their raw IEEE bits. The
  shared atomic mapping is a same-host transport, not a portable artifact file.
  Exported marker packets are independently revalidated before typed access.
- Mapped capture does not retain a second full heap record vector. There is no
  per-frame disk write, `msync`, wait for consumer drain, or large shutdown flush.
  Snapshot decoding allocates a bounded caller-owned vector and is not free.

Seals, the one-writer claim, CRCs and a consistent header do **not** authenticate
a source. The source can write its own storage. Immutable declarations and
actual process/build/role binding belong to the trusted launcher and collector
registry, not this writable header.

## Errors, prefix recovery and closure

A killed worker leaves its committed prefix available in the collector. An
interrupted metadata update is marked inconsistent; neither it nor an
uninitialized source can report clean storage closure. No collector infers
semantic success from a prefix or a process exit.

Worker load, control-loop and command errors have fixed raw events. Any helper
failure, including setup before the loop or READY transmission, also records
`WorkerRunFailed` before closure. Existing render/touch/timer failures retain
their more specific observations too.
`failed_operations` counts **failure observations**, not unique failed work;
duplicate observations must not be mistaken for separate attempts. Failed or
abandoned minimal spans contribute failure observations; repeated timing
summaries do not recount earlier failures. Minimal-clock failures remain in the
timing summary and must be checked alongside raw-record clock failures.
Detailed error strings still use the ordinary error reply/journal path. Fixture
failure and failed-resolution observations also increment the failure counter,
without granting source authority or unique-work credit. Fixed tags27/32 preserve
[calibration and fixture clock domains](native-performance-fixture.md); neither is
an evaluator-shaped evidence conversion.

Normal finalization occurs after the runtime and its producers are dropped:
minimal timing emits its fixed summary, then raw capture closes metadata.
`Capture::finish()` does not clone or serialize a report for shutdown. The
existing 500 ms graceful stop and separate termination/reaping behavior remain
unchanged. Missing timing summaries, any loss, open/abandoned work, clock or
operation failures and failed cleanup remain failures/incompleteness.

Only the creator/owner of a shared frame mapping reports remaining READY
publications as `MappingClosed`, after the producer is stopped/joined. An opener
unmapping is not global disposal: the owner may still consume those frames.
Mapping-local sequence numbers still need an independently bound mapping/source
identity before cross-worker assembly.

## Tests and limits

Focused real-child tests transfer an actual sealed memfd, execute real Lua and
canvas rendering, compare exported publication markers with independently
decoded fake-hardware entry pixels, and reconcile fake normalized touch delivery
with token-bearing pixels. They also retain actual callback timing samples and
summary metadata across process exit.

Negative tests cover the real two-second render watchdog, source load/control
loop/stop/render errors, loss from bounded capacity, missing/extra/truncated
rights, bad descriptor properties, repeated writer tickets, malformed encoded
records, expired deadlines, delayed parent startup with observation off,
pre-loop setup failure, receiver prefaulting and post-close writes. Separate
mapping tests demonstrate opener-versus-owner teardown semantics. These tests
use direct disposable child processes, not installed services or hardware.

This does **not** yet establish broker/supervisor service provisioning, three
independent observed service processes, full timed-fixture/coordinator integration,
native A/B/C overhead comparisons, trusted installed provenance, complete
edge/cohort assembly, or a release artifact schema/verifier. The separate
[fixture slice](native-performance-fixture.md) implements reviewed source binding,
actual token mutation and synthetic in-process phase tests, not native acceptance. Raw records
must not be padded with guessed fields to fit the offline evaluator. Legacy
ledgers and their old numeric gates remain unchanged until coherent integration.
