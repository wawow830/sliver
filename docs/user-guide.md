# Sliver user guide

Sliver turns the Touch Bar on Linux Macs into part of your desktop. You control what it shows,
per app, with waybar-style modules and a stylesheet, or by dragging things around in the settings
app. Apps and scripts can put their own buttons on it, and **Escape and the F-keys always stay
reachable.**

| Piece | Runs as | Does |
|---|---|---|
| `sliverd` | system service (replaces `tiny-dfr`) | Owns the bar's display, touch and backlight. Has a built-in F-key layer. |
| `sliver-agent` | your user session | Reads your config, follows the focused app, runs modules, draws the bar. |
| `sliver-settings` | app ("Sliver" in your app menu) | Edits the bar by drag and drop, with a live preview. |
| `sliverctl` | you, scripts | Inspects and controls the bar, edits config, publishes buttons. |

If the agent isn't running, crashes, or your config is broken, `sliverd` shows its built-in
F-keys. That's the safety net.

## The safe hatch: remember this one

**Press Fn twice quickly and keep holding it.** As long as you hold it, the bar shows F1–F12 (with
Esc first on models without a physical Escape key), whatever your config or any app is doing.
Nothing can turn this off.

## Install

Build the RPMs and install them (Fedora; about 15 minutes for the release build):
```sh
sudo dnf builddep packaging/rpm/sliver.spec
scripts/rpm-build --binary                      # RPMs land in target/rpmbuild/RPMS/
sudo dnf install target/rpmbuild/RPMS/*/*.rpm   # sliver, sliverd, sliver-agent, sliverctl, sliver-settings
```
`sliver-browser` is in the same folder: a Firefox and Chromium extension that puts the focused
window's tabs, back, forward and reload on the bar (`integration/browser/README.md`). Leave it out
if you don't want it.

Installing doesn't take the bar from tiny-dfr right away: sliverd takes it from the next boot.
No reboot is needed, though. To switch now:
```sh
sudo systemctl start sliverd          # stops tiny-dfr and takes the bar
systemctl --user start sliver-agent   # from the next login on, it starts with your session
```
The agent starts with `graphical-session.target`. GNOME, KDE and compositors started through
uwsm activate that target. Hyprland or sway started straight from a display manager don't, so
nothing starts the agent and the bar stays on the built-in F-keys (`sliverctl doctor` says so).
Start it from the compositor's autostart instead:
```
exec-once = systemctl --user start sliver-agent   # Hyprland
exec systemctl --user start sliver-agent          # sway
```

**GNOME and KDE** need a small integration so the agent knows the focused app, workspaces and
windows. The packages `gnome-shell-extension-sliver` and `sliver-kwin` come along automatically
when GNOME or KWin is installed, but you enable them yourself, once:
- GNOME: `gnome-extensions enable sliver-focus@sliver.wawow830.github.io`, then log out and in.
- KDE: System Settings → Window Management → KWin Scripts → *Sliver focus*.

Hyprland, sway, niri and other wlroots compositors need nothing.

**Back to tiny-dfr:** `sudo systemctl start tiny-dfr` stops sliverd until the next boot. For good,
uninstall: `sudo dnf remove sliver sliverd sliver-agent sliverctl sliver-settings` hands the bar to
tiny-dfr at once. (`systemctl disable` doesn't help: udev starts sliverd, not an enabled unit.)
If sliverd crashes 5 times within a minute, systemd brings tiny-dfr back by itself; run
`sudo systemctl reset-failed sliverd` before starting sliverd again. Upgrades (`dnf upgrade`)
restart sliverd (about half a second of bar) and running agents on their own. More in
`docs/packaging.md`.

## First run

Open **Sliver** from your app menu (`sliver-settings`). The first time, it asks how to start:

- **Import tiny-dfr setup** keeps your tiny-dfr keys and look (`theme = "classic"`).
- **Use the default bar**: F-keys until an app adds its own buttons, plus brightness, volume and
  now playing.

On the **Bar** page the preview is your real bar, drawn by the same code. Drag modules from the
Library onto it, along it, off it, or by their right edge to resize; click one to edit it.
**Mirror** (on by default) shows the layout you're editing on the real bar while the window is
focused, and **Customize for app…** makes a layout for one app and a rule that picks it. Every edit
applies at once and can be undone (Ctrl+Z); a change that would break the config is refused.

Prefer a text editor? The settings app and your editor work on the same file, comments included,
and a broken file is never applied:
```sh
sliverctl config path                 # where sliver looks; * marks the file in use
sliverctl config import-tiny-dfr      # start from your tiny-dfr setup, or:
sliverctl config import-waybar        # start from your waybar modules
$EDITOR ~/.config/sliver/config.toml  # changes apply on save
sliverctl config check                # validate
sliver-agent --render /tmp/preview --config my.toml   # see it without the bar
```
Ready-made modules to copy or `include` are in `examples/config/` (power menu, weather, exchange
rate, calendar "up next").

## Reference

Everything, the Fn layer included, can be changed. The reference is in the man pages (sources in
`man/`; read one without installing via `man -l man/sliver-config.5`):

- `man sliver-config`: modules, zones, layers, rules, conditions, actions, gestures, profiles.
- `man sliver-stylesheet`: selectors, properties, themes and animations.
- `man sliverctl`: the command line, presets and scripting from outside.
- `man sliver-settings`, `man sliver-agent`, `man sliverd`.
- Apps and scripts that publish their own buttons: `docs/api.md`, `examples/`.

## Troubleshooting

**Start with `sliverctl doctor`.** It checks the whole install in a few seconds and prints one line
per check, `ok`, `WARN` (a module will be empty: no NetworkManager, no tray host, no PipeWire) or
`FAIL` (the bar or the config isn't working), with the fix under each. It covers sliverd and
tiny-dfr (and that they aren't both running), the agent and its errors, the config, the GNOME/KDE
helper, and the services that feed modules. It only reads. Run it inside your desktop session, and
paste its output (`--json` for scripts) into a bug report: it starts with your OS, machine and desktop.

`sliverctl status` answers most questions: is the agent driving the bar (`Healthy`), which layer
and layout are on the bar and **why** (`ActiveLayoutSource`: a rule, the layer's default, or
forced), the active profile, whether the display is on, dimmed or off, and what's wrong right now
(`Errors`: a rejected config, a stylesheet or module problem, with the file and line).

| Symptom | Try |
|---|---|
| Bar shows only F-keys | The agent isn't driving it. `sliverctl doctor` says why; see Install if it never starts at login. |
| My change didn't apply | `sliverctl config check`. Invalid configs are never applied, and the last good one stays. |
| The wrong layout shows | `sliverctl status` names the rule that won; rules match top to bottom. |
| A custom module shows `!` | Its script failed or timed out (default 2 s). Run it by hand. |
| Bar is dark | It dims after 30 s and turns off after 60 s idle; the first touch only wakes it. With the lid closed it's off by design. See `[display]` in `man sliver-config`. |
| An app's buttons are missing | The settings app's **Apps** page: is it hidden or not allowed? `sliverctl contexts` lists what's published. |
| tiny-dfr has the bar after a crash | sliverd failed 5 times: read `journalctl -u sliverd -b`, then `sudo systemctl reset-failed sliverd && sudo systemctl start sliverd`. |
| `refusing to open` in sliverd's log | Its safety gate found something else using the bar (another tiny-dfr, a test run). |
| `backlight not writable` | `sudo udevadm trigger --action=change` on the bar's backlight, or reboot once after installing. |
| Want tiny-dfr back | `sudo systemctl start tiny-dfr` until the next boot; uninstall for good (see Install). |

Logs: `journalctl -u sliverd -b` (the daemon) and `journalctl --user -u sliver-agent` (config
errors, failing scripts, compositor connection). For a debug log from the daemon, add
`Environment=SLIVER_LOG=debug` in a runtime drop-in
(`/run/systemd/system/sliverd.service.d/debug.conf`) and restart it. The agent already logs
everything to its journal.
