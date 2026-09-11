# ADP/ADBE completion semantics

Research date: 2026-09-11. Related: [Sliver #23](https://github.com/wawow830/sliver/issues/23).

**Superseding response:** the ADP maintainer confirmed intended **30 FPS** operation and closed the upstream report as not actionable. See [native-rate resolution](adp-native-rate-resolution.md). Missing BE semantics are no longer a reason to pursue a presumed 60 FPS panel fix; the local release target needs an explicit decision.

## Conclusion

**No independently supported BE completion/status/mask/acknowledgement register semantics were found in the examined public sources.** Linux names and uses FE registers, acquires a separate BE IRQ, and retains the BE-flush FIXME. Its binding explicitly calls the BE IRQ's function **unknown**. m1n1's playback example sleeps **0.033 seconds** without waiting for completion; it is not a measured 60-FPS precedent. Sources and limits follow below.

This leaves the key discriminator unresolved: genuinely unfinished transfer versus completion between FE observations that is only noticed at the following FE. Neither the FIXME nor the demo establishes a safe earlier DMA-buffer retirement boundary. The maintainer question below records the historical handoff. It is **not a current request for further probes, kernel patches, or generated upstream messages**.

Boundary: read `COMPLETION-RESULT.md` and `SOURCE-FINDINGS.md` in `/home/wawow/sliver-kernel-diagnosis/20260911/`. Their 15/15 tiny-dfr cycles show the FE gate delaying the same event beyond successful dirtyfb return, but no overlapping next-commit dependency wait in that slow workload. The separate archived Sliver throughput remains approximately 29.67 updates/s and unaccepted; neither observation measures optical presentation. This research used remote sources and `/tmp` downloads only: no root, tracing, services, packages, hardware access, execution of downloaded code, or external report submission.

## m1n1 evidence

### Primary implementation evidence

1. **Sasha Finkelstein's original experiments**, commit [`5681036b2ede62667a451c28267cadfbba8f9f71`](https://github.com/AsahiLinux/m1n1/commit/5681036b2ede62667a451c28267cadfbba8f9f71) (April 2023), introduce `touchbar_rainbow.py` and `touchbar_bad_apple.py`. They submit display commands but contain no BE/FE interrupt handling, status polling, or acknowledgement sequence.
2. **Janne Grunau's playback optimization**, commit [`22890f3ba1dfcc1d3f608a6a36dc7c532471ba69`](https://github.com/AsahiLinux/m1n1/commit/22890f3ba1dfcc1d3f608a6a36dc7c532471ba69), explicitly says “Do not allocate a framebuffer for every frame.” The [resulting loop, lines 82–93](https://github.com/AsahiLinux/m1n1/blob/22890f3ba1dfcc1d3f608a6a36dc7c532471ba69/proxyclient/experiments/touchbar_bad_apple.py#L82-L93) allocates/maps once, overwrites the same buffer, submits commands, then sleeps **0.033 seconds**. This is an actual reuse pattern, **not proof that reuse is safe after any particular completion bit or IRQ**. It neither waits for completion nor unmaps/frees the framebuffer.
3. Hector Martin's [rainbow improvement, `a0f01809cea492515595451840feada3a7454b39`](https://github.com/AsahiLinux/m1n1/commit/a0f01809cea492515595451840feada3a7454b39), changes the pattern, not synchronization. Both experiment files remain unchanged at examined upstream HEAD `b4654b32941d51afdb77579d63e7cb1aa6c03ecc`.

### Exact writes versus semantics

All citations below use Janne's pinned playback version, not a moving branch.

| Location/value | What the source actually establishes |
|---|---|
| [`0x228400100 ← 0x613`, lines 37–38](https://github.com/AsahiLinux/m1n1/blob/22890f3ba1dfcc1d3f608a6a36dc7c532471ba69/proxyclient/experiments/touchbar_bad_apple.py#L37-L38) | Comment: “enable fifo and vblank”. Raw constant only; no per-bit names, readback, mask test, or clear operation. Numerically bits **0, 1, 4, 9, 10** are set; assigning those bits individual meanings requires another source. |
| [`0x2282010c0`, lines 68–80](https://github.com/AsahiLinux/m1n1/blob/22890f3ba1dfcc1d3f608a6a36dc7c532471ba69/proxyclient/experiments/touchbar_bad_apple.py#L68-L80) | Writes command header `0xc0000001 \| (len(pipe) << 16)` and stride/address command words. Function named `flush` only loops over writes; it does **not** wait for hardware completion. |

These are literal absolute addresses. Relative offsets would be `0x100` from `0x228400000` and `0x10c0` from `0x228200000`, respectively, **by arithmetic**; these scripts do not themselves define/name those register-bank bases. They also do not define `FE_CTRL` bit fields. Do not mistake the raw `0x613` initialization for independent validation of named Linux FE_CTRL fields, the `(ctrl & 0xf00) == 0x600` completion predicate, or BE IRQ semantics.

### Still unknown from this evidence

- **BE before FE?** No timestamped BE/FE observations, relative-order guarantee, or first-FE completion test.
- **W1C versus ordinary RW?** No discriminating write/read experiment or acknowledgement implementation. The single initialization write cannot distinguish ordinary RW, W1C, mixed control/status fields, or ignored bits.
- **BE status/mask/ack offsets and bits?** None established. No candidate BE bits to recommend from m1n1.
- **Completion versus quiescence?** No definition of transfer-complete, FIFO-empty, engine-idle, or last DMA read; no teardown/unmap lifetime proof. Fixed-delay demo reuse cannot justify early DRM flip completion or buffer retirement.

### Search coverage

Examined upstream m1n1 HEAD and all fetched branch histories; Sasha's `WhatAmISupposedToPutHere/m1n1` branches; Janne's `jannau/m1n1` branches (including `experiments_touchbar_opts`, `j493-touchbar`, and `touchbar_non_fatal_errors`); original experiment/optimization PRs [299](https://github.com/AsahiLinux/m1n1/pull/299) and [304](https://github.com/AsahiLinux/m1n1/pull/304). No additional completion implementation emerged. Searched Asahi docs at [`715664a269937fe83293f46bbbeaae6094cb504e`](https://github.com/AsahiLinux/docs/tree/715664a269937fe83293f46bbbeaae6094cb504e) and legacy wiki checkout `2f215d80cb89284a5a5948f1482f971441f501ed`; no ADP register/ack specification found. These are bounded negative findings, not a claim that unpublished reverse-engineering notes do not exist.

## Linux registers and IRQ routing: supported facts, not a BE specification

The following references pin current upstream to [`08df884136f1c1197bab2a27814404fd329d9aac`](https://github.com/torvalds/linux/tree/08df884136f1c1197bab2a27814404fd329d9aac). This is an upstream comparison, not a claim that this revision is installed.

| Register or route | Source-supported behavior | Limit |
|---|---|---|
| **FE + `0x34`**, `ADP_INT_STATUS`; `VBLANK=0x1`, `INT_MASK=0x7` | FE handler tests bit 0, counts vblank, and finally writes the entire observed status back. Vblank enable/disable writes `0x7` to this register. | This is the driver's **W1C-style acknowledgement usage**, not an independently tested specification of every bit. `INT_MASK` here is a value written to the status register, not evidence of a separate interrupt-mask register. No BE acknowledgement follows from it. |
| **FE + `0x100`**, `ADP_CTRL`; `VBLANK_ON=0x12`, `FIFO_ON=0x601` | Named **aggregate constants**, respectively bits 1/4 and bits 0/9/10. The pending-event gate tests `(ctrl & 0xf00) == 0x600`. | No individually named busy/transfer-done bits or state enum. Do not rename the `0xa00` and `0x600` patterns “DMA busy” and “DMA complete” as established hardware facts. |
| **BE + `0x10c0`**, `ADBE_FIFO`; `ADBE_FIFO_SYNC=0xc0000000` | Flush writes `0xc0000001`, immediately followed in source by `FIXME: use adbe flush interrupt`. | A command submission, not an observed completion, interrupt-enable bit, acknowledgement, or documented sequence-number contract. |
| Separate **`be`** and **`fe`** IRQ resources | Driver obtains both but requests only `adp_fe_irq`. | Acquiring an IRQ number does not establish its event causes or how to clear them. |

Sources: [register definitions](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/drivers/gpu/drm/adp/adp_drv.c#L23-L34), [FE enable/disable](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/drivers/gpu/drm/adp/adp_drv.c#L244-L280), [flush](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/drivers/gpu/drm/adp/adp_drv.c#L308-L350), [IRQ acquisition, handling, registration](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/drivers/gpu/drm/adp/adp_drv.c#L470-L533).

**Arithmetic only:** observed `0x2a13` has bits 0/1/4/9/11/13 set; `0x613` has 0/1/4/9/10 set. Under `0xf00`, those are `0xa00` and `0x600`. Bit 13 also changes but is outside the gate. This decoding assigns no additional hardware meaning to the bits.

The [binding](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/Documentation/devicetree/bindings/display/apple,h7-display-pipe.yaml#L24-L51) describes BE as the primary planes/blending register bank, FE as other configuration including interrupt/FIFO control, and—critically—the two IRQs as **“Unknown function”** and **“Primary interrupt. Vsync events are reported via it”**, respectively. The [M2/t8112 device tree](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/arch/arm64/boot/dts/apple/t8112.dtsi#L452-L463) declares BE base `0x228200000`, FE base `0x228400000`, BE AIC IRQ **614**, FE AIC IRQ **618**, both **`IRQ_TYPE_LEVEL_HIGH`**. The binding's M1 example instead uses IRQs **502/506**. These are SoC interrupt specifiers, not Linux virtual IRQ numbers or completion-bit definitions. Level-high routing alone supplies neither the peripheral acknowledgement protocol nor DMA-lifetime guarantees.

## Original history and submission discussions

- The older Asahi out-of-tree addition [`a0472061a4a37e3c6711fc1aa3e896bcbe47802a`](https://github.com/AsahiLinux/linux/commit/a0472061a4a37e3c6711fc1aa3e896bcbe47802a), reachable from tag `asahi-6.6-1`, already contains the same gate and FIXME. Author date: **2023-04-18**, Sasha Finkelstein; signed by Sasha and Janne Grunau. Its message supplies no BE protocol. This dates the TODO earlier than mainline submission; it does not explain it.
- The public [v1–v8 series](https://patchwork.freedesktop.org/series/141733/) runs from **2024-11-24 to 2025-02-24**. The [v1 driver patch and discussion](https://patchwork.freedesktop.org/patch/msgid/20241124-adpdrm-v1-2-3191d8e6e49a@gmail.com) already contain that gate/FIXME. No BE status/mask/ack explanation was found in the reviewed driver-patch comments for any of v1–v8. The [v2 binding discussion](https://patchwork.freedesktop.org/patch/msgid/20241126-adpdrm-v2-1-c90485336c09@gmail.com) requests descriptions for the cryptic `be`/`fe` names, rather than supplying completion semantics.
- Relevant actual discussion: Hector Martin explains in v1 that the bootloader preinitializes the one hardwired display mode and that unexercised features of this reverse-engineered hardware lack a generic specification. This contextual limitation is **not** itself a statement about BE completion. The [v4 discussion](https://patchwork.freedesktop.org/patch/msgid/20250114-adpdrm-v4-2-e9b5260a39f1@gmail.com) addresses device/resource lifetimes and vblank shutdown, not the last framebuffer DMA read. Do not conflate those lifetimes.
- Mainline addition [`332122eba628d537a1b7b96b976079753fd03039`](https://github.com/torvalds/linux/commit/332122eba628d537a1b7b96b976079753fd03039) links the [v8 patch](https://patchwork.freedesktop.org/patch/msgid/20250224-adpdrm-v8-2-cccf96710f0f@gmail.com). The later [April 2025 fixes](https://lore.kernel.org/r/20250428-drm_adp_fixes-v2-1-912e081e55d8@jannau.net) concern event locking, vblank references/enabling, and IRQ-lock removal; the current source still has no BE completion handler.

Coverage: driver patch/comments for all eight revisions, series cover letters, initial binding discussions, original Asahi and current upstream source/history, plus the m1n1/docs coverage above. Lore's direct raw endpoint returned HTTP 403 during this research; the original submitted patches/replies were read via the freedesktop Patchwork archive instead. Search failures and absent results are not proof that private notes or another public trace cannot exist.

## Maintainer question: evidence-backed handoff

Current [PRE-DCP MAINTAINERS entry](https://github.com/torvalds/linux/blob/08df884136f1c1197bab2a27814404fd329d9aac/MAINTAINERS#L8876-L8890): **Sasha Finkelstein `<k@chaosmail.tech>`**, reviewer **Janne Grunau `<j@jannau.net>`**; lists **`asahi@lists.linux.dev`**, **`dri-devel@lists.freedesktop.org`**; bug tracker **[AsahiLinux/linux/issues](https://github.com/AsahiLinux/linux/issues)**. The original submissions use Sasha's older `fnkl.kernel@gmail.com` address; prefer the current maintained contact entry. The [separate report](adp-completion-upstream-report.md) holds local trace details. A condensed version was submitted as [AsahiLinux/linux#613](https://github.com/AsahiLinux/linux/issues/613) on 2026-09-11; raw traces and kernel addresses were not uploaded.

Suggested precise question:

> The ADP driver has retained `FIXME: use adbe flush interrupt` since its 2023 out-of-tree addition, while the binding still describes the BE IRQ function as unknown. On M2, 15 spaced tiny-dfr updates retain the same event across FE reads `0x2a13` then `0x613`, with successful blocking dirtyfb return between them. Separate continuous Sliver updates complete at about 29.67/s. What original reverse-engineering observation motivated the FIXME? Is there a known BE completion cause/status register, enable/mask control, acknowledgement protocol (including W1C/read-clear/mixed fields), and FIFO command-to-completion association? Which condition guarantees the last DMA read of a framebuffer has finished, rather than merely command consumption or a counted FE? Is there existing source or a vendor-driver capture establishing its ordering relative to FE and behavior during disable/error/power transitions?

The [exact-source default DRM tail](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/drivers/gpu/drm/drm_atomic_helper.c#L1969-L2000) calls the counted-vblank wait followed by plane cleanup; it does not wait for the driver's late flip completion there. [`wait_for_flip_done` is documented as an alternative](https://github.com/AsahiLinux/linux/blob/81d3924095fd017e473332a9b6dd6dd0e3d9a59b/drivers/gpu/drm/drm_atomic_helper.c#L1868-L1942), but that does not supply missing hardware semantics. Consequently this note endorses neither bypassing dependencies nor claiming that the existing FE observation proves safe buffer retirement. No register-read schedule, IRQ-enabling sequence, or replacement kernel code is proposed.
