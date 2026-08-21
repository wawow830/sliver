# sliver

A create/edit/apply widget system for the 13-inch Apple Silicon MacBook Pro
Touch Bar on Asahi Linux.

Sliver consists of:

- **`sliverd`** — owns the DRM touchbar panel, renders widgets, reads touch and
  Fn/Globe input, emits F1–F12 through uinput, and accepts live layouts over a
  Unix socket.
- **`sliver-core`** — shared config, layout, hit-testing, and cairo/pango
  renderer. The GUI preview and physical strip use the same code.
- **`sliver-edit`** — GTK4/libadwaita customizer with live preview, widget
  creation, styling, reorder/delete, Apply, and Save.

## Hardware tested

- Apple MacBook Pro (13-inch, M2, 2022 / Mac14,7)
- Fedora Asahi Remix 44
- Touchbar display: `/dev/dri/card1`, DSI-1, native mode 60x2008
- Touch input: `Mac14,7 Touch Bar`
- Keyboard: `Apple MTP keyboard`

The current DRM and touch-device paths are hardware-specific; see
[troubleshooting](docs/troubleshooting.md) if enumeration differs.

## Build

System libraries required by the Rust crates include GTK4, libadwaita,
cairo, and pango development files.

```bash
cd ~/Projects/sliver
cargo build --release
```

The user running Sliver needs access to the `video` and `input` groups:

```bash
id
ls -l /dev/dri/card1 /dev/input/event2 /dev/uinput
```

## Run

Only one process can own the touchbar DRM device. Stop tiny-dfr first:

```bash
sudo systemctl stop tiny-dfr
```

Start the daemon and keep it running:

```bash
cd ~/Projects/sliver
./target/release/sliverd sliver.toml --drm
```

Start the customizer in another terminal:

```bash
cd ~/Projects/sliver
./target/release/sliver-edit
```

In the customizer:

1. Add or select a widget.
2. Edit its text, color, font, width, alignment, background, or action.
3. Choose **Apply to strip** for an immediate live update.
4. Choose **Save** to write the layout to `sliver.toml` for future starts.

## Function keys

Hold the physical **Fn/Globe** key to replace the custom layout with F1–F12.
Tap a key and release Fn to return to your widgets.

Sliver mirrors held Ctrl, Alt, Shift, and Super modifiers onto its virtual
keyboard, so cross-device chords work. For example, to switch to TTY2:

1. Hold **Ctrl + Alt + Fn/Globe**.
2. Tap **F2** on the touchbar.
3. Release the held keys.

F1–F12 are emitted as real Linux key events through `/dev/uinput`.

## CLI

Render a PNG preview without touching hardware:

```bash
./target/release/sliverd sliver.toml preview.png
```

Apply a config to a running daemon:

```bash
./target/release/sliverd --apply sliver.toml
```

Paint raw calibration bands on the panel:

```bash
./target/release/sliverd --probe
```

Stop Sliver and return to tiny-dfr:

```bash
pkill -INT -x sliverd
sudo systemctl start tiny-dfr
```

## Documentation

- [Configuration and widget reference](docs/configuration.md)
- [Architecture and hardware notes](docs/architecture.md)
- [Troubleshooting](docs/troubleshooting.md)

## Development checks

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Current capabilities

- Shared cairo/pango renderer and exact GUI preview
- TOML label, button, clock, battery, and spacer widgets
- Per-widget color, font size, bold, background pill, alignment, and width
- Shell actions on touch
- Live battery data and once-per-second redraws
- Live apply via `$XDG_RUNTIME_DIR/sliver.sock`
- Touch press feedback
- Momentary Fn/Globe F1–F12 layer with modifier bridging
- GTK4/libadwaita customizer

Drag-and-drop reordering and richer widgets such as media state and sliders
remain natural next steps.
