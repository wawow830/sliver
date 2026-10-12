# ADR-0001: Initial stack

Status: accepted (seed). Any peer may supersede this with a new ADR. Say so on the channel first.

## Decision
- **Language:** Rust. tiny-dfr is Rust, so we can learn from and reuse its DRM, input and
  backlight approach. Use a single Cargo workspace.
- **Processes:**
  - `sliverd`: system daemon, root-ish like tiny-dfr. Owns the DRM panel, touch input, backlight
    and uinput key emission. Keeps it minimal and crash-proof, and has the built-in safe layer.
  - `sliver-agent`: per-user session service. Tracks the compositor (active app/workspace),
    exposes the public session D-Bus API, runs widgets, and decides what to show. Talks to
    `sliverd` over a narrow, versioned local IPC.
  - `sliverctl`: CLI client of the public API.
  - `sliver-settings`: GTK4 + libadwaita settings and editor app. gtk4-devel and libadwaita-devel
    are already installed.
- **D-Bus:** `zbus`. Well-known name and interface naming go in a dedicated ADR before anything
  public ships.
- **Config:** TOML in `~/.config/sliver/` (user) and `/etc/sliver/` (system). Versioned schema,
  plus a tiny-dfr importer.
- **Dev profile:** keep `[profile.dev] debug = "line-tables-only"` in the workspace. The disk is
  small and every peer has its own `target/`.

## Open (decide via ADRs as work demands)
Rendering stack (cairo+pango+librsvg like tiny-dfr, or tiny-skia+resvg+cosmic-text with no system
deps), sliverd↔agent IPC format, widget plugin model (in-process vs out-of-process), config schema.
