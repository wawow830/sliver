# ADP hardware behavior: primary IRC observations

Date: 2026-09-11. Related: [Sliver #23](https://github.com/wawow830/sliver/issues/23), [earlier completion research](adp-be-completion-semantics.md).

**Subsequent authoritative response:** the maintainer confirmed intended **30 FPS** operation and closed the upstream issue. See [native-rate resolution](adp-native-rate-resolution.md). The observations below remain historical context, not grounds for a 60 FPS driver fix or further generated upstream reports.

## Boundary and acquisition accounting

**No proprietary binary acquisition or static analysis occurred.** Before the policy review, only an empty `vendor-static/` directory was created and tool availability checked. No IPSW, kernelcache, kext, Apple manifest, or disassembly was downloaded or viewed. No vendor bytes were acquired against the 250 MB allowance.

After the override, the [Asahi copyright/reverse-engineering policy](https://asahilinux.org/copyright/) was read completely. Research switched to public IRC hardware-behavior discussion and PR metadata. No proprietary code/disassembly excerpts or linked binary-analysis pages were read or quoted. This source-research subtask performed no hardware access, vendor-code execution, installs, mounts, privileged commands, or service/kernel/DRM/MMIO changes. It is **not** a vendor-code analysis report; the separate [phase investigation](adp-completion-phase-analysis.md) records a separate bounded passive trace.

## Useful new findings

The IRC record supplies more than an unfinished TODO: the original author describes **FIFO-triggered versus free-running scanout**, and tentatively identifies the **exact `0x2a13` value** observed in the current passive capture. It still does not establish a BE completion/acknowledgement protocol or safe DMA retirement.

Times below are UTC. `ChaosPrincess`/`chaos_princess` is the driver's original author in these discussions; Janne and marcan supply independent testing and review context. Citations point to individual primary IRC messages, not search summaries.

### 1. Queued register writes apply at the next vblank

On **2023-04-06, 18:02**, after reporting working Touch Bar playback, ChaosPrincess explains:

> like, there is a fifo where you can queue all register writes and they auto-apply at next vblank

[Message #32044874](https://oftc.catirclogs.org/asahi-dev/2023-04-06#32044874).

This is a direct author report about **register application timing**. It does not say that the associated framebuffer transfer has finished at that vblank. Nor does it prove the timing of every write in today's Linux path, which is not identical to the original experiment's command stream.

The same author's initial playback report says **“several seconds per frame due to usb 2”** ([17:53, #32044851](https://oftc.catirclogs.org/asahi-dev/2023-04-06#32044851)). Together with the later m1n1 example's 0.033-second sleep, this supplies no measured 60-FPS precedent.

### 2. Updating framebuffer memory alone does not refresh this operating mode

On **2023-04-14**, ChaosPrincess reports that `atomic_flush` only runs twice despite X taking the display. At **07:53**, Janne explains that Xorg/modesetting updates framebuffer memory without swaps. The author's **07:55** reply is:

> so, like, my hardware doesnt support that. you must do flushes for it to grab the new framebuffer contents.

[Question #32067789](https://oftc.catirclogs.org/asahi-dev/2023-04-14#32067789), [Janne #32067797](https://oftc.catirclogs.org/asahi-dev/2023-04-14#32067797), [author #32067803](https://oftc.catirclogs.org/asahi-dev/2023-04-14#32067803).

At **08:15**, the author identifies `drm_atomic_helper_dirtyfb` as the needed helper ([#32067837](https://oftc.catirclogs.org/asahi-dev/2023-04-14#32067837)). This explains the original dirtyfb/explicit-flush design requirement. It must not be generalized into “this hardware has no continuous-scanout mode”: the later discussion explicitly describes such a mode.

M2 applicability is supported by Janne's **2023-04-15** reports: the display works on M2 at [09:05, #32069906](https://oftc.catirclogs.org/asahi-dev/2023-04-15#32069906), the m1n1 experiments work at [09:21, #32069916](https://oftc.catirclogs.org/asahi-dev/2023-04-15#32069916), and the DRM driver works on M2 at [14:09, #32070271](https://oftc.catirclogs.org/asahi-dev/2023-04-15#32070271). These are functional reports, not completion-lifetime tests.

### 3. Explicit free-run/FIFO-trigger mode description

A later, directly relevant thread was found by following the ADP keyword rather than repeating source history. On **2025-04-16, 09:42**, chaos_princess says:

> but yes, there is a free run mode where it constantly scans out whatever is in the buffer, and writing ADP_CTRL_FIFO_ON turns it off and makes it only scan out once you write ADBE_FIFO_SYNC to the fifo

[Message #34187264](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187264).

**Supported interpretation:** the original author describes a mode switch between continuous framebuffer scanout and explicit FIFO-triggered scanout. `ADP_CTRL_FIFO_ON` and `ADBE_FIFO_SYNC` are named aggregate values here, not newly documented individual control/status bits.

**Limit:** no completion cause, BE status offset, acknowledgement operation, FIFO token association, or last-DMA-read guarantee accompanies this explanation. The statement does not authorize switching modes to bypass the existing synchronization contract.

### 4. The exact observed `0x2a13` has a tentative author interpretation

On **2025-04-16, 09:48**, Janne asks:

> any idea what ADP_CTRL 0x2a13 is? tracing now and see that on some reads

The author replies:

> iirc "scanout in progress"

[Janne #34187273](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187273), [author #34187275](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187275).

This is **attributed, tentative hardware semantics** for precisely the value in our first-FE observation. Preserve the qualifier **“iirc”**: the log is not a per-bit specification, timestamped completion experiment, or guarantee that `0x613` means DMA quiescence. It strengthens the reason not to fabricate completion at the first rejected FE. It cannot distinguish completion later between FE reads from transfer activity lasting nearly until the second FE.

### 5. Status/IRQ behavior has a concrete power-state caveat

The same thread begins at **09:28** with Janne reporting FE IRQs firing with `ADP_INT_STATUS` **consistently zero**; he confirms that the entire register is zero. The author says she has not seen that case before ([#34187247](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187247) through [#34187252](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187252)).

Janne reports `ADP_CTRL=0x0412` and that writing zero does not change it ([09:36, #34187257](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187257)). The author's suggestion that this means free-run is explicitly uncertain: **“maybe? i do not remember exactly”** ([09:39, #34187261](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187261)). Do not promote that value-to-mode mapping into a proven fact.

Saving/restoring `ADP_CTRL` stops the IRQ storm but leaves resume broken ([10:12, #34187299](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187299)). The author warns that surviving power-down requires preserving much more configuration ([10:14–10:15, #34187305](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187305) through [#34187314](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187314)). Janne then reports **“works with always on”** ([10:20, #34187324](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187324)) and later clarifies that the domains power off even without his local PM callbacks ([10:31, #34187347](https://oftc.catirclogs.org/asahi-dev/2025-04-16#34187347)).

These are historical troubleshooting observations, not a proposed host change or a specification of why an interrupt remains asserted. They defeat an unconditional assumption that asserted FE IRQ implies nonzero readable FE status. They say nothing about BE acknowledgement bits.

## Older-driver analogues: an explicit contemporary warning

On **2023-04-06, 18:06**, ChaosPrincess reports finding an older S5L iPhone kernel with similar hardware, but says **“a lot of registers changed positions or changed completely”** ([#32044893](https://oftc.catirclogs.org/asahi-dev/2023-04-06#32044893)).

On **2023-04-24, 11:27**, marcan cautions that even though ADP is ostensibly the same hardware as older iPhones, **“I don't think we should pretend we know that for sure at this point”**, recommending a specific compatible ([#32091013](https://oftc.catirclogs.org/asahi-dev/2023-04-24#32091013)).

Thus old open-source register names/protocols can suggest questions, but their offsets, status meanings, clear semantics and completion boundaries cannot simply be transplanted to M2. No old or proprietary driver code was copied into this note.

## Historical question prepared before the 30 FPS response

> In April 2025, `0x2a13` was tentatively identified as “scanout in progress”, and `ADP_CTRL_FIFO_ON`/`ADBE_FIFO_SYNC` were described as selecting explicit FIFO-triggered scanout. Does any retained hardware trace/documentation establish which transition ends the framebuffer's last DMA read? How does that transition relate to the reported next-vblank application of queued register writes, the driver's `0x600` masked FE gate, and the intended BE-flush interrupt? Is there a documented BE cause/status/acknowledgement protocol and command-to-completion association, including disable/error/power-state behavior?

This is more specific than asking maintainers to rediscover the FIXME. The [binding still labels the BE IRQ function unknown](adp-be-completion-semantics.md), and none of these observations licenses a speculative MMIO probe, an invented completion event, earlier buffer retirement, or a synchronization change. Existing failed performance acceptance remains failed.

## Provenance, coverage and exclusions

- Primary sources: public OFTC IRC archive messages linked above, Asahi policy, and original PR metadata. The old `oftc.irclog.whitequark.org` endpoint redirects to `oftc.catirclogs.org`. `logs.asahilinux.org` and `logs.penz.dev` did not resolve in this environment.
- Original-period coverage: April 1–14 `#asahi-re`/`#asahi-dev`; April 15–30 `#asahi`, `#asahi-dev`, `#asahi-re`; targeted public search results led to the directly relevant April 16, 2025 thread. The original [m1n1 PR299](https://github.com/AsahiLinux/m1n1/pull/299) opened April 9 and merged April 15, 2023; [Linux PR137](https://github.com/AsahiLinux/linux/pull/137) opened April 18. Working driver reports therefore precede the Linux PR opening.
- Before selected messages were displayed, programmatic preflight screened pages for instruction/operand patterns and decompiler-like constructs. Entire April 3 and April 28, 2023 `#asahi-dev` pages, and the broad `#asahi-dev` FIFO-search page, were excluded after possible-snippet flags. Flagged text was not displayed; linked code/disassembly pages were not followed. Screening is heuristic, not a formal provenance audit.
- The April 23 `#asahi-re` discussion provides independently reported flip/rotation experiments, but no additional completion evidence; it is not reproduced here. Unrelated SPI/touchpad, SMC and ATC FIFO conversations were not treated as ADP evidence.
- Research HTTP responses were public text handled in memory. The background agent's factual IRC summary is `/home/wawow/sliver-kernel-diagnosis/20260911/vendor-static/irc-notes-agent.md`; it contains no vendor binaries or proprietary code excerpts. This Markdown report is the only repository file created for this route.
