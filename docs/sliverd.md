# sliverd internals

Owner: **hazel**. Contract with the agent: `docs/api.md` §6 (cedar). Hardware: `docs/hardware.md` (birch).

## Shape
`crates/sliverd` is a lib + a thin bin.

- `daemon::Daemon` is a **sans-IO state machine**. The event loop calls
  `handle(now_ms, Input) -> Vec<Out>` and arms one timer for `next_deadline()`. There are no
  threads, sleeps or polling in the core, so tests drive it with simulated time.
- `Input`: agent `Connected{client,uid}` / `Packet{client,msg,fd: Attachment}` / `Disconnected`,
  `Touch(TouchEvent)`, `FnKey{held}`, `Activity`, `Lid{closed}`, `SessionChanged{active_uid}`, `Tick`.
- `Out`: `Send(client, DaemonMsg)` / `Close(client)`. Hardware effects go straight to `Backends`.
- `ipc::import_attachment(msg, fds)` turns `sliver_ipc::transport` fds into an `Attachment`
  (`MappedBuffer::import` does the seal and size check).

## Event loop (`server.rs`)
One `poll(2)` over the listener, agent conns, `InputSource`s and the seat watch. The timeout is
`Daemon::next_deadline()`, and with no deadline it blocks (no idle wakeups). At most 64 packets per
conn per wakeup. A conn whose socket won't take a reply is dropped. Received fds go through
`ipc::import_attachment`. Single-threaded.

- **Active uid** (`session.rs`): `/run/systemd/seats/seat0` `ACTIVE_UID=`, watched with inotify (what
  sd-login does internally). No D-Bus and no libsystemd in the privileged daemon.
- **Config** (`config.rs`): `/etc/sliver/sliverd.toml` with `physical_esc` and `media_layer_default`.
  Unknown keys and parse errors are warnings and the defaults apply. The daemon always starts.
- **Socket**: systemd socket activation (`FileDescriptorName=ipc`, via `sliver_hw::listen_fd`), else
  bound at `--socket` (default `/run/sliver/sliverd.sock`, mode 0666).
- **Harness** (docs/safe-swap.md): `bar run target/debug/sliverd --seconds N -- --backend real`. With
  `SLIVER_SAFESWAP=1` the socket defaults to `$SLIVER_OUT_DIR/sliverd.sock`, and sliverd exits on its
  own 2 s before `SLIVER_SAFESWAP_SECONDS`. `--exit-after S` does the same anywhere. The backlight fd
  comes from `OpenFile=` (name `backlight`), so production sliverd needs no root for it either.
- **Debug log**: `-v` or `SLIVER_LOG=debug` logs every input (touch phase/slot/x/y, Fn, lid, seat0 uid),
  agent traffic both ways, state changes (view, display, hatch, driving agent), key emits, flushes (spans
  and ms) and backlight writes, with timestamps. Harness runs check themselves from `sliverd.log`.
- **Dev mode**: `sliverd --fake-hardware [--socket PATH] [--uid N] [--frame-png PATH]` serves the real
  IPC with fake hardware. Point an agent at it, and `--frame-png` is rewritten on every panel flush.
  `--touch-socket PATH` injects input: text lines on a Unix datagram socket (`tap X`, `down X [SLOT]`,
  `move X [SLOT]`, `up [SLOT]`, `cancel [SLOT]`, `fn down|up`, `activity`, `lid open|closed`,
  `screen-backlight`; X in bar pixels), routed through the real touch path: key regions and the key
  fast path, agent touch events, the hatch. E.g. `printf 'tap 80\n' | socat - UNIX-SENDTO:PATH`.
  Format in `crates/sliverd/src/inject.rs`. Refused without `--fake-hardware`.

## System integration (ADR-0005)
`packaging/systemd/sliverd.{service,socket}` and `packaging/udev/99-z-sliverd.rules`. udev starts sliverd
in place of tiny-dfr. It runs with `DynamicUser` (video + input groups, no root), and udev makes the bar
backlight group-writable. Recovery: restart in 0.5 s, then after 5 crashes in 60 s `OnFailure=tiny-dfr`.
The socket conflicts with tiny-dfr. Test: `packaging/itest-units.sh` (user manager, no root).

## Backends (`backend.rs`)
| trait | real impl (birch, `sliver-hw`) | fake (`fake.rs`) |
|---|---|---|
| `Display::flush(&Pixmap, &[Span])`: landscape frame + damaged bar columns | `sliver_hw::panel::DrmPanel` (`real.rs`): rotate via `panel::blit_xrgb8888`, DIRTYFB | real rotated XRGB fb; `frame()` reads it back as PNG |
| `Backlight::set(u32)`, `screen_fraction()` | sysfs fd from `OpenFile=` | records writes |
| `Keys::key(code, down)`, `sync()` | uinput, every key registered up front | records `KeyEvent`s |
| `InputSource` (`AsFd` + `drain() -> Vec<HwEvent>`) | evdev bar digitizer + seat0 (Fn, lid, activity) | `FakeInput` + handle |
| `PixelSource::copy_tight` | `sliver_ipc::shm::MappedBuffer` | `FakeShm` |

sliver-hw stays trait-free. sliverd implements its traits for sliver-hw types.

## Layers
- **Built-in F-key layer**: F1–F12, with Esc first on bars without a physical Esc (≥ 2170 px, or
  `physical_esc = false`), i.e. `DeviceProfile::safe_keys()`. It's the safe layer's base, what the
  **safe hatch** shows, and the fn layer while the agent has none (never presented, or `clear_layer`).
- **Media layer**: tiny-dfr's media row (Esc first on the same bars). It's the safe layer's fn.
  `Config::media_layer_default` swaps base and fn.
- The agent drives once it has presented a `base` frame. Frames are never rejected for missing
  Esc/F-keys (H4 revised). Both built-ins render with `Fonts::embedded()`, the built-in icons and
  `Stylesheet::classic()`. Region ids are item indices, and gaps are split at the midpoint, so there
  are no dead zones.
- **Safe hatch** (invariant 1, `sliver_ipc::hatch::FnHatch`): two Fn key-downs ≤ 400 ms apart, then hold.
  While it's open, the built-in F-key layer is shown and touched whatever the agent does. On open or
  close: `cancel_all`, then `hatch{active}`, then `fn{held}`. Agent presents are kept for later.

## Rules implemented (api.md §6)
- Touch semantics: `sliver_ipc::router::TouchRouter` (shared with cedar's `FakeDaemon`). Any change
  of frame source (agent ⇄ built-in, agent A → B, hatch open/close) and display-off call
  `cancel_all`, which releases every held key.
- Agent loss settle: after the agent's frames vanish (crash, hang, demotion; not the hatch), touch-downs
  are ignored for 200 ms, so a tap aimed at an agent button can't press the safe-layer key that
  appeared under it (seen on the real bar: F10 22 ms after a hangup).
- Pressed feedback: built-ins re-render with the item pressed. Agent frames blit the region from
  `pressed`, or lighten it 25% if there's no pressed image. A region is highlighted only if its id
  **and** rect match the capture.
- Unknown message → error, continue. Malformed packet, hello missing, proto major mismatch, `bye`,
  pong timeout or hangup → close + safe layer. Invalid frame → error, previous frame stays.
- Liveness: ping on display wake (to On) and on an agent-region touch-down if none in 5 s. The
  pong deadline (2 s) is the only liveness timer, and it's armed only while a ping is outstanding.
- Display power: `idle.rs`. "Off" is backlight 0 only: CRTC `ACTIVE=0` does **not** blank the Mac14,7 bar
  (it's a command-mode panel that keeps its own frame; measured in T48, docs/hardware.md). Dims at 30 s, off at 60 s (or per the agent's `display_policy`), and off
  while the lid is closed. Backlight is written only on change. Nothing is flushed while off (it's
  flushed on wake). The wake touch is swallowed. No deadline while off.
- Sessions: only `SessionChanged.active_uid` connections become active. A newer hello from the
  active uid revokes the old connection. Others wait in standby (their presents → `not_active`).
- `emit_keys`: validated (SysRq refused), ≤ 100/s, `down` keys tracked per agent and released when it
  goes. `up` only releases what that agent pressed. These keys are refcounted separately from touch
  keys (a known edge: a finger and the agent both holding Ctrl).

## TestDaemon (`testing.rs`)
`sliverd::testing::TestDaemon` (feature `testing`, so the daemon binary doesn't link it) is the real core in-process on fake hardware, over a real socket pair.
It's API-compatible with the former `sliver_ipc::fake::FakeDaemon` (removed; cedar's suite runs unchanged in
`tests/testing.rs`, and so do the agent's e2e tests with an alias import). Extras: `frame()` (the bar
as a pixmap, incl. pressed feedback and hatch), `advance(ms)` (simulated time), `daemon()`, and
`auto_pong` (default on: it answers the daemon's own pings, set false to test liveness).

## Headless
`cargo run -p sliverd -- --headless out.png [--fn] [--width 2170 --model MacBookPro15,1 --max-brightness 2]`
writes what the panel would show (through the rotated fake framebuffer).
Tests: `crates/sliverd/tests/daemon.rs` (core), `tests/server.rs` (real socket + memfds). Snapshots are in `tests/snapshots/`
(`SLIVER_BLESS=1` to update, then look at them).

## Not yet
- COPR (not now, H46; RPM, units and udev rules are in `packaging/`, ADR-0005). SELinux: clean
  without a module (docs/packaging.md).
