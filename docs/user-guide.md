# Sliver user guide

Sliver turns the Touch Bar on Linux Macs into part of your desktop. You control what it shows,
per app, with waybar-style modules and a stylesheet. Apps and scripts can put their own buttons on
it, and **Escape and the F-keys always stay reachable.**

> Status: development snapshot (0.1.0). Packages aren't published yet. The pieces below that say
> *(soon)* are still being wired up.

Reference: `man sliverctl`, `man sliver-config`, `man sliver-stylesheet`, `man sliver-agent`,
`man sliverd` (sources in `man/`; read one without installing via `man -l man/sliver-config.5`).

## 1. How it fits together

| Piece | Runs as | Does |
|---|---|---|
| `sliverd` | system service (replaces `tiny-dfr`) | Owns the bar's display, touch and backlight. Has a built-in F-key layer. |
| `sliver-agent` | your user session | Reads your config, follows the focused app, runs modules, draws the bar. |
| `sliverctl` | you, scripts | Inspects and controls the bar, edits config, publishes buttons. |

If the agent isn't running, crashes, or your config is broken, `sliverd` shows its built-in
F-keys. That's the safety net.

## 2. The safe hatch: remember this one

**Press Fn twice quickly and keep holding it.** As long as you hold it, the bar shows F1–F12 (with
Esc first on models without a physical Escape key), whatever your config or any app is doing.
Nothing can turn this off.

## 3. Install *(soon: RPM/COPR)*

From source:
```sh
cargo build --release
sudo install -m755 target/release/{sliverd,sliver-agent,sliverctl} /usr/local/bin/
sudo install -Dm644 man/*.1 -t /usr/local/share/man/man1/   # likewise .5 → man5, .8 → man8
```
The systemd units and udev rule that let `sliverd` take over from `tiny-dfr` are in `packaging/`
(see `docs/sliverd.md`). `sliverd` hands the bar back to `tiny-dfr` if it keeps crashing.

## 4. Configure

```sh
sliverctl config path                 # where sliver looks; * marks the file in use
sliverctl config import-tiny-dfr      # start from your tiny-dfr setup, or:
sliverctl config import-waybar        # start from your waybar modules
$EDITOR ~/.config/sliver/config.toml  # changes apply on save (soon; until then restart the agent)
sliverctl config check                # validate
```

A small config:
```toml
version = 1

[[layouts.default.zones]]
name = "main"
width = "fill"
modules = ["app-controls"]            # buttons published by the focused app go here

[[layouts.default.zones]]
name = "controls"
modules = ["status-items", "audio/slider", "battery", "clock"]

[[rules]]                             # first match wins
name = "Terminals get F-keys"
when = { app-id = ["foot", "kitty"] }
layout = "fkeys"

[modules.clock]
format = "{:%H:%M}"
format-alt = "{:%a %d %b}"            # tap to toggle

[modules."custom/vpn"]                # your own module, waybar style
exec = "~/bin/vpn-status --json"
return-type = "json"
interval = "5s"
signal = 8                            # `sliverctl signal 8` re-runs it
on-tap = "~/bin/vpn-toggle"
```
Everything, the Fn layer included, can be changed. The full reference is `man sliver-config` and
`docs/customization.md`.

Try a config without the bar: `sliver-agent --render /tmp/preview --config my.toml` writes
`/tmp/preview-base.png` and `/tmp/preview-fn.png`.

## 5. Style

`~/.config/sliver/style.css`, like waybar's:
```css
module:active     { background: @accent; }      /* while pressed */
#battery.critical { background: #c01c28; }
.zone-main        { background: alpha(white, 0.06); border-radius: 12px; }
```
Start from tiny-dfr's look with `@import "sliver:classic";`. Reference: `man sliver-stylesheet`,
`docs/styling.md`.

## 6. Scripts and apps

From the shell, buttons live as long as `sliverctl publish` runs:
```sh
sliverctl publish --provider org.example.Build button:build:'🔨 Build' |
  while read -r item kind _; do [ "$kind" = tap ] && make; done
```
Drive the bar like waybar: `sliverctl signal 8`, `sliverctl hide clock`, `sliverctl layer push media 5000`,
`sliverctl bar toggle`. See `man sliverctl` and `examples/shell/README.md`.

Apps (any language, plain D-Bus) publish items for their own windows. See `docs/api.md`,
`examples/python/pomodoro.py`, and the Rust crate `sliver-client`. Whatever apps publish is yours
to move, restyle, rebind or hide (`app/<provider>#<item>` in your config).

## 7. Troubleshooting

| Symptom | Try |
|---|---|
| Bar shows only F-keys | The agent isn't driving it: `systemctl --user status sliver-agent`, `sliverctl status`. |
| My change didn't apply | `sliverctl config check`. Invalid configs are never applied, and the last good one stays. |
| A custom module shows `!` | Its script failed or timed out (default 2 s). Run it by hand. |
| Bar is dark | It dims after 30 s and turns off after 60 s idle. A touch wakes it. See `[display]`. |
| Want tiny-dfr back | `sudo systemctl disable --now sliverd.socket sliverd.service && sudo systemctl start tiny-dfr` (or uninstall sliver). |
| Debugging the daemon | `sliverd -v` (or `SLIVER_LOG=debug`), `journalctl -u sliverd`. |
