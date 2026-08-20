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
Milestone one: `sliverd sliver.toml --drm` claims the real panel —
DRM master, native 60x2008 mode, rotated painting, holds until Ctrl-C.
