# Native performance observation audit

Source-only audit at [`d90dbe39cbd5bee89d3e4f99d38906722f3cac8e`](https://github.com/wawow830/sliver/tree/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e), 2026-09-11. **Descriptive findings, not an accepted performance contract.** No network/device access, execution of benchmarks, or code/test/verifier/threshold changes during this audit.

## Identity and timing through the production path

| Observation point | What the source preserves—and does not establish |
|---|---|
| **Shared frame slots** | Three slots. Successful publication assigns a mapping-local sequence (initialized to zero), pixels and `FrameTiming`. A producer can reclaim READY slots; the consumer selects newest and frees older READY publications. `CompletedFrame` omits the sequence. These are publication identities, not unique-content IDs or scheduled-tick IDs. No drop ledger is returned. [Slots][slots] |
| **Worker response → supervisor** | Process responses carry optional timing; the supervisor-side wrapper takes the newest shared frame, discards its returned timing, and uses response timing. Association is by the synchronous protocol, not an explicit frame ID. `LogicalFrame` contains only geometry/stride/pixels. [Response][response], [conversion][conversion] |
| **Supervisor acceptance** | After successful `present`, `last_presented_time` becomes the frame's **supplied intended time**, not a fresh completion-clock sample. Later `delta` subtracts that intended time from the new drive time. Neither field measures physical presentation latency. [Drive][drive], [effects][effects] |
| **Broker IPC** | `PRESENT` serializes geometry/stride/pixels. Client waits synchronously; server calls hardware `present` and returns an empty success body. No frame ID, intended time, deadline, input-cause ID or completion timestamp crosses this seam. [Payload][payload], [client][client], [server][server] |
| **M2 present** | Rotates/copies into the existing dumb buffer, calls `dirty_framebuffer`, and on first presentation also calls `show`. Success establishes return from these operations—not optical presentation, a per-frame DRM event, or unique image content. The same path can also paint black during release; supervisor error recovery can re-present old pixels. Raw call counts are not automatically workload-frame counts. [M2][m2], [release][release], [effects][effects] |

A slot sequence could distinguish **successful publications within that mapping if observed there**; it cannot retrospectively identify frames in existing broker/DRM records. A pixel hash could distinguish byte content, but identical repeated pictures are not necessarily the same logical frame.

## Requested cadence is not generated cadence

- The native worker runs timers/rendering when handling supervisor **`DRIVE` commands**, not in an autonomous 60-Hz producer loop. The supervisor waits for worker effects and then synchronous presentation before another drive. Presentation backpressure can therefore reduce **generation**, without first producing frames that the slots discard. [Worker loop][loop], [drive][drive], [effects][effects], [client][client]
- Each repeating timer fires at most once per drive. Rescheduling skips elapsed intervals past callback completion; redraw is a boolean and a drive renders at most once after callbacks. Thus `timer.every(1/60, sliver.redraw)` does not prove 60 callbacks, 60 generated frames, or a numbered 60-frame-per-second offer stream. [Timers][timers], [rescheduling][reschedule], [runtime][runtime]
- Render arguments and `FrameTiming` use the drive's incoming `now_seconds`/`delta`, even after input/timer callbacks take time. `next_worker_deadline` is a scheduling wake-up, not a per-frame completion deadline. A failed slot publication retains one pending frame for retry; a later redraw can replace it. Timer skips, redraw coalescing, pending-frame replacement, READY-slot reclamation and consumer stale-frame removal are **different omission stages**, not one measured drop count. [Runtime][runtime], [pending retry][pending], [slots][slots]

## Canonical fixture versus software benchmark

**Native fixture:** [`video-2008x60.lua` generator][fixture] allocates one constant RGBA background, requests periodic redraw, and overlays intended time formatted to **three decimal places**. It has no frame counter, deadline log, input handler or input-dependent rendering. Despite its filename, this is raw-pixel copying plus text—not video decoding. Rounded time is not a guaranteed unique frame identity; an input followed by its next periodic update does not prove causal response.

**Software test:** [`lua_raw_decoded_frames_hold_native_rate_under_broker_contention`][benchmark] instead creates 60 uniform frames whose red channel encodes render number 1–60. Rust requests them **as fast as possible**, supplying `delta=1/60` without pacing or timers. Its [test staging][test-backend] uses an owner thread/in-process slots, not production process/broker IPC; [fake present][fake] records snapshots.

The test checks that 60 producer requests finish within one second, fewer than 60 reach the consumer, the final image contains 60, and maximum sampled latency is at most 150 ms. The consumer sleeps 20 ms; latency is sampled **before fake presentation**, relative to the pre-render request time. Producer elapsed time excludes final consumer draining/joining. It demonstrates bounded software production/latest-frame behavior under this artificial contention—not sustained physical FPS, each frame's deadline, exact omissions by stage, or input latency. These are source assertions, not fresh test results.

## Causal input and measurement limits

- Touch events preserve contact ID and shared `CLOCK_MONOTONIC` time, but M2 samples that time in userspace `emit_slot`, **not from the incoming evdev event timestamp**. Fn/modifier events carry no acquisition timestamp. Render time instead uses the supervisor's own `Instant` origin: direct subtraction of these two exposed clock values is invalid. [Touch capture][touch], [event types][events], [clock][clock], [supervisor clock][origin]
- The supervisor replaces earlier moves for a contact and can discard a new move when its bounded queue is full. A processed batch shares one scheduling `now` (touch events retain their separate `time`); the worker dispatches callbacks before a possible render, but no input identity is attached to that output. Callback delivery alone cannot establish which pixels changed because of which input. [Touch queue][queue], [event drive][event-drive], [runtime][runtime]
- The existing artifact parser computes FPS as `(count−1)/interval` and “misses” from gaps in **supplied frame indices**, not elapsed per-frame deadlines. It checks input/frame timestamp association and latency arithmetic, not visible causality. “Growth” is last latency minus first; it does not bound intermediate spikes. Labels such as `presentation_s` do not identify which runtime observation produced the value. [Artifact arithmetic][artifacts]

**Bottom line:** existing points can identify publication order locally, benchmark content IDs, protocol success, and reported timing. They do not jointly establish an end-to-end unique-frame/deadline/drop/causal-input record. The human-approved contract must distinguish those observation meanings before any instrumentation, fixture, verifier or threshold changes.

[slots]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/frame_slots.rs#L71-L409
[response]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/worker_process.rs#L518-L574
[conversion]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/hardware.rs#L415-L470
[drive]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/supervisor.rs#L1962-L2029
[effects]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/supervisor.rs#L2108-L2189
[client]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/broker_ipc.rs#L194-L206
[payload]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/broker_ipc.rs#L387-L395
[server]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/broker_ipc.rs#L1003-L1007
[m2]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/m2_hardware.rs#L1600-L1647
[release]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/m2_hardware.rs#L1798-L1821
[loop]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/worker_process.rs#L942-L1012
[timers]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/lua_worker.rs#L1163-L1193
[reschedule]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/lua_worker.rs#L1444-L1489
[runtime]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/lua_worker.rs#L1001-L1087
[pending]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/lua_worker.rs#L934-L977
[fixture]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/scripts/verify-release.sh#L622-L642
[benchmark]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/lua_integration_tests.rs#L648-L738
[test-backend]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/lua_worker.rs#L495-L582
[fake]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/hardware.rs#L757-L762
[touch]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/m2_hardware.rs#L717-L735
[events]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/hardware.rs#L327-L413
[clock]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/clock.rs#L1-L15
[origin]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/supervisor.rs#L694-L696
[queue]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/supervisor.rs#L130-L187
[event-drive]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/crates/sliverd/src/supervisor.rs#L1929-L1959
[artifacts]: https://github.com/wawow830/sliver/blob/d90dbe39cbd5bee89d3e4f99d38906722f3cac8e/scripts/verify-release-evidence.sh#L98-L181
