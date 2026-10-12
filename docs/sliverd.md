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

## Backends (`backend.rs`)
| trait | real impl (birch, `sliver-hw`) | fake (`fake.rs`) |
|---|---|---|
| `Display::flush(&Pixmap, &[Span])`: landscape frame + damaged bar columns | DRM dumb buffer, rotate via `panel::blit_xrgb8888`, DIRTYFB | real rotated XRGB fb; `frame()` reads it back as PNG |
| `Backlight::set(u32)`, `screen_fraction()` | sysfs fd from `OpenFile=` | records writes |
| `Keys::key(code, down)`, `sync()` | uinput, every key registered up front | records `KeyEvent`s |
| `PixelSource::copy_tight` | `sliver_ipc::shm::MappedBuffer` | `FakeShm` |

sliver-hw stays trait-free. sliverd implements its traits for sliver-hw types.

## Layers
- **Safe layer** (no agent driving): base = Esc + F1–F12, fn = tiny-dfr's media row
  (swapped by `Config::media_layer_default`). Esc leads the media row only on bars without
  a physical Esc (≥ 2170 px, or `physical_esc = false`).
- **Built-in fn layer** (agent driving but its fn layer is missing, cleared or failed the floor):
  Esc + F1–F12.
- The agent drives once it has presented a `base` frame. Both built-ins are rendered with
  `Fonts::embedded()` and built-in icons. Region ids are item indices, and gaps are split at the
  midpoint, so there are no dead zones.

## Rules implemented (api.md §6)
- Touch semantics: `sliver_ipc::router::TouchRouter` (shared with cedar's `FakeDaemon`). Any change
  of frame source (agent ⇄ built-in, agent A → B) and display-off call `cancel_all`, which
  releases every held key.
- Pressed feedback: built-ins re-render with the item pressed. Agent frames blit the region from
  `pressed`, or lighten it 25% if there's no pressed image. A region is highlighted only if its id
  **and** rect match the capture.
- Unknown message → error, continue. Malformed packet, hello missing, proto major mismatch, `bye`,
  pong timeout or hangup → close + safe layer. Invalid frame → error, previous frame stays. Floor
  failure → frame accepted, fn := built-in, `escape_floor` error.
- Liveness: ping on display wake (to On) and on an agent-region touch-down if none in 5 s. The
  pong deadline (2 s) is the only liveness timer, and it's armed only while a ping is outstanding.
- Display power: `idle.rs`. Dims at 30 s, off at 60 s (or per the agent's `display_policy`), and off
  while the lid is closed. Backlight is written only on change. Nothing is flushed while off (it's
  flushed on wake). The wake touch is swallowed. No deadline while off.
- Sessions: only `SessionChanged.active_uid` connections become active. A newer hello from the
  active uid revokes the old connection. Others wait in standby (their presents → `not_active`).
- `emit_keys`: validated (SysRq refused), ≤ 100/s, `down` keys tracked per agent and released when it
  goes. `up` only releases what that agent pressed. These keys are refcounted separately from touch
  keys (a known edge: a finger and the agent both holding Ctrl).

## Headless
`cargo run -p sliverd -- --headless out.png [--fn] [--width 2170 --model MacBookPro15,1 --max-brightness 2]`
writes what the panel would show (through the rotated fake framebuffer).
Tests: `crates/sliverd/tests/daemon.rs`. Snapshots are in `tests/snapshots/`
(`SLIVER_BLESS=1` to update, then look at them).

## Not yet
- Real event loop (epoll over listener/conns/input fd + timerfd) and real backends (birch, T-next).
- `/etc/sliver/sliverd.toml` parsing (`Config` is ready).
- Fn double-press-hold safe hatch (H14, docs/customization.md §9). Waiting for the answer and the §6 update.
- Pixel shift, `follow_screen` reading the main backlight on change (it's read on activity now).
