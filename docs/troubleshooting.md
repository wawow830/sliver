# Troubleshooting

## Check ownership

Only one process can own the Touch Bar DRM device. Inspect the broker and its
current owner:

```sh
systemctl status sliver-broker.service
pgrep -a 'sliver-broker|tiny-dfr'
fuser -v /dev/dri/card*
```

Stop `tiny-dfr` before enabling Sliver. Stopping Sliver does not start
`tiny-dfr` automatically.

## Check permissions

The broker account needs access to the DRM card, Touch Bar and keyboard evdev
nodes, `/dev/uinput`, and the DSI backlight:

```sh
id sliver
ls -l /dev/dri/card* /dev/input/event* /dev/uinput
journalctl -u sliver-broker.service -b
```

Applying users need membership in `sliver-supervisors`. Start a new login
session after changing group membership.

## Check the apply socket

The public client talks to the per-user supervisor at:

```sh
ls -l "$XDG_RUNTIME_DIR/sliver/supervisor.sock"
journalctl --user -u sliver-supervisor.service -b
```

`sliver FILE` reports load, validation, worker, authorization, and hardware
errors on stderr. A missing or broken saved path remains selected and puts the
fixed recovery row on the panel; it does not choose another source.

## No frame or stale frame

A command-mode DSI panel can retain its last frame after a process exits. Check
the journal and DRM owner rather than trusting the glass:

```sh
journalctl -u sliver-broker.service -b
journalctl --user -u sliver-supervisor.service -b
fuser -v /dev/dri/card*
```

The tested M2 adapter must flush every repaint with the dirty-framebuffer
ioctl. A missing display, touch input, Fn observer, synthetic-key device, or
backlight is reported by capability name and blocks apply until discovery
succeeds again.

## Touch, Fn, or key output fails

The adapter discovers devices by hardware identity rather than a fixed event
number. Check the broker journal and input devices:

```sh
journalctl -u sliver-broker.service -b | grep -E 'display|touch|Fn|synthetic|backlight'
awk '/^N: Name=/{name=$0}/^H: Handlers=/{print name; print}' \
  /proc/bus/input/devices
```

Holding physical Fn/Globe continuously for exactly two seconds should show the
compiled F1–F12 recovery row. This physical-hold timer is separate from the
Lua callback watchdog, which also has a two-second deadline. Physical Ctrl, Alt,
Shift, and Super modifiers are bridged to virtual key taps. Synthetic keys are
released whenever a worker fails or is replaced.

## Suspend or device loss

During suspend the worker is hidden, contacts are canceled, timers pause, and
the backlight turns off. Resume reacquires hardware and requests one fresh
frame. For device loss, wait for the broker's rediscovery attempt and inspect
both service journals.

## Return to tiny-dfr

Sliver does not restart another Touch Bar daemon during shutdown. Hand ownership
back explicitly:

```sh
sudo systemctl stop sliver-broker.service
sudo systemctl start tiny-dfr.service
```
