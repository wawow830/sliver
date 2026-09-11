# Proposed native performance evidence contract

**Status: nominal 30 FPS direction and offline implementation continuation approved; acceptance policy still incomplete.** On 2026-09-12 the user replied **“yes, don't stop”** to this planning continuation. That approves proceeding, but supplies neither numeric tolerance/latency budgets nor an explicit broker-return/optical selection. For [#23](https://github.com/wawow830/sliver/issues/23), following the [30 FPS premise correction](adp-native-rate-resolution.md). The ≥59.5 native release gate, parent requirements and failed ledgers remain unchanged pending a complete contract. No hardware capture, takeover, kernel change or upstream contact is part of this work.

## Decision to make

Recommend separating three claims rather than renaming one FPS counter:

| Claim | Proposed treatment | What would not prove it |
| --- | --- | --- |
| Software throughput/headroom | Retain the existing 60-frame producer benchmark and stale-frame-drop tests. Describe their actual duration and fake-adapter scope. | Calling the burst benchmark sustained panel output. |
| Native M2 update performance | Amend the target to **nominal 30 FPS**, with an explicitly named observation point and an approved rate/deadline budget. | FE IRQ counts, a mode string, or lowering the gate until archived data pass. |
| Visible frame/input response | Use independently identifiable frame content and causal input markers. If claiming optical timing, require a calibrated optical observation. | Successful dirtyfb return, an unrelated next frame, or numbering observed calls sequentially. |

The parent asks to measure broker presentation, not implementation-level copy counts. A broker-return contract is possible, but must be called **successful broker updates**, not optical presentation or DMA completion. Approval must explicitly select broker-return evidence, optical evidence, or both. Existing observations prove neither a new release pass nor an optical defect.

## Tolerance is a separate decision

The maintainer supplied a nominal panel rate, **not** its clock tolerance, a Sliver performance budget or an optical measurement specification. There is no source-backed numeric replacement gate yet.

Two defensible ways to specify one, requiring approval before capture:

1. **Absolute product budget:** choose allowable throughput shortfall and deadline/latency jitter from the product requirement. Record the rationale independently of old measurements. For comparison only, preserving the old relative throughput allowance gives `30 × (59.5 / 60) = 29.75/s`, which the archived ~29.67/s diagnostic still fails. This arithmetic is not a recommendation or a new gate.
2. **Hardware-relative budget:** establish an independent reference cadence using an approved measurement method and workload, then predeclare the allowed Sliver loss relative to it. Do not derive the reference by fitting the Sliver run being graded. Neither ~60 FE IRQ/s nor a nominal mode label is a validated reference for distinct panel updates.

**Recommended process:** select the observation point first, then approve the absolute budget or reference-calibration protocol before measuring. Do not collect a new release run until these choices are settled. A tolerance for measurement uncertainty is not permission to hide actual missed deadlines.

## Identity and accounting

The [source audit](native-performance-observation-audit.md) describes the current gaps. A future diagnostic fixture can encode a run identity, monotonically increasing render identity and input-state marker in a reserved region of the complete native frame. This uses ordinary canvas drawing rather than adding a public command or changing the broker wire format. Geometry, rotation, pixel encoding, marker capacity and decode integrity need offline round-trip tests before any hardware use.

Keep these quantities distinct:

- **Scheduled opportunity:** derived from a predeclared cadence/epoch, independent of which callbacks actually execute. Timer callbacks can coalesce; callback count is not the schedule.
- **Render identity:** allocated in the producer before rendering, not assigned by counting successful broker calls. Carry it in pixels for end-to-end correlation. Log attempts and failures; a Lua callback log is not proof that shared-slot publication succeeded.
- **Published/selected frame:** a complete frame became available or was chosen from the bounded slots. If exact stale-drop accounting is required, instrument publication/selection/reclamation at that existing internal seam. Optical or broker sequence gaps alone cannot distinguish skipped generation, stale reclamation and other loss.
- **Broker attempt/result:** record frame identity at the real hardware call, with entry, successful return or error. Replays/restoration of the same frame are not new producer frames.
- **Optical observation:** decode frame identity independently from the panel image. Repeated samples of a retained frame are not distinct updates. Undecodable/missing samples must not be silently discarded.

For a closed, successfully drained observation cohort, every published frame must have a recorded disposition: selected, superseded, or explicitly invalidated by a declared lifecycle event. Selected frames must have corresponding broker results or an explicit **not submitted** disposition (for example, key/backlight effects can fail before the hardware call). Do not invent a broker result for a call that never occurred. Any unresolved, failed or unclassified disposition prevents a clean-run claim. Starting/stopping frames and pending work belong in explicit edge categories, not an implicit subtraction. Worker replacement requires a new generation/run identity to prevent counter reuse.

**Two workloads, not two meanings of “miss”:**

- A native-cadence run exercises the approved native schedule and deadline budget.
- A separate overproduction run tests latest-complete selection and bounded frame age. Intentional supersession is reported separately; it must not turn an actual native deadline miss into success. A 60 Hz timer declaration alone does not prove that 60 complete frames/s were produced.

The disposition of missed timer opportunities must also be approved; don't manufacture frames for callbacks that never ran. If native display opportunities cannot be independently identified, report broker update gaps and their declared budget rather than claiming zero missed physical refreshes.

## Causal input response

Replace the measurement fixture only after approval with one whose supported touch callback changes a visibly encoded input token/state and requests redraw. The current timestamp animation is not such a fixture.

1. Name the input origin: physical actuation, kernel event timestamp, or broker receipt. These measure different delays. Default proposal for software attribution is **broker input receipt → successful broker update**, with an optional separately calibrated optical endpoint.
2. Record input order and the token/state actually incorporated into a rendered frame; correlate by identity and content, not proximity in time. For a simple isolated-transition run, allow one outstanding marked transition and retain explicit timeouts/unanswered inputs. This cannot stand in for a high-load/coalescing input test.
3. Use one declared monotonic timebase for software observations. Lua intended presentation time and an independent process's `Instant` origin are not absolute host timestamps. An optical clock needs a recorded mapping and uncertainty, not fabricated CLOCK_MONOTONIC timestamps.
4. Preserve all responses and timeouts across a fixed interval. Report sample count, individual latencies, distribution, fixed-window summaries and frame age/queue bounds. Two endpoint latencies alone cannot establish that latency never grew inside the interval.
5. Approve sampling cadence, minimum sample count, jitter allowance and the no-growth rule before capture. The existing requirement does not authorize inventing a new absolute input-latency limit.

## Smallest implementation after approval

Prefer fixture pixel markers plus **bounded userspace diagnostic events** at existing input, publication/selection and hardware-call seams. This keeps the public interface and broker payload unchanged. It does not require probes into unknown BE registers, a new completion handler, unsynchronized buffer reuse or a polling service.

A single offline evidence validator should join those observations and return derived metrics, uncertainty, omissions and contract version; it should not own hardware or grant release acceptance by itself. Its interface must distinguish synthetic/fake, native broker and optical evidence, and refuse unsupported claims. Native and fake adapters provide genuinely different evidence, not interchangeable pass signals. Instrumentation overhead needs an approved comparison; a producer-side log cannot substitute for a broker-side result.

Proposed evidence revision should explicitly identify: exact commit/RPM/kernel, fixture hash and run identity, observation point, clock/calibration, approved policy revision, predeclared interval/warmup, frame identities/dispositions, causal input links, capture loss and cleanup. Old v1 ledgers cannot be silently imported as accepted v2 evidence.

After the decision, implementation order is:

1. Amend #1 and #17 consistently; record approved policy, numbers/method, and claim names in #23.
2. Add fixture/observer/validator tests for identity, wrap/restart, repeated frames, missing events, intentional supersession, genuine deadline misses, clock mismatch, causal mismatch and capture loss. Include a burst of worsening latency hidden between equal endpoints.
3. Update verifier wording, evidence schema and numeric gates together, preserving old failed artifacts.
4. Pass software and package checks for the exact changed commit/RPM.
5. Only then arrange a fresh complete authorized hardware transaction with sole ownership and the committed rollback input-access checks. No pending decision can be waived by an operator confirming a prompt.

## Approval checklist

- [x] Nominal 30 FPS native direction; existing 60-frame software benchmark retained with accurate scope (2026-09-12 continuation approval).
- [ ] Broker-return, optical, or both observation points, with precise claim names.
- [ ] Absolute rate/deadline budget **or** independent reference-calibration protocol and relative budget.
- [ ] Native/overproduction schedules and separate skipped-generation/supersession/deadline-miss rules.
- [ ] Input origin, causal marker method, sampling and no-growth/jitter rule.

Unchecked items mean **not ready for unattended acceptance implementation**. No accepted ADR, production interface, fixture, release verifier, threshold or release verdict has changed.

## Offline implementation after continuation approval

The [experimental v0 reader](native-performance-observation-format.md) and synthetic tests now exercise closed-cohort identity/disposition accounting, replay exclusion, declared causal input links, timeouts/late responses, and fixed-window latency summaries. It is included in the source audit, not the release verifier. It has no collector, marker decoder, schedule/deadline policy or release-pass mode; accepted diagnostic records can include failures. Unsupported optical claims, capture loss, partial cohorts and old schemas fail validation. This implements a policy-independent portion of the proposed single reader, not the full future acceptance contract.

## Safe work completed while the decision was pending

Existing software checks were rerun at source commit `d90dbe39cbd5bee89d3e4f99d38906722f3cac8e`; no source/test changes were needed:

- Initial `scripts/check-release-surface.sh` failed its opportunistic real-logind test: the long-running agent had audit session 5, while current same-UID logind leaders had audit identities 1 and 13. No leader matched the caller. The focused test reproduced this in milliseconds. Returning no session in this situation is fail-closed behavior, not a reason to authorize against an unrelated active session.
- From the live graphical session, an isolated transient **user scope** inherited audit identity 13. A preflight verified both matching live-leader audit identity and `sd_pid_get_session == -ENODATA`, ensuring the focused test exercised the audit fallback rather than skipping via direct-session lookup. The unchanged focused test and full source audit passed. The temporary scope subsequently became inactive/dead. No existing Sliver/tiny-dfr service was reconfigured or restarted.
- Full audit: 235 library tests, six CLI tests, formatting, clippy, verifier/rollback tests and source/package-manifest checks passed. RPM buildroot checks were explicitly skipped; this was not a full release transaction.
- Existing release-mode raw-frame benchmark passed: reported producer rate 1249.6/s, three of 60 frames consumed, maximum sampled latency 0.013 s. This is the short, in-process, fake-adapter burst described in the source audit—not production IPC throughput, sustained 60 FPS, native output or causal input latency.

Both the original failures and the separate live-session pass are preserved under `/home/wawow/sliver-release-verification/20260911-contract-planning.m3JxMh/` (`audit.log`, `logind-repro.log`, `session-observation.txt`, `live-session-audit.log`, status files, and `software-burst-release.log`). These software-only artifacts do not replace any failed native ledger. tiny-dfr remained active and Sliver's broker inactive.
