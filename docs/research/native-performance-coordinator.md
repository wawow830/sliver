# Private lockstep P1 process coordinator

This implements a further software slice under the
[immutable P1 approval](native-performance-policy-approval.md). It is **not an
installed collector, native evidence, an overhead battery or release acceptance**.
Normal service staging remains unprovisioned. No public command, Lua surface,
broker identity payload or kernel change enables this path.

## Exact all-mode frame correlation

`frame_slots.rs` carries optional `FrameAllocation { id, token, allocated_ns }`
with the complete frame under the same slot ownership protocol as pixels and
ordinary timing. Allocation is sampled before the actual fixture render attempt;
publication sequence is allocated only after obtaining a writable slot. The
consumer receives both as `FrameCorrelation`, including in markerless A. Neither
number is inferred from successful hardware calls.

The same-host shared layout is now `SLVRFRM2`, with 32 additional fixed bytes per
slot. Old layouts, unknown tags, absent-but-nonzero allocation payloads, zero or
out-of-range IDs/timestamps, out-of-range tokens and publication sequence
exhaustion are rejected. Allocation/token/time are bounded to the Lua integer
range; token zero denotes no input token. Sequence snapshots used during slot
selection/reclaim are aligned atomic accesses; the selected payload and its
correlation are read only after acquiring READING ownership. Ordinary publication
writes the absent tag and clears the complete metadata area on reuse.

`LogicalFrame::from_completed` and cloning preserve correlation.
`LogicalFrame::from_wire` deliberately does not: **no allocation identity is sent
in the broker protocol**. C must still independently decode complete pixels at
the broker seam. Same-host metadata is not authentication of a worker, mapping,
installed build, or native update.

Pending frames retain original allocation metadata through retries. Detailed
publication and pending-discard records carry that allocation, while selection
and mapped discard records join through mapping-local publication sequence.
The raw storage is now version 3; tags 3 and 6 retain version 2's explicit optional
allocation. Version 3 adds [role-bound broker recording](native-performance-broker-source.md). This requires matched private binaries, not compatibility with old
raw captures. Record size remains 512 bytes.

## Coordinator interface and scope

`diagnostic_coordinator.rs::Coordinator` takes an already staged process worker,
its exact plan and a fresh minimal sampler. `ProcessWorker` retains the immutable
plan actually sent at bootstrap; construction rejects any differing run,
generation, epoch, rate, mode or causal flag before work. This prevents accidental
same-process plan mismatch, not malicious-source substitution or installed-build
attestation.

The coordinator uses the existing worker render/commit/drive/control/watchdog
path and returns its existing next deadline. It installs **no autonomous producer
or new runtime scheduler**. Its narrow supported execution is one serialized
complete frame and acknowledged resolution at a time:

- all modes require allocation and publication sequences to advance exactly;
  gaps, replays, missing metadata or impossible timestamps terminate the attempt;
- C compares run/generation/frame/token decoded independently from the stable
  complete pixels with the plan and allocation; B does not decode;
- receipt batches are bounded to 64. Callers must reconcile original source
  order before forwarding. A receipt older than **or equal to** the last resolved
  endpoint is rejected: this path lacks a cross-source total-order key and must
  not use an equal clock reading to invent ordering;
- five seconds of actual warmup must elapse, every known allocation must resolve,
  and the child must acknowledge warmup closure strictly before T;
- S and E retain their approved 60-second measurement and two-second drain
  offsets. Child fixture admission/retirement rules govern actual allocations;
  Finish requires E and semantic child closure before ordinary bounded shutdown;
- records are preallocated and bounded to 8,192 frame entries. Failure is sticky,
  stops the child without further callbacks, and leaves the caller's mapped raw
  committed prefix available. A process/storage exit never invents fixture closure.

The coordinator's adapter-return timestamp is sampled immediately after its
**supplied adapter** returns. In particular, with `BrokerClient` this includes IPC
and is later than the real broker hardware endpoint. It **must not substitute for
P1's independently captured native endpoint**. Frame records bind their exact
minimal complete-drive and complete-adapter span IDs. Every mode intrinsically
records the adapter span, even for a bare adapter. A decorator must not write a
second copy into that same sampler; an independent broker source has its own
identity and timing. Complete-drive timing encloses worker dispatch, snapshot,
C decoding, adapter work and resolution acknowledgement.

Worker bootstrap explicitly calibrates the complete minimal sampler before
fixture creation/loading in every provisioned mode. Overflow, missing aggregate
retention or existing source errors reject setup before any fixture allocation.
The coordinator also calibrates its fresh sampler before its work. Calibration
probes are retained separately, never counted as frame-bearing work or subtracted
from measured durations.

## Verification and remaining work

Software tests cross actual child processes, shared mappings, Lua/canvas, and
fake hardware. A full host-clock test runs A/B/C children with a common future
T, at least five seconds of warmup, an unchanged 60-second measured interval and
two-second drain. It verifies actual semantic/source closure, no loss/errors,
allocation identity and nested minimal sample placement. Shared-epoch execution
bounds suite duration; it is **not** the sequential A/B/C/C/B/A comparison.

Other regressions cover all-mode metadata/pending retry/supersession, malformed
shared and raw fields, calibration failure before source loading, plan mismatch,
ambiguous receipt equality, real normalized fake-input forwarding through a
child token mutation and causal pixels, failed adapters and premature closure.
No physical inputs, performance budgets or native provenance are certified.

The coordinator closes **known lockstep work**, not arbitrary asynchronous
omissions. A/B have no general dropped-allocation transport; dropped pending work
remains unresolved and prevents fixture closure, rather than being guessed from
a successor. The complete collector must separately verify raw and minimal source
health, source/cohort closure, calibration, cleanup and all normalized input.
These are not implied by a coordinator acknowledgement.

[Private provisioning](native-performance-provisioning.md) now acquires trusted
root-owned declarations and binds bounded role/process/plan tickets; the
[broker source](native-performance-broker-source.md) records original adapter
input/return seams. Neither is installed startup integration or native evidence.
Still required: actual service/control-channel discovery and provisioning,
complete build/mapping/device provenance, ordered original receipt forwarding,
complete installed supervisor lifecycle integration,
source-complete native assembly and the full overhead battery, versioned native
artifacts and coherent release-verifier gates. Then an exact-commit RPM and a
fresh authorized complete #17/#24 hardware/rollback transaction are required.
Approved budgets and old failed ledgers remain unchanged.
