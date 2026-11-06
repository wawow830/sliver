# Touch Bar hardware

The hardware facts sliver's code relies on, and the tiny-dfr 0.3.7 behaviour it matches or
replaces. Facts about the Mac14,7 come from that machine's sysfs, udev and evdev, and from runs
through the safe-swap harness. Real-hardware runs go only through that harness
(`docs/safe-swap.md`).

## Touch Bar models
sliverd discovers everything at runtime: the card from the DRM driver (`adp`/`appletbdrm`) and a
portrait mode, the geometry from the connector mode, the touch device by seat and name, the
backlight by name, and `max_brightness` from sysfs. Per-model quirks live in one table
(`sliverd::profile::DeviceProfile`), and the fake backend can simulate each row.

| family | models | drivers | bar px | physical Esc |
|---|---|---|---|---|
| T1 (2016–17) | MacBookPro13,2 13,3 14,2 14,3 | T1 iBridge; not covered by mainline `appletbdrm`/`hid-appletb-*` (they match T2 `05ac:8302`) | 2170 | no |
| T2 (2018–19) | MacBookPro15,1 15,2 15,3 15,4 | `appletbdrm` (USB `05ac:8302`), `hid-appletb-kbd`, `hid-appletb-bl`, `hid-multitouch` | 2170 | no |
| T2 16" (2019) | MacBookPro16,1 16,4 | as T2 | 2008 | yes |
| T2 13" (2020) | MacBookPro16,2 16,3 | as T2 | 2008 | yes |
| Apple Silicon M1 | MacBookPro17,1 (J293) | `adp`, `apple_z2` (SPI), `panel-summit` | 2008 | yes |
| Apple Silicon M2 | Mac14,7 (J493) | `adp`, `adpdrm_mipi`, `apple_z2`, `panel-summit` | 2008 | yes |

Only the Mac14,7 row has been observed directly; the others follow tiny-dfr's rules, the kernel
drivers (`apple_z2` binds `apple,j293-touchbar` and `apple,j493-touchbar`) and Apple's hardware
history. None of these Macs has a physical F-key row, so the F-keys exist only on the bar.

What differs per family:
- **Esc:** the 2170 px bars have no physical Esc, so the built-in safe layer puts Esc first there
  (tiny-dfr's rule: Esc is added when the bar is at least 2170 px). On 2008 px bars it's optional.
- **Backlight:** Apple Silicon `panel-summit` is a real 0–255 range. T2 `appletb_backlight` has
  `max_brightness` 2 (0 off, 1 dim, 2 full; the driver maps them to HID values 4/3/1). Adaptive
  brightness and curves on T2 can only choose dim or full, and never 0 while the bar is on.
- **Pixel format and damage:** `appletbdrm` scans out BGR888 natively and emulates XRGB8888. It
  uses the atomic damage helpers, so DIRTYFB clips become partial USB transfers; damage tracking
  saves USB bandwidth there too. Geometry and pixel format come from the device, so neither is
  assumed.
- **Kernel fallback on T2:** `hid-appletb-kbd` has modes `0=ESC 1=FN 2=SPCL (media, default) 3=OFF`
  (module parameter `mode`, plus `fntoggle`, `autodim`, `dim_timeout` 60 s, `idle_timeout` 15 s).
  In ESC/FN/SPCL the firmware draws a basic bar and the driver turns touches into keys, when no
  daemon drives `appletbdrm`. Apple Silicon has no such fallback: with no daemon the bar is blank.
- **T2 USB configuration:** the display and touch interfaces appear only after udev switches
  iBridge `05ac:8302` from USB configuration 1 to 2. sliver's udev rule does this when
  tiny-dfr's isn't there.
- **Touch device names:** `Apple Inc. Touch Bar Display Touchpad` (T2), `MacBookPro17,1 Touch Bar`
  (M1), `Mac14,7 Touch Bar` (M2). Match by seat (`ID_SEAT=seat-touchbar`) first and by name
  second; tiny-dfr matches the substring `" Touch Bar"`, which the T2 name also contains.
- **Main-display backlight** for adaptive brightness: `apple-panel-bl` (max 509) on Apple Silicon,
  `gmux_backlight`/`intel_backlight`/`acpi_video0` on Intel. Normalize by *its* `max_brightness`
  (tiny-dfr hardcodes 509).
- **Battery:** Apple Silicon `macsmc-battery/capacity`; tiny-dfr computes `charge_now/charge_full`
  on x86_64. The agent uses UPower.
- **Fn key:** Apple Silicon reports it from the `Apple MTP keyboard`; T2 from the internal USB
  keyboard (`hid-apple`), which `hid-appletb-kbd` also watches.
- **Rotation** is read from the display mode (portrait modes are rotated), whichever way the
  driver reports the panel.

## The Mac14,7
| thing | value |
|---|---|
| bar DRM | `card1` (`root:video 0660`), driver `adp` (+`adpdrm_mipi`), connector `DSI-1`, single mode `60x2008` (portrait) |
| main DRM | `card3` (`apple-drm`, `eDP-1`); GPU render node `asahi` |
| bar touch | `Mac14,7 Touch Bar`, `apple_z2` over SPI, `INPUT_PROP_DIRECT`, `ID_INPUT_TOUCHSCREEN=1`, MT slots, 230x6 mm, seat `seat-touchbar` |
| keyboard | `Apple MTP keyboard` (05ac:0354), reports `KEY_FN` (464) and `KEY_ESC` |
| lid | `Apple SMC power/lid events`, `SW_LID` |
| bar backlight | `/sys/class/backlight/228600000.dsi.0`, `panel-summit`, max 255, type raw |
| main backlight | `/sys/class/backlight/apple-panel-bl`, `apple-dcp`, max 509 |
| kbd backlight | `/sys/class/leds/kbd_backlight`, max 255 |
| battery | `/sys/class/power_supply/macsmc-battery` (+ `macsmc-ac`) |

## Display
- `card1` (`adp`): DUMB_BUFFER, preferred depth 24, no async page flip; UNIVERSAL_PLANES and ATOMIC
  client caps are accepted. Once no one else holds it, a non-root process becomes DRM master just
  by opening the card. Only one process can be master; tiny-dfr holds it for its whole lifetime.
- One connector (`DSI`): mode `60x2008@60`, **`non-desktop=1`** (why compositors leave it alone).
  One CRTC (ACTIVE, MODE_ID, OUT_FENCE_PTR, VRR_ENABLED). One primary plane: **`XR24` only, no
  `FB_DAMAGE_CLIPS`, no `rotation`**. So sliver rotates in software (draw landscape 2008x60, rotate
  90°: tiny-dfr's `translate(60, 0); rotate(90°)`), and partial updates go through legacy
  `DIRTYFB` with clip rects in portrait framebuffer coordinates.
- No page flipping, no vblank, no double buffering: write into the mapped dumb buffer, then
  DIRTYFB. The flush is what makes the panel update (on T2, appletbdrm turns it into a USB
  transfer; on Apple Silicon, adp drives a command-mode MIPI DSI panel that also needs it).
- **`ACTIVE=0` does not blank the panel.** The commit returns in ~80 µs but the bar keeps its last
  frame: the panel is command-mode and holds its own copy, which is also why it keeps the last image
  while nobody owns the card. "Display off" is backlight 0.
- **Buffer stride:** a 60x2008 dumb buffer (240-byte pitch) shows a static but garbled picture,
  while a CPU readback of the same buffer is perfect. tiny-dfr allocates 64x2008 and scans out 60;
  `sliver_hw::panel` likewise pads the width to a multiple of 64 px (256-byte pitch). A readback
  (`bar snap`) shows the buffer, not the panel, so visuals are confirmed by eye.
- **Timing:** the atomic modeset commits in ~16 ms. A full-frame DIRTYFB takes up to ~34 ms
  (about two refreshes), so full-bar changes run at about 30 fps and damage is tracked per item to
  keep small updates within a 60 Hz frame.

## Touch
`Mac14,7 Touch Bar`:
- ABS_X/MT_POSITION_X 0..23044, ABS_Y/MT_POSITION_Y 0..639, **100 units/mm**: an active area of
  230.4 x 6.4 mm, ~11.5 units per pixel along the bar and ~10.7 across (about 8.7 px/mm).
- **X runs left to right** as the user sees it. Map with `lx = x * 2008 / 23045`. Contacts reach
  within ~2 px of both ends. Y spans ~18..581 in practice.
- MT_SLOT 0..255 (the kernel's cap, not a finger limit), TRACKING_ID 0..65535, TOUCH_MAJOR/MINOR,
  WIDTH_MAJOR/MINOR, ORIENTATION. **No pressure, no hover.** Contact size values are noisy (minor >
  major, odd units), so nothing is built on them.
- **3 simultaneous contacts register reliably.** Slots 0..2 are reused per contact with a fresh
  TRACKING_ID.
- **Reports at ~60 Hz** (median 17 ms between SYN_REPORTs). Quick taps last 50–170 ms.
- The bar is 6 mm tall, so vertical swipes aren't usable.
- Targets: tiny-dfr's F-keys are ~152 px (17 mm) with 16 px gaps. 80 px (~9 mm) is a comfortable
  target and 60 px the floor, so at most ~22 targets fit.

## Keys out
A uinput keyboard. tiny-dfr's is `Dynamic Function Row Virtual Input Device` (bus 0x19,
`1209:316e`); sliverd's is `sliver-touch-bar-keys`, with every key registered up front. Events are
`EV_KEY` 1/0 then `SYN_REPORT`. The device sits on seat0 like any keyboard, so the compositor
applies the user's xkb layout to it, and key repeat is the compositor's: sliverd sends key-down on
touch and key-up on release.

## Fn, lid and activity
The Fn key and the lid switch come from **seat0** (`Apple MTP keyboard` has `KEY_FN`; `Apple SMC
power/lid events` has `SW_LID`). They're read without grabbing. Any seat0 or bar input counts as
activity for the idle timer; opening the lid counts too.

## Backlight and adaptive brightness
- The bar backlight is raw 0–255 on Apple Silicon. It's written as `"<n>\n"` to its sysfs
  `brightness`. While it's 0, touches don't fire anything: the first touch only wakes the bar.
- tiny-dfr's adaptive brightness is `sqrt(main / 509) * ActiveBrightness + 1`, clamped to 255,
  with `ActiveBrightness` 128 by default. sliver uses the same curve (with the main backlight's
  real maximum) unless `[display] curve` replaces it.
- Idle: 30 s after the last activity → 1 (dim), 60 s → 0 (off). Lid closed → 0.
- Every sysfs write to the main display's backlight makes the kernel send a backlight `change`
  uevent (`SOURCE=sysfs`), so sliverd's udev monitor re-applies the bar's brightness without
  polling.
- Fedora's `systemd-backlight@backlight:228600000.dsi.0.service` saves and restores the bar's
  brightness across boots.
- The bar comes back lit, at the same brightness and with a correct picture, after s2idle suspend.

## udev and systemd wiring
tiny-dfr's Fedora package ships:
- `99-touchbar-seat.rules`: DRM `card*` with `DRIVERS=="adp|appletbdrm"` gets `TAG-="master-of-seat"`
  and `ID_SEAT=seat-touchbar`; the three touch inputs get `ID_SEAT=seat-touchbar`. logind never
  creates that seat, so no ACLs are applied and the compositor (seat0) ignores the bar. The seat
  name only matters to libinput.
- `99-touchbar-tiny-dfr.rules`: the T2 USB configuration switch; `SYSTEMD_WANTS=tiny-dfr.service`
  on the three touch inputs (that's how tiny-dfr starts: the unit is static, never enabled); and
  device aliases for the bar card, the bar backlight and the main backlight, which the unit
  `BindsTo=`.
- `tiny-dfr.service`: `Restart=always`, a strict sandbox, starts as root and drops itself to
  `nobody` with groups `video,input`. Because udev pulls it in, stopping it can be undone by any
  udev `change` event.

sliverd's packaging (ADR-0005) mirrors this with its own rule, aliases and `SYSTEMD_WANTS`, and a
handover so exactly one of the two daemons runs.

Installed, sliverd runs in `/system.slice/sliverd.service` as seen from the host, but
`ProtectControlGroups=strict` gives it a private cgroup namespace, so its own `/proc/self/cgroup`
reads `0::/`, and its uid is a DynamicUser one. The hardware gate accepts that pair (a gate that
only accepted the host path would crash-loop sliverd into the tiny-dfr fallback). Sandbox-visible
facts are checked from inside the sandbox.

## tiny-dfr behaviour sliver matches
- **Layers:** `[primary (F-keys), media]`, swapped by `MediaLayerDefault = true`. Holding Fn on any
  seat0 keyboard shows layer 1. Esc is inserted first in both layers only on bars ≥ 2170 px.
- **Touch:** a touch that lands on a button (inside its span, within 10–90% of the height) presses
  its key and holds it while the finger is down; sliding off releases it and sliding back presses
  it again; slots are independent. Combos press all keys in order with one SYN. tiny-dfr releases
  them in the same order; sliver releases in reverse.
- **Look** (the `classic` theme and the importer): N slots by `Stretch`, 16 px between buttons,
  button box from 15% to 85% of the height with radius 8, gray 0.2 inactive / 0.4 pressed, white
  32 px font (`FontTemplate`, default `:bold`), 48 px icons.
- **Pixel shift:** up to ±11 px along the bar and ±2 px across it. sliver moves 1 px per 15 s along
  the same path (resting 60 s at the ends), so content kept 11 px from both ends is never clipped,
  and maps touches back.

### Config
What the importer (`sliver_config::tinydfr`) reads:
- `/usr/share/tiny-dfr/config.toml` merged with `/etc/tiny-dfr/config.toml` **per top-level key**
  (a user `PrimaryLayerKeys` replaces the whole list).
- Keys: `MediaLayerDefault`, `ShowButtonOutlines`, `EnablePixelShift`, `FontTemplate`,
  `AdaptiveBrightness`, `ActiveBrightness`, `PrimaryLayerKeys`, `MediaLayerKeys`.
- A button is `Text` | `Icon` (alias `Svg`) [+ `Theme` for XDG lookup] | `Time` (`24hr`, `12hr` or a
  strftime string, + `Locale`) | `Battery` (`percentage|icon|both`) | none (a spacer); `Action` is a
  key name or an array of them (`input_linux::Key` names: `F1`, `BrightnessDown`, `PlayPause`,
  `IllumUp`, `MicMute`, `Search`…); `Stretch` ≥ 1.
- Icons without a theme: `/etc/tiny-dfr/<name>.{svg,png}`, then `/usr/share/tiny-dfr/…`.
- The default media layer is BrightnessDown/Up, MicMute, Search, IllumDown/Up, PreviousSong,
  PlayPause, NextSong, Mute, VolumeDown/Up.

## Failure behaviour (why invariant 1 needs a daemon of its own)
tiny-dfr treats config errors as panics (a bad icon name, a bad battery mode, an empty layer, a bad
time format). `main()` catches the panic, paints an embedded crash bitmap, and then blocks waiting
for SIGTERM forever **while still DRM master**. The process doesn't exit, so `Restart=always` never
fires: a config typo leaves the user with no Esc and no F-keys until someone restarts the service.
Panics before that (no card) exit nonzero, and systemd's default rate limit gives up after 5 starts
in 10 s. It also wakes at least every 10 s while idle, to poll its dim and off timers.

So sliverd has a built-in safe layer drawn by code with no config, icon or font dependency, takes
it the moment the agent misbehaves, and exits for real when it can't run, so systemd restarts it
and, failing that, hands the bar back to tiny-dfr.

## Idle wakeups
`scripts/idle-wakeups` runs sliverd and sliver-agent with the default config on private buses, 30 s
idle with the bar on and 30 s with it off. Budgets: on ≤ 8/s, off ≤ 3/s.
- sliverd: 0 wakeups in both.
- The agent stays within budget because of two fixes. The audio feed only resets its backoff for a
  `pactl subscribe` that stays up 10 s, else backs off to 30 s. And `blocking` 1.7, which zbus uses
  to connect and close Unix sockets, parks an idle pool thread in a 500 ms `wait_timeout` loop it
  never leaves below the pool limit (2 wakeups/s; `BLOCKING_MAX_THREADS=1` doesn't help).
  `crates/sliver-bus` connects the socket itself and gives zbus a write half that shuts down
  directly, so the pool never starts. Every D-Bus connection in the agent goes through it.
- The real system bus is chattier than the private-bus harness shows: NetworkManager, BlueZ and
  UPower emit `PropertiesChanged` every few seconds, about 1.9 wakeups/s with the subscriptions on.
  With the bar off the agent drops those subscriptions (logind stays), which brings it to ~0.03/s.
  `cargo run -p sliver-session --example idle-probe -- 40 session` with
  `DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket` measures it; `idle-probe` also
  takes `audio`, `notifications`, `tray`, `appearance`, `screensaver` and `dark` to bisect sources.

## Test sessions
A second sliver-agent on the real session bus takes `io.github.wawow830.Sliver` from the installed
one, and when it exits systemd stops the installed agent (`Type=dbus`, `BusName=`). Test agents run
on their own bus (`dbus-run-session`).

## License (for reuse)
- tiny-dfr code is MIT ("Copyright (c) 2023 WhatAmISupposedToPutHere" / The Asahi Linux
  Contributors). Code derived from it keeps that notice.
- The icons in `share/tiny-dfr/*.svg` are Google Material Design Icons, Apache-2.0 (some edited by
  kekrby), shipped with the Apache notice.
