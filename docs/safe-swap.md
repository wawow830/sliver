# Safe-swap harness (real Touch Bar runs)

Owner: birch. Code: `harness/safeswap/`. Upholds GOAL invariants 1 and 2. **This is the only
sanctioned way for a peer to touch the real bar.** Read `docs/hardware.md` first.

## Use (after the human has installed it)
```
cargo build -p sliverd
harness/safeswap/bar run target/debug/sliverd --seconds 30 -- --backend real   # blocks; Ctrl-C ends it
harness/safeswap/bar snap bar.png      # PNG (2008x60) of whatever is on the bar right now
harness/safeswap/bar logs              # sliverd's stdout/stderr from the last run
harness/safeswap/bar status | stop | restore
```
`bar` wraps `pkexec /usr/local/libexec/sliver-safeswap ...`. Limits: 1–300 s per run (default 30),
one run at a time, the payload must be an ELF file (it gets copied to a root-owned staging dir,
so what runs is exactly what was checked), and at most 32 args after `--`.

**First real run of anything new is a `hardware` ask:** tell the human what will appear on the
bar and for how long, and ask them to watch it and confirm tiny-dfr came back. Then look at
`bar snap` yourself.

## Guarantees: how tiny-dfr always comes back
The payload runs in a transient **system** unit `sliver-swap.service`. Restoring is systemd's
job, not the job of a process that might die:

| failure | what restores tiny-dfr |
|---|---|
| sliverd exits / crashes / fails to exec | `ExecStopPost=+sliver-safeswap restore` (runs as root after *any* end) |
| sliverd hangs or ignores the deadline | `RuntimeMaxSec=N`, then SIGTERM, `TimeoutStopSec=5`, SIGKILL of the whole cgroup, then ExecStopPost |
| anything starts tiny-dfr mid-run (udev event, admin) | `Conflicts=tiny-dfr.service`: systemd stops our run first (stop is ordered before start), then tiny-dfr starts |
| the peer that started the run dies (`--wait`, which `bar` always uses) | PDEATHSIG/SIGHUP/SIGINT in the helper → `systemctl stop` → ExecStopPost |
| ExecStopPost somehow fails | dead-man timer `sliver-swap-restore.timer`, armed **before** the run starts, fires at N+25 s |
| reboot / power loss | nothing persistent was changed (no mask, no enable, no config edits), so tiny-dfr starts at boot via its udev rule as usual |
| tiny-dfr's start rate limit | restore does `reset-failed tiny-dfr.service` before `start` |

The `After=tiny-dfr.service` ordering makes sure tiny-dfr has fully exited (DRM master released)
before sliverd starts. Worst case the bar is not ours for N + 5 s (+ 20 s timer slack if even
ExecStopPost fails).

Verified without root or hardware by `harness/safeswap/itest-user.sh`. It runs the same helper
against the *user* systemd manager with a fake tiny-dfr and covers normal exit, crash, timeout
with SIGTERM ignored, preemption, stop, caller death, failed exec, the dead-man timer alone,
idempotent restore and concurrent-run refusal. Re-run it whenever you touch the helper. The
pure logic (unit properties, validation, discovery, snap pixel math) is unit-tested in
`scripts/check`.

## What the payload gets (contract for sliverd, T8)
- Runs as **the invoking user** (never root), `SupplementaryGroups=video input`,
  `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp`,
  `DevicePolicy=closed` with only the bar DRM card, `char-input` and `/dev/uinput` allowed,
  `RestrictAddressFamilies=AF_UNIX AF_NETLINK` (libinput/udev need netlink), 512M RAM, 64 tasks.
  So it has no more privilege than the user already has, minus most of it.
- **DRM:** tiny-dfr is gone, so the first open of the bar card makes sliverd DRM master
  automatically (no CAP_SYS_ADMIN needed). Calling SET_MASTER again is harmless.
- **Bar backlight:** sysfs `brightness` is root-only, so PID 1 opens it and passes the fd via
  `OpenFile=` → `sd_listen_fds`: `LISTEN_FDS=1`, `LISTEN_FDNAMES=backlight`, fd 3, read-write.
  Write `"<n>\n"`. Read `max_brightness` from sysfs yourself (world-readable). **Production sliverd
  should use the same mechanism**, so it never needs root at all (ADR-worthy, T8).
- Env: `SLIVER_SAFESWAP=1`, `SLIVER_SAFESWAP_SECONDS=N` (sliverd should also exit by itself
  before then), `SLIVER_OUT_DIR=/run/sliver-safeswap/out` (the only writable path; put frame
  dumps, PNGs and traces there), `RUST_BACKTRACE=1`.
- stdout/stderr → `$SLIVER_OUT_DIR/sliverd.log`.
- No access to `$HOME`. Pass config via args (or bake test layouts into the binary).

## Snap (screenshot of the real bar)
`sliver-safeswap snap` (root) opens the bar card **without** becoming master: it refuses if
nothing owns the bar, and drops master right away just in case. It reads the CRTC's current
framebuffer via `GETFB2` + `MAP_DUMB` + `mmap` (read-only), un-rotates the portrait scanout into a
2008x60 landscape PNG, and writes it to stdout. It works whether tiny-dfr or sliverd is drawing.
It never modesets. The first use is still a `hardware` ask (it's a new hardware path).

## Install (human, `sudo`)
```
sudo harness/safeswap/install.sh            # --user NAME (default $SUDO_USER), --no-rule
```
It installs:
- `/usr/local/libexec/sliver-safeswap` (root 0755). The helper.
- `/usr/share/polkit-1/actions/org.sliver.safeswap.policy`. A pkexec action for exactly that
  path, `auth_admin_keep` by default.
- `/etc/polkit-1/rules.d/49-sliver-safeswap.rules`. Lets that one user run that one action
  **without a password**, so peers can use it unattended. Skip with `--no-rule` to keep the
  password prompt.

Why a no-password rule is acceptable: the helper can only (a) stop tiny-dfr for ≤300 s with
systemd-guaranteed restore and (b) run a binary *as the calling user* with fewer privileges than
that user already has, plus a backlight fd. `snap` only reads the bar's framebuffer. Test-only
flags (`--user-mode`, `--target`, ...) are rejected when not in user mode, and user mode
refuses root, so the target unit can't be redirected (e.g. to `Conflicts=sshd`).

Remove: `sudo harness/safeswap/uninstall.sh` (restores tiny-dfr, deletes the three files and
`/run/sliver-safeswap`).

## First runs: `sliver-hwprobe` (T23)
`crates/sliver-hw` is real-hardware access (discovery, DRM, evdev, backlight). It will hold the
real sliverd backends. Its `sliver-hwprobe` binary refuses to open any device unless
`SLIVER_SAFESWAP=1` (only `--sysfs-only` works anywhere). Any code that opens bar devices should
call `sliver_hw::require_harness()` the same way. Planned order, each step a `hardware` ask:
```
cargo build -p sliver-hw
harness/safeswap/bar snap tinydfr.png                                      # 1. snap tiny-dfr, check orientation
harness/safeswap/bar run target/debug/sliver-hwprobe --seconds 20          # 2. read-only: caps/planes/props/touch ranges
harness/safeswap/bar run target/debug/sliver-hwprobe --seconds 40 -- --draw 15 --touch 15
                                                                            # 3. test pattern (white block top-left,
                                                                            #    green bottom-right) + raw touches; snap during it
```

## Known gaps / first-run checklist
- SELinux: the staged binary gets `chcon -t bin_t` so PID 1 can exec it from `/run`. Unverified
  on the real system. If the run fails with 203/EXEC, check `ausearch -m avc -ts recent`.
- `snap`'s ioctl struct layouts are unit-tested for size only. Confirm on the first real snap
  that the image isn't mirrored (tiny-dfr's rotation is `translate(60,0)+rotate(90°)`).
- On T2 (`appletbdrm`) the scanout is emulated XRGB8888 and snap should work, but it's untested.
- Systemd `OpenFile=` needs systemd ≥ 253 (Fedora 44 has 259).
