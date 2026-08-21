# sliver

A create/edit/apply widget customizer for the MacBook Pro Touch Bar,
running on Asahi Linux.

## Anatomy
- **sliver-core** — widget types, TOML layout config, one shared renderer
  (cairo/pango) so editor preview and real strip always agree.
- **sliverd** — daemon: DRM master on the touchbar panel, touch input,
  Fn/Globe function-row layer through uinput, and live layout application
  over a unix socket.
- **customizer** — `sliver-edit`, a GTK4 + libadwaita GUI: live preview
  (the same sliver-core renderer the daemon uses), widget list with
  reorder/delete, property editing, "Apply to strip" over the socket,
  and save-to-TOML.

## Status
Milestone zero: `sliverd sliver.toml` renders `preview.png` (2008x60).
Milestone one — DONE: `sliverd sliver.toml --drm` claims the panel,
paints the layout upright, holds until Ctrl-C.
Milestone two — DONE: heartbeat re-renders, live battery widget
(/sys/class/power_supply, self-coloring), touch taps on event2 with
press highlights (evdev grab, hit-tested against the layout).
Milestone three — DONE: `/run/user/*/sliver.sock` accepts a TOML doc
per connection: `sliverd --apply cfg.toml`, ok/error reply.
Milestone four — DONE, v1: the customizer GUI runs; drag-and-drop
reordering is the obvious v2.
Milestone five — DONE: hold Fn/Globe for a momentary F1–F12 layer;
tapping a key emits its real Linux keycode through a Sliver uinput
keyboard, and releasing Fn restores the custom layout.

## Function keys

With `sliverd` running, hold the physical **Fn/Globe** key. The strip
changes to F1–F12. Tap a key, then release Fn to return to your widgets.
The user running Sliver needs read access to `/dev/input/event*` and
read/write access to `/dev/uinput` (the Fedora `input` group provides
both on this machine).

Hard-won truths: the DSI panel freezes its last frame (dead processes
haunt the glass); the driver rounds the dumb buffer up (paint by mode
height, 2008); command-mode panels need `dirty_framebuffer` after every
repaint or the glass never changes; `--probe` paints calibration bands
when orientation is in doubt.
