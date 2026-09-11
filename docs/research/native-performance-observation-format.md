# Offline native observation reader (experimental v0)

Implemented after the user's **2026-09-12 “yes, don't stop”** to the acceptance-planning continuation. This permits progress on the nominal 30 FPS direction and offline evidence plumbing; it does **not** supply missing numeric budgets or select broker-return versus optical release acceptance. The existing 60-frame software benchmark is retained. See the [proposal and outstanding decisions](native-performance-acceptance-proposal.md).

## Scope and interface

`scripts/analyze-native-performance.py` is an uninstalled developer tool. Its module interface is `analyze(document) -> report`, raising `EvidenceError` for inconsistent or unsupported evidence. Its file interface is:

```sh
python3 -B scripts/analyze-native-performance.py /path/to/capture.json
python3 -B scripts/test-analyze-native-performance.py
```

CLI exit 0 means **internally consistent observations**, never release acceptance. Every report says `acceptance: "not_evaluated"`, preserves the declared source, and says `source_authentication: "not_verified"`. Errors exit 1 without a report; usage errors exit 2. No hardware, tracefs, services, subprocesses, live clocks, or network are consulted by the reader. It reads only the supplied capture file. The CLI hashes its exact bytes into `capture_sha256` and never rewrites it.

This is the first, **partial** implementation of the proposed reader. It is not invoked by `verify-release.sh` or `verify-release-evidence.sh`, cannot replace v1 artifacts, and has no threshold override. Its synthetic tests run in `check-release-surface.sh`. No production collector, pixel-marker encoder/decoder, fixture modification, broker protocol change or new public command is included.

## Capture header

One JSON object, with **exactly** these keys:

```json
{
  "schema": "sliver-native-observation-v0",
  "source": "synthetic",
  "run_id": "example-run",
  "clock": "CLOCK_MONOTONIC",
  "start_ns": 0,
  "end_ns": 1000000000,
  "window_ns": 250000000,
  "capture_dropped": 0,
  "events": []
}
```

The empty event array above illustrates the header only; an empty capture is rejected. `source` is `synthetic` or `native-broker`. A caller's label does not authenticate hardware provenance. Optical evidence is unsupported and rejected, not coerced into broker evidence. All times are absolute values in **one declared host monotonic clock**; all time/count/frame-ID values are integers in `[0, 2^63−1]`, not floats or booleans. The interval must be positive. Input receipts belong to `[start_ns, end_ns)`; terminal results may occur exactly at `end_ns`.

`window_ns` is a predeclared positive aggregation width, **not** an approved latency/jitter budget. Its windows are anchored at `start_ns`; the last may be shorter. `capture_dropped` must explicitly be zero. Nonzero loss prevents analysis, rather than producing a misleading clean report. Unknown fields, schemas, JSON duplicate keys and non-finite numbers are rejected. Limits: 16 MiB per CLI file, 100,000 events and 100,000 windows.

IDs and reason codes are nonempty ASCII strings of up to 128 characters, containing letters, digits, `.`, `_` or `-`. A frame key is `(run_id, generation, frame_id)`; `run_id` is fixed by the header; each broker-start record independently repeats the decoded run marker, which must match it. Each generation's render IDs start at **1** and increment by one, including failed attempts. Generation changes allow numbering to restart; reusing an old key, wrapping or omitting an allocated attempt fails. This v0 deliberately supports a **closed cohort**, not arbitrary cuts through an ongoing run. Existing/warmup frames, omitted edge events and unfinished draining are unsupported, not silently excluded.

## Event stream

Every event has `kind` and `time_ns`. Events must be in observation order with nondecreasing timestamps inside the interval. Array order disambiguates equal timestamps; the reader does not sort events to repair an invalid sequence.

| Kind | Additional exact fields | Meaning / required prior state |
| --- | --- | --- |
| `render` | `generation`, `frame_id`, `input_id` | Identity allocated before rendering. `input_id` is null or a previously received input token claimed to be encoded in this frame. |
| `publish` | `generation`, `frame_id` | Successful complete-frame publication after `render`. |
| `select` | `generation`, `frame_id` | Published frame chosen for presentation. |
| `supersede` | `generation`, `frame_id` | Published, not selected frame reclaimed/dropped. Distinct from skipped generation or deadline failure. |
| `invalidate` | `generation`, `frame_id`, `reason` | Published, not selected frame invalidated; retains declared lifecycle reason. |
| `render_failed` | `generation`, `frame_id`, `reason` | Attempt failed before publication. |
| `not_submitted` | `generation`, `frame_id`, `reason` | Selected frame never reached hardware (e.g. an earlier output effect failed). No broker result is invented. |
| `broker_start` | `run_id`, `generation`, `frame_id`, `input_id`, `call_id`, `replay` | Hardware call begins. Frame and input IDs must come from independently decoded complete-frame content in a future collector, not numbering observed calls. Marker must match the render record. `replay` is boolean. |
| `broker_end` | `call_id`, `ok` | One result for the outstanding call; `ok` is boolean. Success means hardware-call return, not DMA retirement or optical presentation. |
| `input` | `input_id` | Unique causal token at declared broker input receipt. |
| `input_timeout` | `input_id` | Explicit observer timeout for the outstanding transition. This records the timeout, not approval of its duration. |

Only one broker call can be outstanding. A non-replay call requires a selected frame and cannot be retried under the same render identity. A replay requires a previously successful frame and a fresh call ID. Successful replays are reported but never counted as new producer updates or used to reset original frame age. Failed calls and failed replays remain visible in `broker_calls` and `broker_errors`.

Every recorded attempt must end as completed, broker error, superseded, invalidated, render failed or not submitted. Every call needs a result. Every input needs a response or explicit timeout. Missing events fail closed; an explicit failure remains valid **diagnostic data**, not a clean-run verdict. Reason strings and `capture_dropped=0` are observer assertions, not independently verified facts.

Input mode is **isolated transitions**, not a high-load/coalescing test. A new input requires the preceding one to have responded or timed out. A render token must already exist; broker and render markers must agree. First successful return of content carrying that token establishes its reported response. Unrelated animation frames and unsuccessful calls do not answer an input. A response after a recorded timeout retains **both** the timeout and late latency. Retained/replayed old tokens cannot answer a newer input.

## Derived report and limits

- `successful_unique_updates`: first successful result per complete render identity.
- `between_returns_per_second`: `(unique_count−1)/(last−first)`; null when fewer than two distinct return times exist. This excludes edge idle time.
- `whole_interval_updates_per_second`: unique successful updates divided by the **full declared interval**. It does not discard start/end silence.
- `frames` and `dispositions`: all recorded attempt identities, terminal dispositions, original render/return times, reasons and input tokens.
- `broker_calls`: all call identities, original frame keys, entry/result times, result flags and replay flags.
- `max_frame_age_ns`: maximum original render-start → first successful broker return. Not queue occupancy or age of all still-visible images.
- `inputs`: every receipt, response/latency or unanswered timeout, including late responses.
- `input_window_max_latency_ns`: maximum response latency bucketed by **input receipt**, with null for windows without measured responses. Timeouts remain separately enumerated.
- `peak_window_increase_ns`: largest positive increase from any earlier observed window's maximum to a later observed window's maximum (zero if comparable windows never worsen, null if fewer than two windows have responses). This exposes an interior spike despite equal endpoints. It does **not** by itself prove absence of within-window growth, adequate sampling, or no latency growth under a yet-unapproved rule.

`optical_fps`, `missed_physical_refreshes`, `native_deadline_misses` and `skipped_generation` remain **null**. The reader has no schedule/opportunity ledger and does not reinterpret timer callbacks or identity gaps as physical deadlines. Supersession counts are not forgiveness for native misses. No numeric policy is inferred from synthetic examples or archived measurements.

## Remaining work before release measurement

1. Select the acceptance observation point and approve rate/deadline/latency/sampling policy; nominal 30 FPS is not a numeric tolerance.
2. Complete the schema for predeclared schedule opportunities, edge cohorts, exact build/fixture provenance, capture closure/loss attestation and instrumentation-overhead comparison. The current v0 is intentionally not a release-evidence revision.
3. Implement and offline-test actual pixel identity encoding/decoding and bounded observers at the existing production seams. Caller-supplied JSON correlation alone is not proof of independently observed causality.
4. Test native-cadence deadlines and overproduction separately, including skipped generation, pending-frame replacement, supersession, lifecycle invalidation, late responses and queue/frame-age bounds.
5. Update parent requirements, canonical fixture, verifier, schema and numeric gates coherently, then run a fresh authorized hardware transaction with the committed rollback checks. Preserve old failures; no v0 or v1 report is silently promoted into a new release pass.
