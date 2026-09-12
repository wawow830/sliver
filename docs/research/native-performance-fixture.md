# Private P1 fixture and calibration

Implemented under the [immutable P1 approval](native-performance-policy-approval.md).
These are software collection components, not native evidence or a release pass.
Ordinary service staging remains unprovisioned.

## Reviewed workload capability

`scripts/native-performance-p1.lua` is the canonical, uninstalled workload.
`diagnostic_fixture.rs` owns its private capability. Before capability-bearing
execution, the bytes actually loaded must exactly equal the embedded reviewed
source. The marker encoder is evaluated from embedded bytes, not a user-selected
`require` path. Normal Lua entry receives no arguments. No public Lua API,
global, environment switch or broker identity payload is added. This is trusted
instrumentation, not a sandbox against native Lua code.

A bounded plan fixes run/generation, requested 30/60 Hz, A/B/C mode and integer
host-monotonic T. S=T+60 seconds and E=S+2 seconds are derived, not adjustable
independent endpoints. All modes allocate IDs before attempts and execute the
same scene/control logic. A omits marker pixels, B/C include them, and only C
retains detailed fixture observations. Minimal timing is separate from detailed
recording.

The fixture uses the existing repeating timer, redraw mechanism and canvas.
The allocation guard suppresses requests crossing S; the runtime retires the
identified periodic timer before dispatching it late. Warmup needs at least
five seconds from the first allocation, quiescence and explicit coordinator
confirmation of known work closure strictly before T. At S, only a pending
confirmed pre-S causal down can admit a new drain frame, and not if outstanding
work already carries that token. E closes allocation admission.

The down callback mutates capability-owned token state once within its delivered
receipt context. Receipt-side contact and unanswered-input frontiers are kept
separately from callback delivery: queued overlapping downs cannot become valid
merely by delaying their callbacks. Up at E is permitted. A supplied frame
resolution carries its actual endpoint timestamp; local confirmation time must
not replace that endpoint. The coordinator must preserve original source-event
order, including notification order for equal timestamps, rather than ordering
by forwarding arrival.

These checks concern known fixture work. They do not establish physical-input
eligibility, initial physical idleness, complete source discovery or native
provenance. A missing cross-role observation cannot be manufactured from a local
allocation count or a successful command reply.

## Clock domains and raw export

Every allocation samples actual host `CLOCK_MONOTONIC` once, independently of
the explicitly synthetic clock used by in-process phase tests. The same host
sample is retained locally and in raw allocation records. Raw tag32 preserves
`synthetic`, decision time, typed fixture observation and supplied resolution/
response endpoint independently; `Record.at_ns` remains the actual host sample.
Neither `synthetic=false` nor a valid codec authenticates a source.

C-only observations use a bounded local buffer and may mirror to the bounded
mapped source. Raw overflow, premature closure and prior source errors cannot
be hidden by a healthy-looking local report. Failure observations remain sticky.
`FixtureObserved(Failed)` and failed resolutions each contribute a raw failure
observation, not unique failed-work credit. Fixed-record decoding rejects
unknown variants, invalid fields, truncated bodies and nonzero padding.

## Private direct-child provisioning

The existing worker bootstrap accepts an optional bounded plan after its ordinary
fields. Run/generation lengths, version, rate, mode, causal flag and derived
endpoint arithmetic are checked again in the child. Its clock is always
`HostMonotonic`; there is no serialized synthetic-clock choice. Capture level must
match the plan, and C attaches the raw sink before source binding/execution.
Ordinary staging explicitly supplies no fixture.

Private typed controls carry `Receive`, `Resolve`, `ConfirmWarmupClosed` and
`Finish` over the existing watchdog/status protocol. Their decoder consumes the
entire bounded payload before applying any effect. Normal shutdown closes the
recorder but does **not** invent a fixture `Closed` observation. The private
parent-side staging/control entry points remain test-only.

The actual child tracer preserves supplied resolution endpoints independently of
host=decision observation times. A receipt crosses the actual Lua callback and
changes token pixels; early warmup/finish commands fail against real host time.
These tests do not replace the five-second warmup or the 60+2-second measured and
drain intervals with shorter epochs, and do not claim a successful complete
host-clock lifecycle. Additional direct-child tests check identical A/B/C scene
pixels outside the marker rectangle and matching minimal callback-span placement,
reject altered source before execution, and reject malformed/bounded plan and
control packets before effects. Marker decoding in the C tracer supplies software
test context, not authoritative all-mode allocation/publication correlation.

## Complete minimal-sampler calibration

`TimingCapture::calibrate()` explicitly runs 32 no-op probes before work. The
outer clock brackets include the sampler's clocks, counters, retention and
optional raw export. Probes are retained as `Calibration`, never frame-bearing
render, present or drive work. Tag27 retains the complete fixed array of 32
costs. No cost is subtracted from measured operations and no numeric acceptance
is inferred.

Successful calibration requires local closure/error/loss checks **and** an open,
healthy authoritative raw source before and after probing and after the aggregate
append. Even if all 32 probes fit, loss of the aggregate rejects calibration.
The non-closing source health accessor neither copies records nor resets errors.
It is a point-in-time check, not a guarantee about future writes.

Calibration is explicit; ordinary worker startup does not automatically run it.
Missing calibration remains an assembly/integration gap, not implicit success.

## Broker adapter seam

`diagnostic_hardware.rs::ObservedHardware` installs optional detailed and minimal
observation around a supplied adapter before fallback-supervisor construction.
All other hardware operations and lifecycle/ownership queries delegate unchanged.
Its minimal present span covers the complete **decorated adapter operation**,
including detailed decode/record work when enabled; it is not a bare ioctl or
DMA duration. Detailed present and poll records still come from the independent
complete-call and normalized batch-receipt seams. There is no added broker wire
field or service arming.

Targeted tests cross a real broker Unix protocol loop, real Lua/canvas and fake
hardware in both minimal-only and detailed modes. Additional adapter tests
preserve errors, device availability, reacquisition and backlight behavior with
capture off/on. These are not installed broker/service or M2 hardware tests.

## Remaining integration

Complete process/coordinator lifecycle integration, allocation-to-publication
correlation in all modes (including markerless A), trusted root-owned startup
acquisition, per-role identity/build binding, source/cohort closure, complete overhead battery,
versioned native artifact assembly and release-verifier integration must be
completed together. The separately authorized hardware/RPM/rollback transaction
remains required. Old ledgers, approved budgets and native release gates are not
changed by this software slice.
