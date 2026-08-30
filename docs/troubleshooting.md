# Troubleshooting

## `Device or resource busy` / cannot become DRM master

Another process owns the Touch Bar DRM card.

```bash
fuser -v /dev/dri/card*
pgrep -a 'sliverd|tiny-dfr'
```

Stop the existing owner:

```bash
pkill -INT -x sliverd
sudo systemctl stop tiny-dfr
```

Then start one Sliver daemon.

## `Permission denied`

Check device ownership and groups:

```bash
id
ls -l /dev/dri/card* /dev/input/event* /dev/uinput
```

The Fedora package grants device-specific hardware access to the `sliver`
broker account through dedicated udev groups. A user applying a config needs membership in
`sliver-supervisors`; group changes require a new login session.

## The strip still shows an old image

The DSI panel retains its last frame when scanout stops. Check the process and
DRM owner rather than trusting the glass:

```bash
pgrep -a sliverd
fuser -v /dev/dri/card*
```

A failed new daemon can leave the previous frame visibly frozen.

## The first frame appears, but updates do not

Command-mode DSI requires a dirty-framebuffer ioctl after changing memory.
Current Sliver calls `dirty_framebuffer` after every repaint. If modifying the
backend, preserve that flush.

## Touch does not react

```bash
ls -l /dev/input/event*
rg 'touch:' /tmp/sliverd.log
```

The adapter looks for the `Mac14,7 Touch Bar` device by name. Verify the
assigned event node with:

```bash
awk '/^N: Name=/{name=$0}/^H: Handlers=/{print name; print}' \
  /proc/bus/input/devices
```

The event number is assigned dynamically and is not a configuration value.

## Fn does not show F1–F12

Check daemon startup logs for both lines:

```text
fn: watching /dev/input/eventN (Apple MTP keyboard)
fn: virtual F-key keyboard ready
```

Then verify input/uinput permissions. The keyboard event node is discovered by
name; `/dev/uinput` must be writable.

## Ctrl+Alt+Fn+F2 does not switch TTY

Use this order:

1. Hold physical Ctrl and Alt.
2. Hold Fn/Globe until F1–F12 appears.
3. Tap F2 on the strip.
4. Release the held keys.

Use a current build containing modifier bridging. Restart `sliverd` after
rebuilding; a previously running process does not gain new code automatically.

## Apply fails from the customizer

Confirm the daemon and socket exist:

```bash
pgrep -a sliverd
ls -l "$XDG_RUNTIME_DIR/sliver.sock"
```

Test the protocol directly:

```bash
./target/release/sliverd --apply sliver.toml
```

## The customizer does not open

Run it in a terminal to see GTK errors:

```bash
./target/release/sliver-edit
```

On Hyprland, portal warnings may indicate a missing or failed
`xdg-desktop-portal` backend, but they are not normally fatal to this app.
Required runtime libraries are GTK4 and libadwaita.

## Return to tiny-dfr

```bash
pkill -INT -x sliverd
sudo systemctl start tiny-dfr
```

To return to Sliver:

```bash
sudo systemctl stop tiny-dfr
./target/release/sliverd sliver.toml --drm
```

## Collect a useful diagnostic bundle

```bash
uname -a
fastfetch --logo none
pgrep -a 'sliverd|tiny-dfr'
fuser -v /dev/dri/card*
awk 'BEGIN { RS="" } /Touch Bar|Apple MTP keyboard|Sliver Function Row/' \
  /proc/bus/input/devices
cat /tmp/sliverd.log
```
