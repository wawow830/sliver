# sliver

Sliver is a scriptable Touch Bar service for the tested M2 Mac14,7 running
Asahi Linux. Lua owns the layout, visuals, and normal interaction. Sliver owns
DRM scanout, normalized input, generic key output, backlight, service
lifecycle, and recovery. This release does not claim M1 support; that requires
separate real-hardware validation.

## Public command

There is one public executable. Its only operational forms are:

```sh
sliver          # select the embedded default and clear the saved path
sliver FILE     # apply FILE as the active Lua configuration
sliver --help
sliver --version
```

An apply succeeds only after the candidate has loaded, passed validation, and
presented its first complete frame. Success is silent. Usage errors exit 2;
service and application failures exit 1.

## Runtime

The package contains three private service processes:

- `sliver-broker` owns the Touch Bar hardware and the persistent `Sliver
  Keyboard` uinput device.
- `sliver-supervisor` runs for the active local user and stages Lua workers.
- `sliver-lua-worker` runs one disposable configuration without hardware
  device access.

The embedded default is the canonical `crates/sliverd/src/default.lua` source
and is embedded into the worker at build time. It is not installed as an
editable file.

## Build

System libraries required by the Rust crate include Cairo, Pango, and
PangoCairo. Build with:

```sh
cargo build --release
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Fedora Asahi packaging is documented in
[packaging/fedora/INSTALL.md](packaging/fedora/INSTALL.md).

## Enable the service

The package does not take over the Touch Bar during installation. Stop
`tiny-dfr` before enabling Sliver; the broker also declares a systemd conflict
with it.

After the one-time account and udev setup, an administrator enables takeover:

```sh
sudo systemctl enable --now sliver-broker.service && \
  sudo systemctl --global enable sliver-supervisor.service
```

For an existing graphical session, start the user unit immediately:

```sh
systemctl --user enable --now sliver-supervisor.service
```

The broker shows the embedded default before login. A local user's supervisor
keeps that frame visible until its selected configuration commits. A broken
saved path remains selected and shows the fixed recovery row; it does not
silently fall back to the default.

## Lua v1

A configuration returns one application table:

```lua
local sliver = require("sliver.v1")

return {
    api_version = 1,
    render = function(canvas, time, delta)
        canvas:rectangle(0, 0, 2008, 60, "#000000")
        canvas:text(20, 36, "Hello", 24, "#ffffff")
    end,
}
```

The allowed fields are `api_version`, `start`, `stop`, `visibility`, `touch`,
`key`, and required `render`. Unknown fields and invalid callback values are
rejected during apply. Callbacks run serially, cannot yield, and have no useful
return value.

The module provides:

- `sliver.redraw()` for a coalesced frame request;
- `sliver.timer.after(seconds, callback)` and
  `sliver.timer.every(seconds, callback)`;
- `sliver.input.state()` and `sliver.input.key.down`, `.up`, and `.tap`;
- versioned keyboard and consumer-key constants under
  `sliver.input.keys.keyboard` and `.consumer`;
- `sliver.backlight.get()` and `.set(level)` for levels from 0.0 to 1.0;
- a logical 2008 by 60 Cairo/Pango canvas with rectangles, paths, fill, stroke,
  text, images, raw pixels, transforms, clipping, alpha, save/restore, and
  source or source-over compositing.

Touch callbacks receive `down`, `move`, `up`, and `cancel` events with contact
ID, monotonic time, logical coordinates, modifier state, and optional pressure
and contact dimensions. Fn and left/right Ctrl, Alt, Shift, and Super
transitions are exposed; general keyboard monitoring is not.

See [the Lua interface reference](docs/lua.md) for the complete v1 surface.

## Safety and recovery

Workers run as the applying user but cannot access DRM, evdev, uinput, or the
backlight. A callback deadline is two seconds; graceful cleanup has a 500 ms
deadline. Worker descendants are terminated on replacement or failure.

Holding the physical Fn/Globe key for three seconds selects the compiled F1–F12
recovery row. It is also shown when no worker is healthy. Recovery touches do
not reach Lua, synthetic keys are released during replacement, and recovery
uses 0.75 brightness.

Suspend hides the worker, cancels contacts, pauses timers, turns off the
backlight, and restores the worker with one fresh frame after resume.

## Return to tiny-dfr

Stopping Sliver does not restart another daemon automatically:

```sh
sudo systemctl stop sliver-broker.service
sudo systemctl stop tiny-dfr.service  # only if it was started separately
sudo systemctl start tiny-dfr.service
```

## Release verification

Run the repository-side audit before a release:

```sh
scripts/check-release-surface.sh
```

A real Mac14,7, a local graphical session, and administrator access are
required for hardware and RPM cutover checks. Build the RPM from a clean,
tested commit and keep the `rpmbuild` output. The verifier checks the RPM's
NEVRA, complete file list, and embedded source commit before it asks to install:

```sh
scripts/verify-release.sh --build-log "$HOME/sliver-rpmbuild.log" \
  /path/to/sliver-0.1.0-1.aarch64.rpm
```

The verifier is a resumable wizard. Account setup ends before logout, and the
next local graphical session continues with:

```sh
scripts/verify-release.sh --resume "$HOME/sliver-release-verification/YYYYMMDD-HHMMSS"
```

Every acceptance item gets its own evidence entry. A failed run performs a
full rollback and verifies the package, account, linger, udev, service, and
selected-path state. If a machine is left in takeover state, use the explicit
`--service-only` cleanup only when retaining the package is intentional. Use
`--rollback` for the full transaction. Do not run the verifier over SSH or
claim M1 support from this run.

See [architecture](docs/architecture.md) and
[troubleshooting](docs/troubleshooting.md) for hardware, ownership, and
failure details.
