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

Long-running hardware owner. Its main loop coordinates:

- DRM scanout and dirty-framebuffer updates
- touch events
- keyboard Fn and modifier events
- uinput function-key output
- once-per-second dynamic redraws
- live TOML application over a Unix socket
- shell actions attached to widgets

Blocking input readers run on small threads and communicate with the render
loop through standard Rust channels.

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

Sliver grabs this evdev node so touches do not become compositor mouse input.
Raw X range 0–23044 is normalized into logical strip X 0–2008 and hit-tested
against widget rectangles.

Current touch handling is single-touch/tap oriented. A future slider widget
will require down/motion/up messages rather than release-time taps alone.

## Fn and virtual keyboard path

The internal keyboard is discovered by evdev name:

```text
Apple MTP keyboard
```

Sliver observes it without grabbing it. `KEY_FN` press activates the generated
F1–F12 layout; release restores the custom config.

A uinput device named `Sliver Function Row` advertises:

- F1–F12
- left/right Ctrl
- left/right Alt
- left/right Shift
- left/right Meta/Super

Modifiers physically held on the Apple keyboard are mirrored around the
function-key event on the virtual keyboard. This is required because Hyprland
tracks modifiers per keyboard; without bridging, physical Ctrl+Alt and virtual
F2 are not interpreted as one Ctrl+Alt+F2 chord.

For a held Ctrl+Alt and F2 tap, Sliver emits from one virtual device:

```text
LeftCtrl down
LeftAlt down
F2 down
F2 up
LeftAlt up
LeftCtrl up
```

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
