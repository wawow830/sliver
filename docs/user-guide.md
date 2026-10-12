# Sliver user guide

Sliver turns the Touch Bar on Linux Macs into part of your desktop. You control what it shows,
per app, with waybar-style modules and a stylesheet, or by dragging things around in the settings
app. Apps and scripts can put their own buttons on it, and **Escape and the F-keys always stay
reachable.**

> Status: development snapshot (0.1.0). There are no published packages yet: you build the RPMs
> from this repository (§3). Tested on Apple Silicon (Fedora Asahi, Mac14,7). Intel T2 Macs are
> supported but untested (`docs/t2-testing.md`).

Reference: `man sliverctl`, `man sliver-config`, `man sliver-stylesheet`, `man sliver-settings`,
`man sliver-agent`, `man sliverd` (sources in `man/`; read one without installing via
`man -l man/sliver-config.5`).

## 1. How it fits together

| Piece | Runs as | Does |
|---|---|---|
| `sliverd` | system service (replaces `tiny-dfr`) | Owns the bar's display, touch and backlight. Has a built-in F-key layer. |
| `sliver-agent` | your user session | Reads your config, follows the focused app, runs modules, draws the bar. |
| `sliver-settings` | app ("Sliver" in your app menu) | Edits the bar by drag and drop, with a live preview. |
| `sliverctl` | you, scripts | Inspects and controls the bar, edits config, publishes buttons. |

If the agent isn't running, crashes, or your config is broken, `sliverd` shows its built-in
F-keys. That's the safety net.

## 2. The safe hatch: remember this one

**Press Fn twice quickly and keep holding it.** As long as you hold it, the bar shows F1–F12 (with
Esc first on models without a physical Escape key), whatever your config or any app is doing.
Nothing can turn this off.

## 3. Install

Build the RPMs and install them (Fedora; about 15 minutes for the release build):
```sh
sudo dnf builddep packaging/rpm/sliver.spec
scripts/rpm-build --binary                      # RPMs land in target/rpmbuild/RPMS/
sudo dnf install target/rpmbuild/RPMS/*/*.rpm   # sliver, sliverd, sliver-agent, sliverctl, sliver-settings
```
`sliver-browser` (§10) is in the same folder; leave it out if you don't want it.

Installing changes nothing until the next boot: tiny-dfr keeps the bar. To switch right away:
```sh
sudo systemctl start sliverd          # stops tiny-dfr and takes the bar
systemctl --user start sliver-agent   # normally starts with your graphical session
```
**Back to tiny-dfr** at any time: `sudo systemctl start tiny-dfr` (it stops sliverd). If sliverd
keeps crashing, systemd hands the bar back to tiny-dfr by itself. `sudo dnf remove sliverd
sliver-agent` gives the bar back to tiny-dfr immediately. Upgrades restart sliverd (about half a
second of bar) and the agent on their own.

From source without packages, see `docs/sliverd.md` (the units and udev rule in `packaging/` are
what make `sliverd` take over).

## 4. First run: the settings app

Open **Sliver** from your app menu (`sliver-settings`). The first time, it asks how to start:

- **Import tiny-dfr setup** keeps your tiny-dfr keys and look (`theme = "classic"`).
- **Use the default bar**: F-keys until an app adds its own buttons, plus brightness, volume and
  now playing.

On the **Bar** page the preview is your real bar, drawn by the same code:
- Drag a module from the Library onto the bar, along it to move it (also into another zone), off it
  to remove it, or its right edge to resize it. Click a module to edit its format, icon, width,
  actions and "Show only when" conditions.
- From the keyboard: Tab to the preview, ←/→ to select, Alt+←/→ to move, Delete to remove, Enter
  to edit.
- **Mirror** (on by default) shows the layout you're editing on the real bar while the window is
  focused.
- **Customize for app…** makes a layout for one app (say Firefox) and a rule that picks it.
- Every edit applies at once and can be undone (Ctrl+Z, or Undo in the toast). A change that would
  break the config is refused.

The other pages: **Layouts & Rules** (which layout shows when; the rule active now is marked),
**Gestures**, **Appearance** (theme, fonts, accent, animations), **Display & Power** (brightness,
dim/off timers, the on-battery profile, the brightness curve), **Apps** (what apps may publish),
**Profiles & Presets**, and **Status** (is the agent running, why this layout, errors).

## 5. Configure by hand

The settings app and your editor work on the same file, comments included. Change either and the
agent reloads within a moment; a broken file is never applied (the last good config stays).
```sh
sliverctl config path                 # where sliver looks; * marks the file in use
sliverctl config import-tiny-dfr      # start from your tiny-dfr setup, or:
sliverctl config import-waybar        # start from your waybar modules
$EDITOR ~/.config/sliver/config.toml  # changes apply on save
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
`docs/customization.md`. Ready-made modules to copy or `include`: `examples/config/` (power menu,
weather, exchange rate, calendar "up next").

Try a config without the bar: `sliver-agent --render /tmp/preview --config my.toml` writes
`/tmp/preview-base.png` and `/tmp/preview-fn.png`.

## 6. Themes and stylesheets

`theme = "…"` in config.toml picks the base look: `sliver` (dark, the default), `light`,
`high-contrast` or `classic` (tiny-dfr's). Your `~/.config/sliver/style.css` goes on top, like
waybar's:
```css
module:active     { background: @accent; }      /* while pressed */
#battery.critical { background: #c01c28; }
.zone-main        { background: alpha(white, 0.06); border-radius: 12px; }
```
`@accent` follows your desktop's accent colour. Animations stop when your desktop asks for reduced
motion. Reference: `man sliver-stylesheet`, `docs/styling.md`.

## 7. Gestures

Every module takes `on-tap`, `on-hold`, `on-double-tap`, `on-swipe-left`, `on-swipe-right` and
(sliders) `on-drag`: a shell command, a key (`{ key = "F5" }`), a combo, a layer or layout switch,
a D-Bus call, a URL or app to open, or a macro. Swipes across the whole bar go in `[gestures]`:
```toml
[gestures]
two-finger-swipe-left = { layer = "media" }
edge-swipe-from-left = { keys = ["LeftAlt", "Tab"] }
```
Press-and-slide on a collapsed slider's icon sets it without lifting your finger. On Hyprland, key
combos follow your keyboard layout (on AZERTY, `ctrl+z` is still undo).

## 8. Profiles and presets

A **profile** is a whole config you switch to, like "Work" or "Presenting":
```sh
sliverctl config profile new presenting      # your main config plus changes, in profiles/presenting.toml
sliverctl profile presenting                 # switch (or the settings app, or a { profile = … } button)
sliverctl profile default                    # back to config.toml
```
A **preset** is a whole setup (config, includes, stylesheet, icons) in one readable `.sliver` file:
```sh
sliverctl preset export desk --name "My desk"   # writes desk.sliver; env tables stay home
sliverctl preset show desk.sliver               # files, everything it runs, does it load
sliverctl preset import desk.sliver --profile desk   # as a new profile (or replace your config)
```
A preset's scripts run as you, so read the `runs` list before you say yes, or import with
`--no-commands`. Double-clicking a `.sliver` file opens it in the settings app for review.

## 9. The lock screen and privacy

While the screen is locked, the bar shows the `locked` layout: media controls, brightness, volume,
mute (and Esc on Macs without one). Whatever your config says, it never shows buttons from apps,
script output, notification text or window titles there. Notification text also stays off the bar
while you share your screen.

## 10. Scripts and apps

From the shell, buttons live as long as `sliverctl publish` runs:
```sh
sliverctl publish --provider org.example.Build button:build:'🔨 Build' |
  while read -r item kind _; do [ "$kind" = tap ] && make; done
```
Drive the bar like waybar: `sliverctl signal 8`, `sliverctl hide clock`, `sliverctl layer push media 5000`,
`sliverctl bar toggle`. See `man sliverctl` and `examples/shell/README.md`.

Apps (any language, plain D-Bus) publish items for their own windows: buttons, toggles, sliders,
segmented controls, scrollable strips and popovers. See `docs/api.md`, `examples/python/pomodoro.py`,
the Rust crate `sliver-client`, and recipes for GTK/GJS, Qt and Electron/Node in
`docs/integrating-apps.md`. Whatever apps publish is yours to move, restyle, rebind or hide
(`app/<provider>#<item>` in your config; the settings app's **Apps** page).

**Browsers:** the optional `sliver-browser` package adds a Firefox and Chromium extension that puts
the focused window's tabs (swipe to scroll, tap to switch), back, forward and reload on the bar.
Load the extension from `/usr/share/sliver/browser/` (see `integration/browser/README.md`).

**GNOME and KDE** need a small integration so the agent knows the focused app and workspaces: the
GNOME Shell extension or the KWin script in `integration/` (copy it as `integration/README.md`
says and enable it once per user; the packages don't ship them yet). Hyprland,
sway, niri and other wlroots compositors need nothing.

## 11. Troubleshooting

`sliverctl status` answers most questions: is the agent healthy, which layer and layout are on the
bar and **why** (`ActiveLayoutSource`: a rule, the layer's default, or forced), the active profile,
and whether the display is on, dimmed or off.

| Symptom | Try |
|---|---|
| Bar shows only F-keys | The agent isn't driving it: `systemctl --user status sliver-agent`, `sliverctl status`. |
| My change didn't apply | `sliverctl config check`. Invalid configs are never applied, and the last good one stays. |
| The wrong layout shows | `sliverctl status` names the rule that won; rules match top to bottom. |
| A custom module shows `!` | Its script failed or timed out (default 2 s). Run it by hand. |
| Bar is dark | It dims after 30 s and turns off after 60 s idle. A touch wakes it. See `[display]`. |
| An app's buttons are missing | The settings app's **Apps** page: is it hidden or not allowed? `sliverctl contexts` lists what's published. |
| Want tiny-dfr back | `sudo systemctl start tiny-dfr` (or uninstall sliverd). |

Logs: `journalctl -u sliverd` (the daemon; `sliverd -v` or `SLIVER_LOG=debug` for more) and
`journalctl --user -u sliver-agent` (config errors, failing scripts, compositor connection).
