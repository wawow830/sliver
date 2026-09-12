# Native M2 performance policy candidate P1

**Proposed, not approved or implemented.** This makes the remaining choices in
[#23](https://github.com/wawow830/sliver/issues/23) concrete after the user's
“don't stop” continuation. That instruction authorizes preparing the proposal,
not treating the numbers below as accepted. The existing ≥59.5 native gate,
parent requirements and failed evidence remain unchanged.

## Recommended decision

Adopt **successful broker updates**, not optical presentation, as the M2 release
measurement. Use nominal 30 Hz software opportunities, a **29.75 updates/s**
minimum, a **100 ms** completion/freshness budget, and an isolated causal-input
check with **150 ms** maximum response and **one nominal period** allowed
increase between window maxima. Keep software headroom and overproduction
claims separate. No kernel change or optical instrument is required by P1.

These are proposed **product budgets**, not established panel specifications:

| Choice | Rationale and limitation |
| --- | --- |
| 29.75/s minimum | Preserve the old relative rate allowance: `30 × (59.5 / 60)`. This is a conservative continuity choice, not a measured hardware tolerance. The archived ~29.67/s diagnostic remains below this floor. |
| 60 s measurement; 5 s warmup | Twice the old minimum observation duration; includes start/end silence instead of selecting a favourable first-to-last span. Not a reliability or thermal endurance test. |
| 100 ms deadline, update gap and original-frame age | Three nominal 30 Hz periods bound individual stalls as well as average throughput. This is a proposed responsiveness allowance, not a DRM retirement guarantee. |
| 150 ms maximum causal response | A new explicit input budget; numerically matches the existing fake benchmark's age ceiling but measures a different path. That benchmark does not validate this native/input limit. |
| 33,333,334 ns window increase | One nominal period, rounded up, tolerates scheduling phase variation while bounding a worsening middle window. Replaces literal zero growth with an explicit finite-sample jitter rule, not a claim of no possible growth. |

Choosing 29.75 means the next real run may still fail. Do not adjust P1 after
seeing a new run to obtain a pass. If this budget is unacceptable, propose a
separately justified absolute revision or an independent hardware-reference
protocol **before** acceptance capture. Either requires explicit approval;
P1 deliberately does not combine methods or fit a reference to Sliver.

## Observation, identity and clock

- Endpoint: successful return from the real M2 hardware adapter's complete
  `present` operation, including initial setup when applicable. Neither an FE
  interrupt nor an individual ioctl count substitutes for this event. Call it
  a **successful broker update**, never optical FPS or DMA completion.
- Independently decode run/generation/frame/input identity from a stable
  complete-frame snapshot at broker entry. Join it to separately observed
  render allocation, publication, selection and terminal disposition. Use the
  [tested digital marker format](native-performance-marker-format.md); CRC and
  matching JSON labels are not source authentication.
- Use integer host `CLOCK_MONOTONIC` nanoseconds at all software observation
  seams. Do not subtract Lua intended time or unrelated `Instant` origins.
  Record clock-read resolution and diagnostic timestamp-placement overhead;
  neither changes the stated integer limits or supplies optical calibration.
- Count only a frame's first successful **non-replay** return. Recovery,
  restoration, repeated content identity and warmup frames cannot add updates,
  satisfy a new opportunity, answer a new input, or reset original frame age.
- Every attempt, publication, selection, call and accepted input must resolve.
  Capture loss, undecodable markers, missing dispositions, render/submission/
  broker errors, timeouts, worker replacement, recovery, device loss or suspend
  make the performance case unsuccessful. Preserve the events and reason;
  do not silently cut around them. The 15-miss allowance never forgives a
  globally disqualifying event. Lifecycle behaviour is tested separately.

## Fixed interval and scheduled-opportunity rules

Before starting a case, save its policy revision/hash, exact fixture/build
identity, run identity and workload parameters. Arm the collector, record a
future monotonic epoch `T`, warm up for at least five seconds before `T`, and
stop new workload opportunities at `S = T + 60,000,000,000 ns`. Observe until
`E = S + 2,000,000,000 ns` to drain; unresolved work at E is unsuccessful.
Warmup must be quiesced before T. Log its drain/closure explicitly; do not
relabel an outstanding warmup frame as measured work. A failed preflight does
not disappear; retain it as an incomplete attempt with its reason.

Measured opportunities at rate `r` are one-based:

```
due(i) = T + floor((i - 1) * 1,000,000,000 / r)
due(i) < S
```

Allocate render IDs before attempts, not when hardware calls succeed. The
assignment timestamp is the observed host-monotonic **render allocation** time,
not intended frame time or timer due time. For every render allocated in
`[T,S)`, assign the latest due, unresolved opportunity, if any, and explicitly
mark earlier unresolved opportunities skipped at that timestamp. If none is
due and unresolved, the render is unscheduled. This rule is identical for timer,
input and coalesced redraws: an input-triggered frame may satisfy an opportunity
but never more than one. Record the actual repeating timer registration and
period (`1/r` seconds), tied to the reviewed fixture hash, to establish that
30/60 Hz redraw requests were installed. Report timer dispatch/coalescing
separately; do not infer installed requests from the assigned output rate.

At S, resolve remaining unattempted opportunities as skipped. Drain may finish
existing attempts and allocate **unscheduled** frames solely to answer pre-S
inputs; those frames get no scheduled rate or deadline credit and retain all
age/input limits. No scheduled catch-up or periodic animation render may start
at or after S. Continue logging post-S events: ups releasing pre-S contacts
are permitted, new downs/cancels or unexpected key transitions invalidate this
isolated case. Post-S downs are not added to the measured input sample. The
fixture must stop periodic redraw requests at S while permitting those final
causal responses. All contacts must be released by E. Production-seam tests
must show this bounded fixture/observer behaviour without introducing a new
runtime scheduler, backdating attempts or creating an autonomous producer.

For the **30 Hz native-cadence case**:

- Exactly 1,800 software opportunities exist, regardless of executed callbacks.
  An opportunity is met only by its assigned frame's first successful original
  return at or before `due(i) + 100,000,000 ns`. Equality passes.
- All other opportunities are **software deadline misses**: skipped generation,
  supersession, failure or late completion. Keep these causes separate. Permit
  at most **15 misses out of 1,800**; this explicitly replaces the old zero-miss
  requirement, rather than calling skipped generation an intentional success.
- Independently require **at least 1,785** first successful returns of scheduled
  cohort frames in `[T,S)`: count divided by the fixed 60 seconds must be ≥29.75.
  Unscheduled redraws, replays and drain completions cannot pad this count.
  A drain completion can meet its original deadline but not this rate count.
- Sort those in-window scheduled returns in observed order. Every consecutive
  gap, including `T → first` and `last → S`, must be ≤100,000,000 ns. An empty
  list fails. This is an update-gap bound, not a physical-refresh count.
- Every measured frame, including unscheduled frames and dropped publications,
  must have original render allocation → terminal disposition ≤100,000,000 ns.
  Published residence (publication → selection or unselected disposal) must
  also be ≤100,000,000 ns. Replays do not reset either age.

Keep both rate and deadline checks: timely drain frames cannot hide silence
inside the measurement interval, and sufficient average rate cannot hide
late opportunity responses. Report first-to-last throughput only as a
secondary diagnostic. No averaging across failed cases is allowed.

## Three required native cases

Run these in this order with fresh identities, each using the fixed interval
above. Do not repeat only a failed case until a favourable sample appears;
retain every attempt and investigate failures before a new complete battery.

1. **Cadence:** 30 Hz raw full-frame animation with no intentional touch input.
   Apply all native-cadence rules. Begin with no active contacts; any physical
   touch or key/Fn transition during measurement/drain invalidates this case.
2. **Causal response:** same 30 Hz animation, plus isolated physical touch-down
   transitions that change a visible input token and request redraw. Apply the
   native-cadence rules and the input rules below. Log all normalized input;
   touch moves/ups do not allocate extra causal tokens. Unexpected key/Fn
   transitions or concurrent contacts invalidate this isolated-input case.
3. **Requested overproduction:** declare 60 Hz opportunities (3,600 total),
   no intentional touch input and the same no-active-contact/unexpected-input
   rules as case 1, otherwise the same fixture. Require ≥1,785
   unique scheduled in-window returns, the same ≤100 ms boundary/update-gap
   and original-frame/residence bounds, and complete omission accounting.
   Report skips, supersession and lateness relative to the **offered 60 Hz
   schedule**, but do not apply the 15/1,800 native-miss rule to this case.
   Synchronous backpressure may prevent actual 60/s generation. Therefore this
   case is a requested-load stress test, **not proof of a 60 FPS producer or
   physical stale-frame dropping**. Existing fake-adapter producer/drop tests
   remain separately required; no new autonomous producer is implied by P1.

The three-slot capacity and latest-complete selection also need observable
production-seam regressions. A 100 ms age bound or JSON event count does not
prove queue capacity, and successful native cases cannot waive those tests.

## Isolated causal-input rules

Origin is **broker receipt of a normalized touch-down**, before supervisor or
worker forwarding; endpoint is the first successful original broker return
whose independently decoded pixels incorporate that input token. This excludes
physical-actuation → broker acquisition delay and optical response; say so in
release results. Eligibility is **every normalized physical touch-down** from
the identified Touch Bar received in `[T,S)`, anywhere on the strip. Start with
no active contacts. No hit region, latency-dependent filter or discretionary
operator exclusion is permitted. Cancellation, duplicate down, broken contact
ordering or ambiguous source identity invalidates the case.

Require a one-to-one observed chain: broker receipt/contact identity and order
→ worker callback delivery of that same down → fixture token mutation in that
callback → render incorporating the token → independently decoded broker pixels
and successful return. The reviewed fixture increments its token only in the
down callback, not in a timer. Observer records must reconcile broker/worker
contact order with those mutations; missing or ambiguous links fail. Do not
add a public input API or broker payload field to perform that reconciliation.
This is a trusted, reviewed diagnostic fixture, not proof about arbitrary Lua.

- Prompt for approximately one isolated touch per second. Retain **all**
  eligible touch-downs in `[T,S)`; do not choose the fastest subset. There must
  be at least **60**, at least **8 in each of six 10 s receipt windows**, and
  no receipt gap >2 s, including both interval boundaries.
- Permit only one unanswered token at a time. A concurrent contact or new
  eligible down before response makes the case invalid, not an excluded
  sample. Lift before the next contact. This does not claim high-load input
  queue or move-coalescing performance.
- Every token must respond within **150,000,000 ns**, equality passing. Retain
  timeouts and late responses; either fails. Responses during drain still
  belong to their input receipt window and must meet the same deadline.
- Report every latency, sample count, median, p95 (nearest rank, index
  `ceil(0.95*n)` in the one-based sorted sample) and maximum. The **maximum**,
  not only p95, is gated. Empty or undersampled windows fail.
- Let `m[k]` be the maximum latency of inputs received in 10 s window k.
  Require `max(0, max(m[j]-m[i] for i < j)) ≤ 33,333,334 ns` over all six
  windows. Equal endpoints do not excuse an intermediate spike. This is a
  bounded sampled worsening rule, not an assertion of zero latency drift or
  a guarantee about unobserved physical actions.

## Instrumentation overhead and evidence integrity

Before acceptance, compare three modes in fixed order **A, B, C, C, B, A**,
using the same native 30 Hz scene and 5 s warmup/60 s interval for each:

- A: marker off, detailed observer off;
- B: marker on, detailed observer off;
- C: marker on, detailed observer on (the acceptance mode).

All modes retain identical minimal in-memory hardware-call count/duration,
render-callback duration and complete frame-bearing drive duration measurement,
restricted to measured fixture work, with errors and boundary work reported
separately. Drive duration starts in the supervisor **before** detailed
per-drive observer work or worker dispatch and ends **after** the broker reply
and all detailed per-drive observer processing. Thus it includes worker work,
IPC, snapshot copying/decoding and broker logging; these cannot fall outside
every measured duration. This overhead interval is distinct from the native
hardware-return acceptance endpoint. No per-frame synchronous disk writes.

Compare modes within the first A/B/C triplet and the reversed C/B/A triplet;
do not pool away a bad comparison. In **each** triplet require B versus A,
C versus B, and C versus A to lose at most **0.5%** in full-window
successful-call rate (`candidate_count * 200 >= reference_count * 199`), add
at most **1 ms to p95 complete render-callback duration**, and add at most
**1 ms to p95 complete frame-bearing drive duration**. The latter is a separate
proposed end-to-end observer budget: cadence-limited counts and renderer timing
alone can miss substantial broker-side delay. Use nearest-rank p95 as above;
retain every sample and call error. Any error, loss, empty sample set, or
inability to isolate measured work invalidates comparison.

The 0.5% proposal gives instrumentation less than the 0.8333% total nominal
rate allowance; the two 1 ms limits bound added callback and complete-drive
work separately. These are new engineering budgets, not measurements. Call counts in A/B do **not** establish
unique-frame acceptance, and this comparison cannot prove the minimal observer
itself is free. Record its timestamp/counter cost, storage bounds and output
flush/cleanup behaviour. Do not subtract estimated overhead from acceptance
results. If marker cost is too high, improve/remeasure the diagnostic design
rather than relaxing the native floor after a failed capture.

Retain exact commit, RPM NEVRA and source identity, kernel and model, fixture/
observer hashes, local-session/sole-device ownership evidence, predeclared
parameters and arm timestamp, raw per-source records and sequence/loss counts,
clock metadata, all edge/drain dispositions, measurements and cleanup result.
Hash artifacts and record who/what collected them; a hash or a caller-supplied
`source=native-broker` alone is not provenance verification. An incomplete
capture or failed cleanup is not an operator-overridable pass.

Trust the reviewed local launcher/collector and the authorized operator, not
arbitrary imported JSON. The launcher must bind the policy/fixture/observer and
installed-build hashes to the run ID and future epoch before arming; the
verifier must independently compare these with the selected policy, RPM and
actual collected artifacts, and check sequence/closure/loss records. This is
provenance and accidental-mismatch verification, **not** remote attestation,
cryptographic proof against a malicious root operator, or a new signing service.

The current [experimental v0 reader](native-performance-observation-format.md)
cannot implement this contract as-is: it lacks separate warmup/measured/drain
cohorts, authenticated predeclaration/build provenance, production collection,
verified input-token correspondence, policy evaluation and overhead evidence.
Do not feed it a hand-edited interval and promote `acceptance=not_evaluated`
into a release result. A versioned release schema and regressions must precede
new hardware acceptance.

## Approval and implementation boundary

Approval must name **P1 at its exact committed revision**, or explicitly amend
its choices, before implementation changes the acceptance contract. In
particular it must accept broker-return-only claims, 29.75/s, up to 15 software
misses, the age/gap/input/jitter limits, three workloads, sampling, and the
overhead protocol. **No such approval has been recorded.** This is not an ADR.

After approval:

1. Amend #1/#17 and the planning checklist in #23 consistently; preserve their
   old requirement and failed-run provenance in the decision history.
2. Test exact threshold equality and one-unit failure, skipped opportunities,
   surplus unscheduled updates, replay, drain boundaries, late-but-counted
   frames, interior latency spikes, sparse windows, false causal tokens,
   capture loss, provenance mismatch and failed overhead comparisons.
3. Implement fixture/observer/schema/evaluator at existing private seams;
   update verifier wording and gates together. No public diagnostic command,
   kernel synchronization change or reinterpretation of old v1 ledgers.
4. Pass exact-commit software and RPM checks, then arrange a fresh authorized
   full hardware transaction. Revalidate the committed input-access rollback
   path for #24 as well as the remaining release checks in #17.

This document alone neither changes acceptance nor authorizes service takeover.
