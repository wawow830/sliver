# Architecture and hardware notes

## Components

The public interface is the `sliver` executable. Its only configuration
language is Lua v1; the embedded default is ordinary Lua source.

The `sliver-broker` system service owns DRM, evdev, uinput, and backlight. It
runs the embedded default before login and accepts one authenticated active
user supervisor over a private Unix socket. The broker never accepts config
selection or Lua source from that socket.

The `sliver-supervisor` user service owns that user's selected path and Lua
worker. It accepts `sliver FILE` and the no-argument default request on the
user's private runtime socket, then forwards only normalized hardware requests
to the broker. The real adapter polls nonblocking evdev descriptors and emits
normalized input events through the hardware seam. The broker coordinates:

- the embedded default before login and its logout restart
- DRM scanout and dirty-framebuffer updates
- normalized hardware events from the adapter
- uinput function-key output
- worker handoff without a blank frame
- backlight and hardware release

Each `sliver-lua-worker` is disposable and unprivileged with respect to
hardware. Its complete frames cross a bounded shared-memory frame interface;
input and output requests cross private control sockets.

## Display path

On the tested Mac14,7:

```text
/dev/dri/cardN
  driver: adp
  connector: DSI-1
  mode: 60x2008
```

The adapter scans the numbered DRM cards and selects the one with a connected
DSI connector, rather than relying on card numbering.

The physical strip is landscape, while the panel scanout is portrait. The
logical renderer draws in 2008x60 coordinates and cairo transforms it into the
native buffer:

```text
logical (x, y) -> buffer (60-y, x)
```

The tested panel has these established mapping properties:

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

The touch digitizer is exposed through a udev-tagged touchscreen event device.
The real adapter selects the local-seat device from udev properties and its
absolute-axis capabilities, opens it in nonblocking mode, and polls it without
handing touches to the compositor. It normalizes raw coordinates and lifecycle
data into logical 2008x60 `TouchEvent` values, then emits them through the
hardware seam. The supervisor forwards those normalized events without adding
layout or gesture policy. The adapter also subscribes to login1
`PrepareForSleep` and reports precise input and display capability loss.

## Fn and virtual keyboard path

The internal keyboard is selected from its local-seat udev identity and
`KEY_FN` plus modifier capabilities. Sliver observes it without grabbing it.
`KEY_FN` press activates the generated F1–F12 layout; release restores the
custom config.

One persistent uinput device named `Sliver Keyboard` advertises:

- F1–F12
- left/right Ctrl
- left/right Alt
- left/right Shift
- left/right Meta/Super
- Escape and consumer media, brightness, mute, and volume keys

Lua key taps and the fixed function row use this device. A continuous physical
Fn hold for exactly two seconds switches a healthy worker to that fixed row.
This physical-hold timer is separate from the Lua callback watchdog, which also
allows two seconds. Modifiers physically held on the Apple keyboard are mirrored
around a key event when inherited. This is required because Hyprland tracks
modifiers per keyboard; without bridging, physical Ctrl+Alt and virtual F2 are
not interpreted as one Ctrl+Alt+F2 chord.

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
write marks synthetic output unavailable, releases the device claim, and waits
for the full contract to be discovered again. This path does not retry,
recreate, or roll back a partially written device.

## Live-apply socket

The user supervisor listens at:

```text
$XDG_RUNTIME_DIR/sliver/supervisor.sock
```

The supervisor-to-broker socket is an internal deployment detail:

```text
/run/sliver/broker.sock
```

The supervisor protocol is a length-prefixed Unix stream. A request contains
one tagged absolute path or the embedded-default tag. The client shuts down its
write side and reads a status byte followed by an error message when the apply
fails. The broker protocol is private to the two Sliver services and carries
normalized events, complete frames, backlight requests, and generic key output.
A missing display, touch input, Fn observer, synthetic output device, or
backlight is reported by name; applies are rejected until the complete contract
is available again. The broker retries discovery without replacing a healthy
Lua worker.

The supervisor parses and swaps a candidate atomically from its own thread. If
Fn is held during an apply, the new config is stored immediately and becomes
visible when Fn is released. Suspend sends visibility loss to the current
worker, cancels contacts, pauses timers and rendering, releases synthetic keys,
and turns off the backlight. Resume shifts timer deadlines by the suspended
interval, restores script brightness, and requests one fresh frame without
replacing a healthy worker. On broker shutdown, the adapter paints a black
frame, turns off the backlight, ungrabs input, and releases DRM ownership.

## Privileges and ownership

Sliver requires:

- read/write access to the DRM primary node
- read access to keyboard evdev
- read/write access to the touch evdev node
- read/write access to `/dev/uinput`

The Fedora package grants those permissions to the `sliver` broker account
through its udev rules and systemd service. Users only need membership in
`sliver-supervisors` to connect their supervisor to the broker. DRM master is
acquired by the first suitable opener; tiny-dfr and Sliver cannot own the panel
simultaneously.

Lua configurations are trusted executable content. They run with the active
user's ordinary environment and standard Lua file, process, and module access;
only hardware ownership stays in the broker.

## Lua apply transaction

`sliver FILE` sends an absolute, lexically normalized path to the supervisor.
`sliver` with no path sends a tagged embedded-default selection over the same
socket. The supervisor starts a fresh Lua worker and waits for its first
complete frame before committing the active frame, brightness, and selected
path state. An explicit path writes the active user's state file; the embedded
selection removes that user's state file. After commit, the supervisor asks the
replaced worker to stop. A rejected candidate leaves the active worker, frame,
and selected path unchanged. Sliver does not watch the source or imported files.
Reapplying the same path or selecting the default starts a new worker.

The hardware broker starts the embedded worker when no local session owns the
seat. When logind reports an active local session, it stops that fallback while
leaving its last frame on the panel, stages the selected source for that user,
and presents the new frame only after the candidate commits. A failed handoff
stops the old worker and presents the fixed recovery row. A revoked or
unexpectedly disconnected user is fenced with that row before the next
supervisor can claim the seat, so a command-mode panel never exposes the old
user's retained pixels while a replacement starts. On logout it runs the old
worker's `logout` cleanup and stages the embedded default. The user's selected
path remains available for the next login.

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
FIFO request queue. Session handoff checks logind on every broker poll and
reloads the active user's selected path or the embedded default. Workers run
with the supervisor's systemd user-manager environment. The protocol has no TCP listener or token field and carries no
client environment.

## Lua worker lifecycle

Each production Lua worker runs in the `sliver-lua-worker` executable. The
user supervisor keeps its broker connection and gives the worker one private
control socket. The broker keeps hardware descriptors in its own process. The worker receives source and input data
through bounded packets and returns complete frames and output requests. Frame
pixels never cross the process boundary until a render has finished.

The supervisor owns the Lua callback watchdog. It waits at most two seconds for
startup, render, commit, and drive requests. This watchdog is separate from the
physical two-second Fn recovery hold. The worker sends heartbeats only
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
