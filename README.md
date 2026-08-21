# sliver

A create/edit/apply widget customizer for the MacBook Pro Touch Bar,
running on Asahi Linux.

## Anatomy
- **sliver-core** — widget types, TOML layout config, one shared renderer
  (cairo/pango) so editor preview and real strip always agree.
- **sliverd** — daemon: DRM master on the touchbar panel, touch input,
  live layout application over a unix socket.
- **customizer** (planned) — GUI for building and applying layouts
  (GTK4 + libadwaita; decided).

## Status
Milestone zero: `sliverd sliver.toml` renders `preview.png` (2008x60).
Milestone one — DONE: `sliverd sliver.toml --drm` claims the panel,
paints the layout upright, holds until Ctrl-C.
Milestone two — DONE: heartbeat re-renders, live battery widget
(/sys/class/power_supply, self-coloring), touch taps on event2 with
press highlights (evdev grab, hit-tested against the layout).

Hard-won truths: the DSI panel freezes its last frame (dead processes
haunt the glass); the driver rounds the dumb buffer up (paint by mode
height, 2008); command-mode panels need `dirty_framebuffer` after every
repaint or the glass never changes; `--probe` paints calibration bands
when orientation is in doubt.
