# ADR-0005: sliverd system integration (systemd, udev, tiny-dfr fallback, no root)

Status: accepted · Owner: hazel · Files: `packaging/systemd/`, `packaging/udev/`, `packaging/itest-units.sh`

## Context
GOAL invariant 1: "If the daemon dies, systemd brings back tiny-dfr or sliverd." tiny-dfr is
started by udev (`SYSTEMD_WANTS` on the digitizer), runs `Restart=always`, opens its devices as root
and then drops privileges. A tiny-dfr crash leaves its crash screen up while it still holds DRM
master (docs/hardware.md). We need sliverd to replace it cleanly, coexist with the installed
package, and hand the bar back when it can't run.

## Decision
- **Started like tiny-dfr.** `99-z-sliverd.rules` sorts after `99-touchbar-tiny-dfr.rules` and
  overrides *only* `SYSTEMD_WANTS` on the digitizers (→ `sliverd.service`). tiny-dfr's T2 USB
  config switch and `/dev/tiny_dfr_*` aliases stay, so tiny-dfr can still start as a fallback.
  Removing our rule gives the bar back to tiny-dfr.
- **Exactly one owner.** `sliverd.service` has `Conflicts=tiny-dfr.service` and is ordered
  `After=` it, so tiny-dfr's DRM master is gone before sliverd opens the card. It's `BindsTo=` our
  DRM alias `/dev/sliver_display`.
- **Recovery ladder.** `Restart=always`, `RestartSec=500ms`, `RestartMode=direct` (no trip through
  `failed`), then `StartLimitBurst=5` in 60 s, then `OnFailure=tiny-dfr.service`. A crash costs
  ~0.5 s of bar. A crash loop costs ~3 s, then tiny-dfr. `systemctl reset-failed sliverd &&
  systemctl start sliverd` goes back. The sliverd core itself never hangs holding master: it exits
  and lets systemd act.
- **Socket.** `sliverd.socket` (`/run/sliver/sliverd.sock`, 0666, `FileDescriptorName=ipc`) comes up
  with the service (`Requires=`), not via `sockets.target`, so nothing starts sliverd on a Mac
  without a Touch Bar. It also `Conflicts=tiny-dfr.service`, so a reconnecting agent can't pull
  sliverd back after the fallback. sliverd authorises connections itself (seat0 active uid).
- **No root.** `DynamicUser=yes` with `SupplementaryGroups=video input`: the card is `root:video
  0660`, and the input nodes plus `/dev/uinput` are `root:input 0660` (measured on Mac14,7). The bar
  backlight's `brightness` is root-only, so the udev rule gives it to group `video` (g+w), as
  brightnessctl/light's rules do. Hardening mirrors tiny-dfr's unit (known to allow backlight
  writes) plus `CapabilityBoundingSet=`, a device allowlist, a syscall filter and memory/task caps.
- **Why not `OpenFile=` for the backlight** (what the harness does): its path is per model
  (`228600000.dsi.0`, `228200000.display-pipe.0`, `appletb_backlight`), and `OpenFile=` can't
  take a udev-discovered path without template units keyed on the backlight, while the start
  trigger is the digitizer. sliverd still accepts a passed fd named `backlight` (sliver-hw), so
  either works.

## Consequences
- Any member of `video` can set the bar brightness. That's harmless, and it's the same trust as the DRM card.
- Verified without root or hardware: `udevadm verify`, `systemd-analyze verify` (in `scripts/check`
  when available), and `packaging/itest-units.sh`. The itest runs the real `[Unit]` sections under
  the user manager: crash loop → start limit → tiny-dfr, no early fallback, preemption both ways, and
  no reactivation via the socket after the fallback.
- Installing the files is a `sudo` ask. The first boot with them is a `hardware` ask (watch the
  bar, confirm `systemctl status sliverd`, and check that the fallback works with
  `systemctl kill -s SEGV` ×6).
- SELinux: sliverd runs as `unconfined_service_t` like tiny-dfr. A policy module is packaging work later.
