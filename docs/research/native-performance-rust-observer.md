# Private Rust observer slice

Implemented under [approved P1](native-performance-policy-approval.md), at the
existing real Lua/canvas and fake broker-hardware seams. This is **not yet a
production native collector**. It now includes bounded raw export from an
actual worker process through private bootstrap. It still has no service arming,
release-pass flag, public command or Lua interface addition.

## Interface and default behaviour

`crates/sliverd/src/diagnostic_observer.rs` is a private module. Its
`Capture::new(capacity)` preallocates 1–65,536 fixed-size records; its injected
`present` and `poll` operations observe the complete hardware adapter operation
and normalized input receipt. `close()` returns a typed in-memory report.
No environment variable, installed service configuration or broker payload
field enables it. Existing production constructors leave observers **off**;
only private test staging provisions a capture today. The real worker accepts
its explicitly transferred storage before loading Lua; ordinary supervisor
staging sends observation-off. See [worker transport](native-performance-worker-transport.md).

Records use actual integer host `CLOCK_MONOTONIC` nanoseconds, not intended Lua
time or `TouchEvent.time`. Normalized events returned in one poll batch share
the observed batch-return timestamp and retain order. Present success means
the complete adapter operation returned successfully; it is neither optical
presentation nor DMA retirement. Repeated pixel identities remain repeated
raw observations, not fresh-frame credit.

The recorder observes:

- render identity allocation before attempting the real canvas, and outcome;
- independently decoded marker pixels after rendering and at publication;
- shared-slot publication sequence, selection and producer/consumer reclamation;
- pending-frame replacement and remaining ready/pending frames at teardown;
- marker identity at entry to complete hardware `present`, and success/error;
- normalized input receipt and touch callback entry/outcome;
- timer registration, activation, cancellation (including no-op attempts),
  dispatch, callback outcome and registry advancement, plus redraw coalescing;
- raw worker load, control-loop and command failure observations.

Timer values ending in `_seconds` preserve actual scheduler `f64` values,
including rounding/overflow. They are not host timestamps, exact rational
periods or P1 opportunity credit. Timer IDs are local to one worker registry.
An error outcome and a rescheduled/cancelled registry disposition are separate
facts; rescheduling does not promise the failed worker will dispatch again.

The digital decoder independently reads every 2×2 black/white cell from a
complete owned logical 2008×60 frame and validates the complete
[marker packet](native-performance-marker-format.md), padding, identities and
CRC. It does not trust a producer-supplied label or read a concurrently writable
shared slot. CRC is integrity checking, not provenance authentication.

## Bounds and closure

The report includes record size/capacity, attempted record sequence, loss,
clock failures, failed operations, failed decode observations, attempted writes
after closure, and storage closure time. Overflow does not block the producer
waiting for a drain or silently discard its loss count. No per-frame file I/O
or unbounded record-vector growth occurs. Snapshot copies/decoding and observer
locking still cost time and memory; no overhead acceptance is inferred.

Clock metadata includes `clock_getres` and 32 raw consecutive-clock-read
intervals sampled before recording. These include timestamp-placement/loop
cost and are diagnostic samples, not optical calibration or an amount to
subtract from measured latency.

Sources must stop/join before `close()`. A repeated report can expose attempted
writes after the earlier closure. **Storage closure is not semantic cohort
closure, cleanup success, proof that every operation resolved, or permission
to grant acceptance.** Reports are not serialized as the Python evaluator's
input; a future assembler must reconcile actual source records and refuse
missing fields rather than invent them.

## Minimal timing

`diagnostic_timing.rs` retains bounded raw spans independently of detailed
observation: the complete Lua render callback, the supplied adapter's complete
`present`, and the supervisor's active drive from before dispatch through
snapshot selection, broker reply and effect processing. Callback timing excludes
canvas allocation, invalidation and snapshot/marker decoding; these remain
inside the complete drive. The final minimal-record append itself is outside
its own measured span. No duration subtracts estimated overhead.

Nested spans have distinct allocation IDs and completion-record sequences.
Errors, abandoned/open spans, overflow and post-closure activity remain visible.
Startup/restoration/shutdown calls are retained, not silently credited as
measured fixture work. At a supervisor `BrokerClient`, a present span includes
IPC; it is **not** the real M2 adapter duration. Only broker-side instrumentation
can supply that native measurement.

Heap capture returns bounded samples. Mapped worker capture exports every
completed callback span immediately and emits a fixed timing summary after
runtime teardown, before raw source closure; there is no second sample vector
or shutdown sample flush. Missing summaries, raw-source loss and timing-summary
errors remain disqualifying/incomplete, never inferred success. Requested
worker capture now selects detailed-plus-minimal or minimal-only recording via
private bootstrap; ordinary staging stays off. Native A/B/C provisioning and the
complete overhead battery remain unfinished. Explicit complete-sampler
[calibration](native-performance-fixture.md) retains 32 no-op probes and checks
authoritative raw-source health, including aggregate-record loss.

## Tests and exact scope

The original six regressions in `lua_integration_tests.rs` cross real Lua/canvas and fake
hardware. They exercise input-to-pixel identity, repeated presentation, complete
present errors, render/decode failures, three-slot reclaim/selection reasons,
pending replacement/teardown, capacity loss and post-close writes. Both the
anonymous mapping and the production shared-map **algorithm** are exercised
through the owner-thread adapter. Additional regressions cover real timers,
rounding/overflow, callback and complete-drive timing, failure/closure and
mapping teardown. Separate real-child tests now exercise transferred storage,
Lua/canvas/input and fake presentation; see the transport record for exact scope.
The existing separate software headroom/drop benchmark is unchanged.

`scripts/native-performance-observer-smoke.lua` is an uninstalled bounded
smoke fixture using the existing Lua marker encoder. It has no periodic timer,
predeclared epoch, warmup/stop scheduler or native run controls. It is deliberately
**not the P1 timed measurement fixture** and cannot replace the canonical
hardware workload or supply overhead samples.

## Work still required

- Trusted local launcher and per-role broker/supervisor/worker provisioning,
  maintaining existing privilege and private IPC rules.
- Production provisioning of the tested timer/callback/active-drive observations
  in all A/B/C modes and the tested broker adapter wrapper.
- Full process/coordinator integration of the [timed fixture and authoritative
  token mutation](native-performance-fixture.md), whose warmup, stop, causal drain
  and clock-domain behavior is tested through real Lua/canvas with fake hardware.
- Source identity/sequence/loss/work closure, lifecycle exclusions, installed
  RPM/fixture/observer provenance and safe artifact export/assembly.
- Coherent native release schema/verifier integration, instrumentation overhead
  comparison and fresh authorized full hardware/RPM/rollback verification.

These are remaining implementation work under #23, not missing policy approval.
