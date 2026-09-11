# Upstream report: ADP flip completion trails dirtyfb return by one FE

Status: submitted as [AsahiLinux/linux#613](https://github.com/AsahiLinux/linux/issues/613) on 2026-09-11, using a condensed version without raw kernel addresses, then **closed as not actionable**. The maintainer confirmed the panel is intended to run at **30 FPS** and requested no further LLM-authored issues. See [native-rate resolution](adp-native-rate-resolution.md). The original report below is preserved, not an outstanding driver-fix request. Sliver’s conflicting native 60 FPS acceptance requirement remains unresolved in [#23](https://github.com/wawow830/sliver/issues/23); no failed release evidence was relabeled accepted.

## Reporting destination

The [installed-source MAINTAINERS entry](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/MAINTAINERS#L8707-L8721) lists the Asahi Linux issue tracker, `asahi@lists.linux.dev` and `dri-devel@lists.freedesktop.org`, with Sasha Finkelstein as maintainer and Janne Grunau as reviewer. The report was filed in the listed Asahi Linux issue tracker; no email was sent.

## Environment and symptom

- Apple Mac14,7 Touch Bar, Fedora Asahi, kernel `7.1.13-402.asahi.fc44.aarch64+16k`.
- Exact package archive commit: `81d3924095fd017e473332a9b6dd6dd0e3d9a59b`. Relevant selected files are unchanged by the SRPM patches; installed and loaded ADP module build IDs match.
- Sliver at `57dce93` uses blocking `DRM_IOCTL_MODE_DIRTYFB` with a native 2008×60 logical video workload.
- 35-second syscall-only repeat: 1038 successful dirtyfb completions over 34.955785 seconds, **29.666048 completions/s**. Median syscall duration 26.3075 ms; median completion gap 33.709 ms. No trace loss.
- Full trace: all 1037 between-flush intervals span exactly two consecutive FE vblanks. Median dependency wait 8.423375 ms and post-programming vblank wait 16.850680 ms in a separate five-second function-graph capture. Plane register programming takes microseconds.

These are kernel update/completion rates, **not optical FPS**. No causal input latency or independently numbered presented workload frames were measured. The full and syscall-only captures agree on approximately half-rate updates; no driver changes were involved.

## Stronger passive observation under tiny-dfr

A separate 30-second capture while physically switching Fn layers in unmodified tiny-dfr observed 15 complete flush/event cycles. All 15 showed:

1. Flush queues a particular pending event/completion.
2. First FE: `ADP_CTRL = 0x2a13`, so `(ctrl & 0xf00) == 0xa00`; the driver's event gate rejects it.
3. Blocking dirtyfb returns successfully.
4. Second FE: `ADP_CTRL = 0x613`, mask `0x600`; the **same event/completion** is delivered.

Median first-control observation to event delivery: **16.831 ms**. Running BTF and loaded-module identity were checked for probe offsets; raw event/commit identities were matched, rather than correlating only timestamps. No incomplete cycles, ring loss, or kprobe misses; diagnostic probes and instances were removed afterward.

One representative cycle, CLOCK_MONOTONIC seconds:

| Event | Time |
| --- | ---: |
| dirtyfb entry | 11589.871238 |
| atomic flush | 11589.871301 |
| first control read, gate rejects | 11589.887206 |
| successful dirtyfb return | 11589.887237 |
| second control read, gate accepts | 11589.904024 |
| same pending event delivered | 11589.904027 |

The deliberately spaced tiny-dfr updates produced **no dependency waits overlapping delayed event delivery**. Thus this second workload independently establishes the late event, not continuous half-rate throughput. The earlier Sliver workload provides the continuous-throughput observation.

## Source path involved

At the exact installed archive commit:

- [ADP atomic flush and FE handler](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/drivers/gpu/drm/adp/adp_drv.c): flush queues `ADBE_FIFO_SYNC | 1`, with `FIXME: use adbe flush interrupt`; probe obtains a BE IRQ but bind installs only the FE handler. FE counts vblank before testing the `0x600` control gate and delivering the pending event.
- [Generic atomic helper](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/drivers/gpu/drm/drm_atomic_helper.c): blocking dirtyfb's default commit tail waits for dependencies, programs the planes, marks hardware done, then waits for a counted vblank. A counted vblank need not imply the ADP event has completed. `wait_for_flip_done` is a documented alternative, not evidence that changing waits alone removes the ADP gate.
- [Atomic commit dependency wait](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/drivers/gpu/drm/drm_atomic.c): waits for the old commit's hardware and flip completions.

[Upstream ADP at 08df884136f1c1197bab2a27814404fd329d9aac](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/drivers/gpu/drm/adp/adp_drv.c) retains this gate and BE FIXME. Differences from the installed driver inspected here are atomic API signature and connector helper changes, not a completion fix.

## Questions for maintainers

1. What does the `0xa00 → 0x600` FE state transition guarantee: FIFO consumption, scanout/DMA retirement, MIPI transfer completion, or another boundary?
2. Is the hardware actually busy for two FE intervals, or does it complete between FE observations and only get recognized at the next FE?
3. Are the ADBE flush interrupt's status/mask/acknowledgement and FIFO sequence semantics documented or captured from the vendor driver? Which event guarantees that an old DMA buffer may safely be released or reused?
4. The [device-tree binding](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/Documentation/devicetree/bindings/display/apple,h7-display-pipe.yaml#L42-L50) describes BE IRQ as **“Unknown function”**. What original observation motivated the BE-flush FIXME? Are there relevant error, power-management, or shutdown constraints?
5. Would a bounded observation of an existing safe status read distinguish these cases without adding MMIO accesses, enabling an undocumented IRQ, or perturbing the transfer?

We have **not** removed the control gate, fabricated flip completion, changed synchronization, reused buffers earlier, installed a modified kernel, or adjusted acceptance thresholds. The data do not justify such changes.

## Evidence handoff

Raw captures remain local and were not attached to the upstream report:

- `/home/wawow/sliver-release-verification/20260911-170920/`: continuous throughput and dependency/vblank capture, analyzers and original workload.
- `/home/wawow/sliver-kernel-diagnosis/20260911/`: exact-source audit, BTF/module checks, passive completion capture, parser tests and cleanup records.
- Passive raw capture `completion-20260911-203257.txt`, SHA256 `ed6a846ce2aa451b5b4c6449167a2e21e898c65c0823eed7a7722f82bd84e203`.

Before uploading, inspect/redact raw process identifiers, kernel pointers and machine-local metadata; preserve stable anonymized identities for event/commit correlation. Retain originals and record the redacted derivative's hash. Failed release evidence and subsequent rollback repair are not retroactively marked accepted.
