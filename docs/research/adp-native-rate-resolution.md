# M2 native-rate diagnosis: intended 30 FPS, conflicting release target

## Authoritative response

On **2026-09-11 at 11:17:07 UTC**, the ADP maintainer/original author (`WhatAmISupposedToPutHere`) closed [AsahiLinux/linux#613](https://github.com/AsahiLinux/linux/issues/613#issuecomment-5633632681):

> Not actionable. 30 fps is what the panel is supposed to run at. Please do not open llm-authored issues in the future.

**Correction to the investigation’s premise:** approximately 29.67 DRM updates/s is consistent with the maintainer’s stated intended 30 FPS operation. It is not, by itself, an actionable driver-performance bug. Roughly 60 FE interrupts/s and a mode string reporting 60 Hz did not establish 60 distinct panel updates/s. The earlier attempt to turn that assumption into an upstream bug report was mistaken.

Do not file additional LLM-authored upstream issues or continue this thread with generated follow-ups. No proprietary driver binary was acquired or disassembled. No kernel/driver patch, synchronization bypass, free-run mode change, or Sliver takeover was performed.

The earlier measured gate ordering and phase data remain observations. They are not retroactively false, but they no longer justify pursuing an “early completion fix” to satisfy a presumed native 60 FPS panel contract. Nor does this maintainer statement measure optical FPS, causal input latency, or a successful Sliver release run.

## Requirement provenance

This was not merely a verifier typo:

| Source | Existing requirement or behavior |
| --- | --- |
| [Parent issue #1](https://github.com/wawow830/sliver/issues/1), user story 66 | Native 2008×60 output at up to 60 frames/s |
| #1, Canvas and frame interface | Targets up to 60 frames/s on tested M2 hardware |
| #1, testing requirements | Explicit native 60-frame/s M2 target; measure misses and broker presentation |
| [Release issue #17](https://github.com/wawow830/sliver/issues/17) | Sustain the agreed 60 FPS native video target |
| [`verify-release.sh`](https://github.com/wawow830/sliver/blob/ef5941c/scripts/verify-release.sh#L695-L716) | Requires at least 30 seconds, **≥59.5 FPS**, zero misses, no latency growth |
| [`ea8a858`](https://github.com/wawow830/sliver/commit/ea8a858cabf61c3a154e3249e9bbf1e59dfe4800) | Introduced the numeric ≥59.5 gate on 2026-08-31, enforcing the existing 60 FPS premise |
| [`lua_integration_tests.rs`](https://github.com/wawow830/sliver/blob/ef5941c/crates/sliverd/src/lua_integration_tests.rs#L684-L735) | A separate **software/fake-adapter** raw-pixel producer benchmark, not a panel-rate measurement |

The public 60 FPS hardware acceptance criterion therefore conflicts with the newly supplied hardware expectation. **No threshold, parent acceptance checkbox, test, or production code was changed to paper over the conflict.** [Sliver #23](https://github.com/wawow830/sliver/issues/23) is now a specification/acceptance decision, not an unattended driver-fix task.

## Recommended decision, not yet adopted

Amend the M2 native-output contract to the intended **nominal 30 FPS**, while retaining the 60 FPS software benchmark as headroom/stale-frame-drop coverage. Approval must settle:

1. The hardware-rate acceptance threshold and its justified measurement tolerance. Do not pick a number merely because the archived 29.67/s diagnostic passes it.
2. Whether the native workload runs at the approved panel cadence or intentionally overproduces to test stale-frame dropping. Count intentional producer drops separately from missed presentation deadlines; the spec already requires dropping stale completed frames rather than building latency.
3. The frame identity, observation point and causal input-response measurement. Successful dirtyfb calls may be delayed/coalesced by the UAPI and are not independent optical presentations.

The [source observation audit](native-performance-observation-audit.md) now traces the identity, timer, clock and causality gaps through the current implementation. The [acceptance proposal](native-performance-acceptance-proposal.md) supplies explicit observation/accounting choices and an approval checklist; it is not an adopted contract.

Only after that decision should the verifier, fixture/evidence contract, tests and parent requirements be updated coherently. Preserve old failures and require fresh evidence under the revised contract; do not relabel an old ledger as accepted. The committed input-access rollback fix also still needs a fresh full verification transaction before final release acceptance.

## Retired follow-up and evidence

The extra physical-Fn FE-status capture request was withdrawn after the maintainer response. The previously handed-off `check-fe-status.sh` now only prints the superseding explanation and exits without privilege, tracing or input requests. Its original wizard is preserved locally as `check-fe-status.retired.sh`; do not run it to pursue the superseded 60 FPS driver-fix premise.

The ten-second **empty** baseline remains empty/insufficient, with no FE samples, clean probe removal, and no performance claim. No later update capture was used to claim acceptance. tiny-dfr remains active and in control; no diagnostic trace instance or probes remain from this work.

Artifacts under `/home/wawow/sliver-kernel-diagnosis/20260911/`:

- `upstream-613-response.json`: issue state, closure time and primary response URL/body; SHA256 `bcdb399b3101561866e4e99e121d41ce7732ceb6589ba357fc82731a067c119c`.
- Original completion/throughput evidence remains untouched; [phase analysis](adp-completion-phase-analysis.md) and [historical hardware notes](adp-historical-hardware-notes.md) retain their limits.
- Offline validation: 12 phase tests, seven original completion tests and 17 FE-status parser tests passed. The empty baseline’s `--require-status` check correctly failed. These do not turn the native 60 FPS release gate green.
