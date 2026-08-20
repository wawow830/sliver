# sliver

A create/edit/apply widget customizer for the MacBook Pro Touch Bar,
running on Asahi Linux.

## Anatomy
- **sliver-core** — widget types, TOML layout config, one shared renderer
  (cairo/pango) so editor preview and real strip always agree.
- **sliverd** — daemon: DRM master on the touchbar panel, touch input,
  live layout application over a unix socket.
- **customizer** (planned) — GUI for building and applying layouts.

## Status
Milestone zero: `cargo run -p sliverd` renders `sliver.toml` to
`preview.png` (2008x60). Hardware takeover comes next.
