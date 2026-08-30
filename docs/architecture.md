# Architecture and hardware notes

## Components

### sliver-core

Shared library containing:

- serde TOML config types
- widget layout and hit-testing
- cairo/pango rendering
- battery data lookup
- generated F1–F12 layer
- socket-path convention

The daemon and customizer both call the same renderer. A GUI preview is not an
approximation of the strip; it is the same layout and draw path on another
cairo surface.

### sliverd

Long-running hardware owner. The real adapter polls nonblocking evdev
descriptors and emits normalized input events through the hardware seam. The
owner coordinates:

- DRM scanout and dirty-framebuffer updates
- normalized hardware events from the adapter
- uinput function-key output
- once-per-second dynamic redraws
- live TOML application over a Unix socket
- shell actions attached to widgets

### sliver-edit

GTK4/libadwaita editor with shared mutable config state. It provides:

- live shared-renderer preview
- add/select/reorder/delete operations
- property controls
- Save to TOML
- Apply over the Unix socket

GTK signals are reentrant. List refreshes collect model text and release
`RefCell` borrows before selecting rows, because `select_row` emits callbacks
synchronously.

## Display path

On the tested Mac14,7:

```text
/dev/dri/card1
  driver: adp
  connector: DSI-1
  mode: 60x2008
```

The physical strip is landscape, while the panel scanout is portrait. The
logical renderer draws in 2008x60 coordinates and cairo transforms it into the
native buffer:

```text
logical (x, y) -> buffer (60-y, x)
```

A raw `--probe` experiment established:

- buffer rows run left-to-right across the physical strip
- column zero maps to the lower edge
- the visible window is 60 pixels wide

The dumb buffer is requested as 64x2008 for a 256-byte pitch. The driver rounds
the allocation height to 2048; copies must use the mode height (2008), not the
allocation height.

This panel is command-mode DSI. Mapping and modifying the dumb buffer does not
update the glass by itself. Every repaint calls `dirty_framebuffer` for the
visible 60x2008 rectangle.

The panel retains its last frame when scanout stops. A frozen image does not
prove a daemon is still alive.

## Touch path

The touch digitizer is exposed as:

```text
Mac14,7 Touch Bar
/dev/input/event2   # on the tested boot
```

The real adapter opens this evdev node in nonblocking mode and polls it
without handing touches to the compositor. It normalizes raw coordinates and
lifecycle data into logical 2008x60 `TouchEvent` values, then emits them through
the hardware seam. The supervisor forwards those normalized events without
adding layout or gesture policy.

## Fn and virtual keyboard path

The internal keyboard is discovered by evdev name:

```text
Apple MTP keyboard
```

Sliver observes it without grabbing it. `KEY_FN` press activates the generated
F1–F12 layout; release restores the custom config.

One persistent uinput device named `Sliver Keyboard` advertises:

- F1–F12
- left/right Ctrl
- left/right Alt
- left/right Shift
- left/right Meta/Super
- Escape and consumer media, brightness, mute, and volume keys

Lua key taps and the fixed function row use this device. Modifiers physically
held on the Apple keyboard are mirrored around a key event when inherited.
This is required because Hyprland tracks modifiers per keyboard; without
bridging, physical Ctrl+Alt and virtual F2 are not interpreted as one
Ctrl+Alt+F2 chord.

For a held Ctrl+Alt and F2 tap, Sliver emits from one virtual device:

```text
LeftCtrl down
LeftAlt down
F2 down
F2 up
LeftAlt up
LeftCtrl up
```

The hardware seam receives each synthetic sequence as one ordered batch. The
M2 adapter submits that batch once to `VirtualDevice::emit`. A partial device
write is handled as device loss by issue #14. This path does not retry,
recreate, or roll back a partially written device.

## Live-apply socket

Path:

```text
$XDG_RUNTIME_DIR/sliver.sock
```

Protocol:

1. Connect with a Unix stream.
2. Write one complete TOML document.
3. Shutdown the stream's write side.
4. Read `ok` or `error: ...`.

The render loop parses and swaps the config atomically from its own thread. If
Fn is held during an apply, the new config is stored immediately and becomes
visible when Fn is released.

## Privileges and ownership

Sliver requires:

- read/write access to the DRM primary node
- read access to keyboard evdev
- read/write access to the touch evdev node
- read/write access to `/dev/uinput`

On this Fedora installation, membership in `video` and `input` provides those
permissions. DRM master is acquired by the first suitable opener; tiny-dfr and
Sliver cannot own the panel simultaneously.

Button/label actions execute with the Sliver user's privileges through
`sh -c`. Configs must therefore be treated as executable content.

## Lua apply transaction

`sliver FILE` sends an absolute, lexically normalized path to the per-user
supervisor. `sliver` with no path sends a tagged embedded-default selection over
the same socket. The supervisor starts a fresh Lua worker and waits for its
first complete frame before committing the active frame, brightness, and
selected-path state. An explicit path writes the state file; the embedded
selection removes it. After commit, the supervisor asks the replaced worker to
stop. A rejected candidate leaves the active worker, frame, and selected path
unchanged. Sliver does not watch the source or imported files. Reapplying the
same path or selecting the default starts a new worker.

The default lives in one `crates/sliverd/src/default.lua` source. The build
embeds its exact bytes and installs no editable copy. An absent state file
selects those bytes. A broken saved path remains selected and enters the fixed
recovery row instead of falling back to the default. Embedded Lua keeps the
ordinary runtime and `sliver.v1`, but receives no source path or default marker.

The transaction covers only state owned by Sliver. Lua runs as trusted user
code while staging. Filesystem writes, child processes, network requests, and
native-module effects happen immediately and cannot be rolled back when an
apply fails.

## Local apply authorization

See [ADR 0001, local apply authorization](adr/0001-local-apply-authorization.md)
for the decision and its transaction limit.

`sliver FILE` and the no-argument reset send only a tagged path-or-default
selection to `$XDG_RUNTIME_DIR/sliver/supervisor.sock`. The supervisor reads
the Unix kernel peer credentials and asks logind for the peer's session. It
accepts a non-root peer only when that peer belongs to the active, local,
non-remote session on its seat. Inactive sessions, SSH sessions, cron and
user-service processes without a qualifying session, and root are rejected.

The supervisor checks the session before staging and again immediately before
commit. A session switch cancels candidates that are staging or waiting in the
FIFO request queue. Workers run with the supervisor's systemd user-manager
environment. The protocol has no TCP listener or token field and carries no
client environment.

## Lua worker lifecycle

Each production Lua worker runs in the `sliver-lua-worker` executable. The
supervisor keeps the hardware descriptors in its own process and gives the
worker one private control socket. The worker receives source and input data
through bounded packets and returns complete frames and output requests. Frame
pixels never cross the process boundary until a render has finished.

The supervisor owns the callback deadline. It waits at most two seconds for
startup, render, commit, and drive requests. The worker sends heartbeats only
while its command loop is idle, so a loop, native call, or process call cannot
extend a callback deadline. Replacement, logout, and shutdown send one stop
reason and allow at most 500 milliseconds. A timed-out or exited worker is
killed without sending another Lua callback.

The worker starts a new process group, sets `PR_SET_PDEATHSIG`, enables the
supervisor's child-subreaper mode, and opens a pidfd. The supervisor kills the
pidfd and process group on failure, then drops the worker. It closes inherited
descriptors before Lua starts and sets `NoNewPrivileges`; device-node and
resource policy comes from the systemd worker policy drop-in.

Input transitions and touch events have a fixed application queue. Move events
coalesce by contact. A full queue rejects the worker instead of allocating an
unbounded backlog. Synthetic key effects have a fixed per-drive limit too.
