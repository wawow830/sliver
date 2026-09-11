# ADP completion: phase constraints and passive status follow-up

2026-09-11. **Superseding finding:** upstream closed #613 after the maintainer confirmed intended **30 FPS** operation. See [native-rate resolution](adp-native-rate-resolution.md). [Sliver #23](https://github.com/wawow830/sliver/issues/23) now concerns the conflicting 60 FPS release target, not a proven driver regression. The diagnostic data below are retained; no kernel/driver changes, synchronization bypass, or Sliver takeover occurred.

## New result: not a fixed 33 ms from submission

Independent replay of the preserved 15-update tiny-dfr capture found **19.300–32.723 ms from flush entry to the accepted control sample**, despite exactly two FE observations in every cycle. This contradicts a literal constant two-full-FE-period delay **as a model of this gate**. It does not contradict hardware becoming ready near FE2, nor establish when the last framebuffer DMA read finishes.

The phase spread is real: flush occurs **0.955–14.373 ms after the preceding FE handler entry**. All first post-flush FEs already have the matching pending event; late event installation does not explain these observations.

All values below are milliseconds relative to software probe timestamps, not hardware-edge timestamps. Every rejected sample is `0x2a13`; every accepted sample is `0x613`.

| Cycle | Previous FE → flush | Flush → rejected read | Flush → accepted read |
| --- | ---: | ---: | ---: |
| 1 | 0.955 | 15.905 | 32.723 |
| 2 | 2.880 | 13.971 | 30.799 |
| 3 | 8.390 | 8.477 | 25.305 |
| 4 | 8.709 | 8.143 | 24.966 |
| 5 | 10.167 | 6.679 | 23.516 |
| 6 | 7.375 | 9.467 | 26.299 |
| 7 | 13.332 | 3.536 | 20.369 |
| 8 | 4.291 | 12.571 | 29.396 |
| 9 | 14.373 | 2.473 | 19.300 |
| 10 | 3.865 | 12.995 | 29.828 |
| 11 | 11.558 | 5.289 | 22.122 |
| 12 | 11.163 | 5.677 | 22.498 |
| 13 | 8.090 | 8.758 | 25.584 |
| 14 | 6.772 | 10.080 | 26.910 |
| 15 | 8.515 | 8.333 | 25.154 |

**Conditional model, not a measurement:** if this gate undergoes one monotonic transition at a constant delay `D` after flush entry, treating control-probe times as read-time approximations, the samples permit **15.905 < D ≤ 19.300 ms**. Thus a 17 ms model fits. The actual MMIO load precedes its probe; using the preceding pending-pointer probe gives a more conservative lower endpoint of 15.903 ms. Timestamp precision is 1 µs, and the FIFO write time is not captured. This cross-cycle constraint is not an individual completion-time measurement.

**What remains indistinguishable:** for every cycle, both a transition at first FE +100 µs and one at second FE −1 µs reproduce the sampled values. These candidate transitions differ by over 16.7 ms. More varied phases alone do not resolve the gap between samples, and neither construction claims actual hardware behavior. The [historical author recollection](adp-historical-hardware-notes.md) supplies useful context, not missing timestamps or DMA-lifetime proof.

## Keep three completion contracts separate

The [exact-source `drm_crtc_commit.flip_done` contract](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/include/drm/drm_atomic.h#L89-L100) says hardware has **flipped to the new buffers**, not that every DMA read from the new buffer has finished. Old-buffer retirement, new-buffer scanout/transfer completion, and optical presentation must not be conflated. The author’s tentative “scanout in progress” recollection alone neither proves nor disproves safe retirement of the *old* buffer at FE1; it also does not establish whether programming the next state while the current transfer runs is safe.

The [dirtyfb UAPI explicitly permits delayed/coalesced calls](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/include/uapi/drm/drm_mode.h#L752-L764). Thus one successful dirtyfb call is not a promised distinct presented frame. [Sliver’s current presentation path](https://github.com/wawow830/sliver/blob/54135dd/crates/sliverd/src/m2_hardware.rs#L1599-L1645) overwrites one mapped dumb framebuffer and marks it dirty; a Cairo staging surface is not a second scanout buffer. This source audit does **not** demonstrate an observed tear or unsafe DMA access, but any future pipelining proposal must account for buffer reuse as well as register/FIFO scheduling. Neither a faster syscall rate nor changing the completion gate alone settles those contracts. No application or driver change is proposed on this evidence.

## Exhaustive archive checks

- 1,785 FE entries; 1,785 matching pending samples: 1,755 NULL and 30 non-NULL.
- Exactly 30 control samples, all inside the 15 pending-event lifetimes. No orphan read/send, hidden rejection, or idle-window control value. Pointer reuse is matched per lifetime.
- FE gaps 16.812–16.861 ms; no extra already-enabled FE handler cadence between them.
- Pending samples lie downstream of the handler's status-bit-0 test. This supports bit 0 being set in those invocations, **not that the entire status was `0x1`**. The original trace omitted the status value.
- Earlier continuous Sliver capture: 1,038 flushes, median 29 µs after the preceding software vblank; all 1,037 adjacent flush pairs span two vblank sequences. That capture has no control/completion-pointer samples, so its individual vblanks cannot be relabeled rejected/accepted by borrowing the later observations.

Local reproduction:

```sh
python3 /home/wawow/sliver-kernel-diagnosis/20260911/phase-analyze.py
python3 /home/wawow/sliver-kernel-diagnosis/20260911/phase-tests.py
```

**12 offline tests pass**, including mutated identities/control values, hidden-read detection, trace-boundary handling, ring loss and the constructive indistinguishability models. Seven original completion-parser tests also pass. These are analyzer checks, not a fix or a new live throughput result.

Source raw: `completion-20260911-203257.txt`, SHA256 `ed6a846ce2aa451b5b4c6449167a2e21e898c65c0823eed7a7722f82bd84e203`. Full replay, per-cycle source line numbers, archived JSON cross-checks, and earlier-trace hashes are retained under `/home/wawow/sliver-kernel-diagnosis/20260911/phase-*`. No raw kernel pointers are reproduced here.

## Safe additional field: existing FE status CPU copy

The exact loaded-module disassembly was independently reviewed: at `adp_fe_irq+0x2c`, `x2` holds the value already loaded from `ADP_INT_STATUS` (FE offset `0x34`), after its load barrier and before the bit-0 branch. A kprobe fetching `status=%x2:u32` adds **no MMIO access**. This is not a probe of an unknown BE register.

The narrow question is whether other FE status bits are co-observed with bit 0, especially at rejected versus accepted gate samples. Co-occurrence would not identify an acknowledgement protocol, flag-assertion time, or safe DMA retirement. All-`0x1` samples would not rule out a separate/masked BE source.

A local copy of the guarded capture tool adds that CPU-value probe, verifies all-online-CPU coverage, checks tiny-dfr's sole ADP-node ownership before and after, preserves failed partial captures, and verifies removal of its uniquely named probes and private instance. Exact kernel, module hash, loaded build ID and BTF checks remain mandatory. No tracefs global reset, extra device opener, synthetic input, service change, package change, or display setting change is involved.

### Preserved empty baseline

Ten-second capture `fe-status-20260911-211403.txt`, SHA256 `40c0a5508399b8fcae9f766532d88372795fef56ceaae4eb6e60476749294598`, contains **zero FE entries/status samples/updates**. It cannot establish any register value. Process exit is 0 (capture/cleanup completed), but the observation gate below correctly exits **1**:

```sh
python3 /home/wawow/sliver-kernel-diagnosis/20260911/analyze-fe-status.py \
  /home/wawow/sliver-kernel-diagnosis/20260911/fe-status-20260911-211403.txt \
  --require-status
# AssertionError: no FE status samples
```

All loss/miss counters are zero; the private instance and probe names are absent afterward. tiny-dfr remains active and the sole owner of the ADP node; Sliver remains inactive. Global hit counts from generic commit-wait probes are not tiny-dfr updates: their trace records are task-filtered.

**Seventeen status-analyzer tests pass**, using the real empty baseline and explicitly synthetic status insertions into the old trace. Synthetic values are parser fixtures, not hardware observations. The analyzer distinguishes empty, status-without-updates and complete-update evidence; validates CPU/IRQ association, pending/event identity, loss/misses and cleanup; and offers `--require-updates` for the next workload capture.

Independent review caught two follow-up tool defects, corrected before the next physical sample: post-capture ownership was checked after closing the raw file, and unrelated CRTCs' global send events could be mistaken for ADP events. New captures persist the required post-ownership result inside the failure-preserving path; analysis now identifies ADP's CRTC from its driver-specific probes before associating generic sends. The original empty baseline is retained unchanged: its separate post-capture ownership check succeeded, but that result is **not encoded in its raw file**, so replay reports `owner_after_recorded: false`. Tests cover both defects, missing post-checks, and wrong ADP layout.

A physical-Fn helper was prepared and passed `bash -n`; shellcheck is unavailable. **That request was withdrawn after the 30 FPS maintainer response.** `/home/wawow/sliver-kernel-diagnosis/20260911/check-fe-status.sh` now prints the superseding explanation and exits without tracing or input requests; the original wizard is retained as `check-fe-status.retired.sh`. The extra-field experiment remains unanswered, not an outstanding requirement for fixing presumed 60 FPS hardware operation.
