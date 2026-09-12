# P1 policy evaluation over supplied observations

P1 is [approved at its immutable revision](native-performance-policy-approval.md).
`scripts/evaluate-native-performance.py` implements its numeric/accounting rules
at the offline evaluator seam. **It is not a native collector, provenance
verifier, release-evidence schema or replacement release gate.**

## Interface and verdicts

```sh
python3 -B scripts/evaluate-native-performance.py capture.json
python3 -B scripts/test-evaluate-native-performance.py
python3 -O -B scripts/test-evaluate-native-performance.py
```

Module interfaces are `evaluate(document) -> report` for a complete declared
battery and `evaluate_case(case, source=...) -> report` for one declared case.
Malformed, unsupported, open or causally inconsistent evidence raises
`EvidenceError`. Internally consistent observations that violate a budget
produce `policy_result: "fail"` and named failures. The CLI returns 0 only for
a **policy-arithmetic pass**, 1 for failures/invalid evidence, 2 for usage.

Every result still says `acceptance: "not_evaluated"` and
`source_authentication: "not_verified"`, including a caller-supplied
`source: "native-broker"`. Never use CLI success as a release pass. The CLI
hashes exact input bytes, does not rewrite captures, rejects duplicate JSON
keys/non-finite numbers, and limits input to 16 MiB. No devices, clocks,
subprocesses, services or network are consulted. Ordinary Python and optimized
Python execute the same validations; no evidence checks depend on `assert`.

## Battery document

Exactly these keys:

- `schema`: `sliver-native-policy-evidence-v1` (**not** legacy
  `sliver-performance-v1`, experimental `sliver-native-observation-v0`, or a
  native release schema).
- `policy_revision`: `cbe2b5da809eed7cc4b3406b222baf43e1e34a2d`.
- `policy_sha256`: `fa565559057f855726e5bc728911617b76b1003f4116bfc21fbd3e05060f5a99`,
  the exact approved candidate file's bytes. No numeric override is accepted.
- `source`: `synthetic` or `native-broker`. Optical evidence is unsupported.
- `cases`: exactly cadence, causal-response, requested-overproduction, in order.
- `overhead`: exactly six runs in A/B/C/C/B/A order, described below.

All nine run IDs must differ. Overhead runs precede the three cases, and every
run's observed closure must precede the next arming; merely reaching its nominal
end deadline is insufficient. The declaration is **not proof** of
live predeclaration or the actual installed fixture/build. The current reader
cannot authenticate that correspondence and refuses extra provenance-shaped
fields rather than implying that a hash label proves it.

## Case document and extended observation stream

Each case has exactly `name`, `run_id`, `arm_ns`, `start_ns`, `stop_ns`,
`end_ns`, `capture_dropped`, `cleanup_ok`, `events`. Names are `cadence`,
`causal-response`, `requested-overproduction`. Times/counts are nonnegative
signed-64-bit integers, not booleans/floats. `start_ns` is T; stop is exactly
T+60 s, end is exactly stop+2 s. `arm_ns < T`. Loss must be zero and cleanup is
an explicit boolean. Failed cleanup produces a policy failure, not a waiver.

`events` contains up to 100,000 records in nondecreasing host-monotonic time;
array order disambiguates equal timestamps. It includes **all warmup,
measurement and drain frames in one sequence**. No frame IDs are reset or
renumbered when T is crossed. All underlying
[v0 frame/input/call events](native-performance-observation-format.md#event-stream)
are supported, **without** its optional schedule, `opportunity_id` render field
or explicit `opportunity_skipped` events. P1 assignment is derived independently
from actual render allocation time, not accepted from supplied opportunity
labels. All underlying v0 identity, lifecycle ordering and closure checks apply.

Additional exact event fields beyond `kind` and `time_ns`:

| Kind | Fields | Meaning |
| --- | --- | --- |
| `input_idle` | none | Exactly one initial no-active-contact preflight at arm. |
| `timer_registered` | `timer_id`, `period_numerator_ns`, `period_denominator` | Exactly one observed periodic animation registration before the first warmup render. Canonical period is `1000000000 / 30` or `/ 60` according to case. |
| `timer_dispatch` | `timer_id`, `skipped` | Actual dispatch and observed skipped-interval count for the registered timer; at/after registration and before stop. At least one dispatch record required. Counts do not manufacture frames or prove a sustained producer rate. |
| `warmup_closed` | none | Exactly one closure after at least five seconds since first warmup allocation and strictly before T; all warmup frames/inputs/contacts must already be resolved. |
| `touch` | `phase`, `contact_id` | Every normalized physical down/move/up/cancel at broker receipt. No hit-region eligibility filter. |
| `key` | `key_id`, `pressed` | Every normalized key/Fn transition; key identity is a string and pressed is boolean. Measured/drain transitions invalidate the case. |
| `input_delivered` | `input_id`, `contact_id` | Observed worker delivery of the specific received down. |
| `input_mutation` | `input_id` | The fixture's token mutation in that delivered callback, before any frame carrying the new token. |
| `capture_closed` | none | Final observed closure at or after end. All other records and all work must resolve by end; a later closure never extends draining or countable rate. Storage closure alone does not supply this semantic work/transaction closure. |

The future end deadline and actual capture-closure timestamp are distinct. Do
not backdate an actual clock read to make it land exactly on E. Frame/contact
closure at equal timestamps follows **record order**, not an invented 1 ns gap.

Every `touch:down` has exactly one underlying `input` at the same receipt
nanosecond. IDs use the v0 ASCII identity syntax; a contact ID can be reused
only after up, whereas causal input tokens are run-unique. Delivery and mutation
must occur once in the observed order. Every render must carry the latest
mutated token (or null before any mutation); matching render/broker JSON alone
cannot substitute for that chain. Actual pixel decoding remains a collector
responsibility; the evaluator validates its supplied result, not pixels.

Duplicate/concurrent downs, cancellation, broken order, missing delivery,
mutation without delivery, outstanding contacts at closure, or unaccounted
inputs invalidate evidence. Inputs before T must drain before warmup closure.
Only the causal case permits touch during measurement/drain. New downs after
stop invalidate it; existing contacts can move/lift during drain. A render
allocated during drain is valid only to answer an unanswered pre-stop input.
It is unscheduled and cannot add rate or opportunity credit.

The evaluator runs the existing v0 accounting engine over the **whole** event
cohort for identity/disposition consistency, then applies P1 interval rules
separately. It does not change v0 or relabel its whole-capture throughput as
P1's measured rate. Warmup frames cannot add rate or reset measured IDs.

## Derived checks and reports

Every real measured render is assigned the latest due, unresolved opportunity,
if any. Earlier unresolved opportunities are skipped; renders without a due
opportunity and causal-only drain frames are unscheduled. Scheduled successful
returns in `[T,stop)` supply the fixed denominator rate and edge-inclusive gaps.
All opportunities retain their assigned frame/disposition or an explicit
skipped result. Late completions can count toward rate without meeting a
software deadline; timely drain completions can meet deadlines without counting
in-window rate. Requested-60-Hz load reports its misses but does not apply the
native 15/1,800 allowance.

All measured dispositions, including drops and unscheduled frames, retain the
100 ms age/residence bounds. Any render/submission/broker failure, input timeout,
replay, or generation replacement is independently disqualifying; the miss
allowance does not excuse it. Missing phases or raw loss invalidate the capture.

Input summaries use receipt-window membership, every eligible down, fixed six
10 s windows, the 60/8-sample and 2 s gap rules, 150 ms maximum, and the maximum
positive difference between any earlier/later window maxima. Reports retain
all latencies, p95 (nearest rank), maximum and **`median_twice_ns`** (exact integer
twice the median, avoiding floating-point loss). Threshold equality passes.

## Overhead observations

Each overhead run has exactly `mode`, `run_id`, `arm_ns`, `start_ns`, `stop_ns`,
`end_ns`, `closed_ns`, `warmup_start_ns`, `warmup_closed_ns`, `capture_dropped`, `cleanup_ok`,
`samples`. Modes are A/B/C/C/B/A. The same five-second warmup, 60-second measured
interval and two-second work-drain deadline are required. Actual `closed_ns`
must be at/after that deadline, without extending sample eligibility or work
resolution. Unlike detailed mode C,
minimal modes A/B have no per-frame marker identity claim; warmup/closure fields
are supplied launcher assertions, not authenticated proof.

Each of up to 100,000 samples has exactly:

```
drive_start_ns render_start_ns render_end_ns present_start_ns
present_end_ns drive_end_ns ok
```

Times must be ordered in that sequence; each complete frame-bearing drive
finishes before the next starts. Drive/render starts are in the measured
interval, completions may drain. The full-drive timestamps bracket **all**
detailed observer processing; call counts alone cannot detect broker-side
observer delay. The collector must place those clocks correctly; the evaluator
checks the declared ordering, not clock placement in an unknown executable.

The reader derives in-window successful calls from `present_end_ns < stop`,
reports boundary calls/errors separately, and derives nearest-rank p95 render,
complete-drive and hardware-call durations from every measured-cohort sample.
Empty samples, no successful in-window calls, capture loss or malformed ordering
invalidate comparison; errors and cleanup failure disqualify it. Within each
triplet, B/A, C/B and C/A must each preserve 99.5% of reference calls and add
at most 1 ms to both render and full-drive p95. Bad comparisons are never pooled
away; estimated overhead is never subtracted from native results.

## Remaining integration

This is a tested policy-arithmetic slice, not finished #23/#17 acceptance.
Trusted live collection must bind all records, actual timer/token observations,
source/loss sequences, input device/contact origin, fixture/observer/build
hashes, clock placement/resolution, sole ownership and cleanup. The independent
Rust observer slice has a different raw in-memory interface and no production
service arming/export yet; it does **not** emit this document today. No converter
may fill absent events with guesses. The marked timed P1 fixture, local launcher,
versioned release verifier/schema integration and fresh RPM/hardware transaction
remain required. Legacy failed ledgers and the old verifier's gates are not
reinterpreted by this developer tool.
