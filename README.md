# sliver

A create/edit/apply widget system for the 13-inch Apple Silicon MacBook Pro
Touch Bar on Asahi Linux.

Sliver consists of:

- **`sliver-broker`** — system service that owns the DRM Touch Bar, normalized
  input, uinput, backlight, and the embedded fallback before login.
- **`sliver-supervisor`** — per-user service that owns one user's selected Lua
  worker and accepts `sliver FILE` apply requests.
- **`sliver`** — the public CLI for selecting an explicit Lua source or the
  embedded default.
- **`sliverd`**, **`sliver-core`**, and **`sliver-edit`** — retained legacy
  binaries and libraries during the migration away from the TOML widget
  product.

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

Enable the system broker after installing its service account and device
permissions:

```bash
sudo systemctl enable --now sliver-broker.service
systemctl --user enable --now sliver-supervisor.service
```

The broker paints the embedded default before login. When a user session starts,
that user's supervisor stages its selected Lua source and hands over the first
complete frame without clearing the panel. Suspend hides the worker, pauses
its timers, and turns off the backlight; resume restores the same worker and
requests a fresh frame.

## Function keys

Hold the physical **Fn/Globe** key to replace the custom layout with F1–F12.
Tap a key and release Fn to return to your widgets.

Sliver mirrors held Ctrl, Alt, Shift, and Super modifiers onto its virtual
keyboard, so cross-device chords work. For example, to switch to TTY2:

1. Hold **Ctrl + Alt + Fn/Globe**.
2. Tap **F2** on the touchbar.
3. Release the held keys.

F1–F12 are emitted as real Linux key events through `/dev/uinput`.

## Scriptable apply CLI

The supervisor accepts one tagged apply request through a local Unix socket.
Use `sliver FILE` for an explicit Lua source, or run `sliver` with no path to
select the embedded default and remove the saved path. Success is silent;
failures go to stderr.

The embedded default is one canonical `default.lua` source. It uses the same
Lua worker, canvas, input, key, timer, and backlight interfaces as an explicit
file. A broken saved path stays selected and shows the fixed recovery row.

## Legacy TOML CLI

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
- Live apply via `$XDG_RUNTIME_DIR/sliver/supervisor.sock`
- Touch press feedback
- Momentary Fn/Globe F1–F12 layer with modifier bridging
- GTK4/libadwaita customizer

Drag-and-drop reordering and richer widgets such as media state and sliders
remain natural next steps.
