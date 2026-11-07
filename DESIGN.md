# sliver

The Touch Bar is a keyboard with a screen. Touches come in, keys go out.

## The one rule

sliverd always knows what is **shown**:

    shown = nothing          if the bar is dark
          = the background   if Fn is held or no window is mapped
          = the top window   otherwise

The background is Esc and F1–F12, a fixed 2008×60 image. The picture is `shown` composited over the
background (Porter-Duff over). Where a window is transparent or smaller than the bar, the background
shows through. A touch goes to the window if it lands in the window's input region (the Wayland
rule), and to the background otherwise.

`shown` is an identity, not pixels. A commit from the shown window changes the picture, not `shown`.
Everything else follows:

- **Drawing.** The panel is redrawn when `shown` changes, when the shown window commits, and on
  wake.
- **Frame callbacks** go out when a frame reaches the panel. A hidden window or a dark bar gets
  none. Commits from hidden windows cost a snapshot and no panel work.
- **Touches** go to whatever is under the finger when it lands, and stay with that target until the
  finger lifts. On the background, the key is down while the finger is. A window gets `wl_touch`. On
  a dark bar nothing is shown, so the touch is ignored until it lifts, and it wakes the bar.
- **Keys come only from what is shown.** Fingers on the background press keys, and so do the shown
  window's virtual keyboards. Virtual keyboards of any other client are ignored.
- **When `shown` changes**, everything pressed through the old one is released. Window touches get
  `wl_touch.cancel`, and their contacts are ignored until they lift.

Inputs wake the bar. It has two: touches and Fn. Commits are not inputs. One timer counts from the
last input. The bar dims after 30 s and goes dark after 60 s. Lit is half the backlight's maximum,
dim is 1 and dark is 0. These numbers are policy, taken from tiny-dfr. While dark, sliverd has no
timer and sends no frame callbacks, so it does nothing until an input arrives. The panel keeps its
last frame, so going dark only changes the backlight.

## Keys

Every key that is down has an owner: a finger on the background, or a virtual keyboard. A key goes
out down when its first owner presses it and up when its last owner lets go. An owner lets go when
it releases the key, lifts, is cancelled or destroyed, or stops being shown. All keys go out
verbatim through one uinput device, as evdev codes. The protocol's keymap rule holds: a key sent
before a keymap is an error. The keymap fd is closed unread, and modifier requests are ignored. So a
client types Ctrl+C by pressing Ctrl and C.

## Windows

Every toplevel is configured fullscreen at 2008×60. smithay checks the xdg lifecycle: initial
configure, ack, map with a buffer, unmap with a null buffer, configure again. A toplevel goes on top
of the stack when it maps, and leaves when it unmaps or is destroyed. When a commit attaches a new
buffer, sliverd copies its pixels into the window and releases the buffer at once. That copy is the
window's content: a hidden window shows again instantly, and a destroyed buffer changes nothing.
Buffer scale and transform are surface state, applied when compositing, so a commit that changes
only them needs no buffer. sliverd places toplevels, always at (0, 0), so the offset doesn't move
them. Popups have no room on a 60 px bar, so sliverd dismisses each one (`popup_done`).

Globals: `wl_compositor`, `wl_shm` (ARGB and XRGB), `wl_output` (2008×60, normal transform),
`wl_seat` (touch), `xdg_wm_base`, `zwp_virtual_keyboard_manager_v1`. sliverd removes smithay's
`wl_subcompositor` global. Qt treats subsurfaces, viewporter and decorations as optional. The
rotation to the 60×2008 scanout happens only in the panel copy.

## Who may connect

Connecting to sliverd means typing keys into the session, which is what `/dev/uinput` already
grants. So sliverd binds `/run/sliver` with umask 077, gives it `/dev/uinput`'s owner, group, mode
and access ACL, and only then listens. This copies the permissions once, at startup. Clients use
`WAYLAND_DISPLAY=/run/sliver`.

## Hardware

sliverd owns seat-touchbar: the bar's DRM card, its digitizer, and its backlight (a udev rule tags
the backlight). sliverd draws into two mapped dumb buffers and shows each with an atomic page flip
that requests its event. The event is the pacing: it sends the frame callbacks and frees the other
buffer. libinput reads two seats: seat-touchbar for touches, and seat0 for Fn. Every seat0 fd is
opened with an `EVIOCSMASK` that passes only `KEY_FN`. The kernel drops the emptied frames, so
typing and the trackpad never wake sliverd. sliverd runs as a system service, like tiny-dfr, so the
F-keys work at the greeter and on a TTY.

Hardware experiments run only through the safeswap harness, which bounds the run and restores the
bar's owner and brightness. sliverd opens and checks every device before it modesets.

## Not sliverd's job

sliverd starts no clients. The session starts the bar (`bar/shell.qml`, Quickshell) on `/run/sliver`
with `QT_QUICK_BACKEND=software`, since sliverd offers only `wl_shm`. Quickshell gets a small
`VirtualKeyboard` type for typing.

## Files

    src/main.rs     shown, stack, touches, keys, Fn, idle, event loop, headless
    src/wayland.rs  the six globals, snapshots, hit testing, frame callbacks, virtual keyboard, socket
    src/fkeys.rs    the background: 13 keys from a 14-glyph bitmap font
    src/hw.rs       DRM, libinput, uinput, backlight
    bar/shell.qml   the default bar
    packaging/      unit, udev rule

Headless (`sliverd --headless SOCKET`) writes each frame to `SOCKET.png`. Tests run in process with
a fake key sink and a fake clock.

## To verify on hardware

- That page flips on the command-mode panel deliver their event once the transfer is done.
- That libinput reports `KEY_FN` from the "Apple MTP keyboard" on seat0.
- The digitizer's axis orientation.