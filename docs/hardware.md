# Touch Bar hardware and tiny-dfr internals

Owner: birch (ask me before re-reading all of this). Sources: tiny-dfr **v0.3.7**
(`github.com/AsahiLinux/tiny-dfr`, tag `v0.3.7`, commit `6267754`), the installed Fedora package
`tiny-dfr-0.3.7-3.fc44`, and read-only inspection of this machine (sysfs, udev, `/proc`, journal)
on 2026-10-12. Nothing was opened, stopped or written.

## TL;DR for implementers
- **Several Touch Bar variants exist** (T1, T2 2170/2008 px, M1, M2). See "Touch Bar models".
  Everything below is for *this* Mac14,7. Discover at runtime and keep quirks in a profile table.
- Display: DRM **`/dev/dri/card1`** (driver `adp`, `platform-228200000.display-pipe`), connector
  `DSI-1`, single mode **60x2008** (portrait: 60 wide, 2008 tall). Draw landscape, rotate 90°.
- Touch: **`Mac14,7 Touch Bar`** (`/dev/input/event2`, driver `apple_z2` over SPI), libinput seat
  **`seat-touchbar`**. It's a touchscreen (`ID_INPUT_TOUCHSCREEN=1`), MT slots, 230x6 mm.
- Fn key and lid switch come from **seat0** (`Apple MTP keyboard` = event1 has `KEY_FN`;
  `Apple SMC power/lid events` = event3 has `SW_LID`).
- Bar backlight: **`/sys/class/backlight/228600000.dsi.0`** (`panel-summit`, max 255, `raw`).
  Main screen: `/sys/class/backlight/apple-panel-bl` (`apple-dcp`, max 509).
- Keys out: a uinput device named `Dynamic Function Row Virtual Input Device` (bus 0x19,
  vendor 0x1209, product 0x316e) is created by tiny-dfr. Ours must have its own name/ID.
- Only one process can be DRM master on card1. tiny-dfr holds it for its whole lifetime.
- **Esc question:** tiny-dfr only adds an Esc button when the bar is ≥2170 px long. This bar is
  2008, which matches Apple's 2020+ 13" design that has a **physical Esc key** beside the bar.
  **Confirmed by the human (H2): this Mac has a physical Esc.** There are **no physical F-keys**,
  so the invariant stands.

## Hazards for peers (read this)
- Our user `wawow` is in the **`input` (104) and `video` (39)** groups. So peers *can* open
  `/dev/input/event2`, `/dev/dri/card1` and `/dev/uinput` without sudo. **Don't.** Opening card1
  read/write while tiny-dfr is master is mostly harmless, but `EVIOCGRAB` on event2, a modeset,
  or a stray uinput device that injects keys into the live Hyprland session are not. Use fakes;
  real runs only go through the safe-swap harness (T3).
- Reading sysfs attributes and `udevadm info` is fine. `evtest`/`libinput debug-events` on the
  bar devices is **not** (it opens them). Ask with `--kind hardware` first.
- `tiny-dfr.service` has `Restart=always` and is pulled in by udev (`SYSTEMD_WANTS`), so killing
  it is pointless and stopping it can be undone by any udev `change` event. See T3 design.

## This machine
| thing | value |
|---|---|
| model | MacBook Pro 13" M2, `Mac14,7`, kernel `7.1.13-402.asahi.fc44.aarch64+16k` |
| bar DRM | `card1` (226:1, `root:video 0660`, no ACL), driver `adp` (+`adpdrm_mipi`), `DSI-1` connected, mode `60x2008` |
| main DRM | `card3` (`apple-drm`, `eDP-1` 2560x1600); GPU render node `card2`/`renderD128` (`asahi`) |
| bar touch | `input2`/`event2` "Mac14,7 Touch Bar", phys `apple_z2`, bus 0x1c, `PROP=2` (INPUT_PROP_DIRECT), `EV=b` (SYN,KEY,ABS), `KEY=BTN_TOUCH`, ABS X,Y + MT SLOT, TOUCH_MAJOR/MINOR, WIDTH_MAJOR/MINOR, ORIENTATION, POSITION_X/Y, TRACKING_ID (**no PRESSURE, no DISTANCE/hover**); `ID_INPUT_WIDTH_MM=230`, `HEIGHT_MM=6`; `by-path/platform-23510c000.spi-cs-0-event` |
| keyboard | `input1`/`event1` "Apple MTP keyboard" (dockchannel HID, 05ac:0354), reports `KEY_FN` (464) and `KEY_ESC` |
| trackpad | `input0`/`event0` "Apple MTP multi-touch" (tiny-dfr opens it via seat0, see below) |
| lid | `input3`/`event3` "Apple SMC power/lid events", `SW_LID` |
| tiny-dfr uinput | `input5`/`event5` "Dynamic Function Row Virtual Input Device" |
| bar backlight | `228600000.dsi.0`, `panel-summit`, max 255, type raw, currently ~62 |
| main backlight | `apple-panel-bl`, `apple-dcp`, max 509 |
| kbd backlight | `/sys/class/leds/kbd_backlight` (max 255) |
| battery | `/sys/class/power_supply/macsmc-battery` (+ `macsmc-ac`) |
| hid_apple | `fnmode=3`, no swaps |
| tiny-dfr proc | runs as `nobody` (65534) with groups `video,input`, no caps |
| config | `/usr/share/tiny-dfr/config.toml` only; `/etc/tiny-dfr/config.toml` absent (defaults) |

Kernel has `apple_z2` (touch), `adpdrm` + `adpdrm_mipi` (display pipe), `panel_summit` (bar
panel/backlight). Physical DRM properties (planes, formats, damage support) need an open card
to query, so that's a first harness-run item.

## udev + systemd wiring (Fedora package)
`/usr/lib/udev/rules.d/99-touchbar-seat.rules`:
- DRM `card*` with `DRIVERS=="adp|appletbdrm"`: `TAG-="master-of-seat"`, `ID_SEAT=seat-touchbar`.
- Inputs named `Apple Inc. Touch Bar Display Touchpad` (T2), `MacBookPro17,1 Touch Bar` (M1),
  `Mac14,7 Touch Bar` (M2): `ID_SEAT=seat-touchbar`.
- Effect: logind never creates the seat (no `master-of-seat`), so `loginctl` doesn't list it and
  no ACLs get applied. The compositor (seat0) ignores the bar entirely. The seat name only
  matters to libinput (`udev_assign_seat("seat-touchbar")`).

`/usr/lib/udev/rules.d/99-touchbar-tiny-dfr.rules`:
- T2 only: USB `05ac:8302` config 1 → set `bConfigurationValue` 0 then 2 (exposes the
  display + touch interfaces).
- The three touch inputs: `TAG+="systemd", ENV{SYSTEMD_WANTS}="tiny-dfr.service"`. **This is how
  tiny-dfr gets started** (the unit is `static`, never enabled).
- DRM card (adp|appletbdrm) → systemd alias `/dev/tiny_dfr_display`.
- Bar backlight (`appletb_backlight`/hid-appletb-bl on T2; `228200000.display-pipe.0` or
  `228600000.dsi.0`/panel-summit on AS) → `/dev/tiny_dfr_backlight`.
- Main backlight (`apple-panel-bl`, `gmux_backlight`, `intel_backlight`, `acpi_video0`) →
  `/dev/tiny_dfr_display_backlight`.

`/usr/lib/systemd/system/tiny-dfr.service`:
```
After=systemd-user-sessions.service getty@tty1.service plymouth-quit.service systemd-logind.service dev-tiny_dfr_*.device
BindsTo=dev-tiny_dfr_display.device dev-tiny_dfr_backlight.device dev-tiny_dfr_display_backlight.device
ExecStart=/usr/bin/tiny-dfr
Restart=always
NoNewPrivileges ProtectSystem=strict ProtectHome PrivateTmp PrivateIPC ProtectKernelTunables
ProtectKernelModules ProtectKernelLogs ProtectControlGroups=strict
RestrictAddressFamilies=AF_UNIX AF_NETLINK RestrictNamespaces RestrictSUIDSGID
```
Plus Fedora's global drop-in `TimeoutStopFailureMode=abort`. Runs as root, then drops itself.
`/sys` stays writable in its namespace (backlight writes work). Upstream also ships two empty
`systemd-backlight@backlight:{228200000.display-pipe.0,appletb_backlight}.service` files meant
as masks. Fedora doesn't install them, so `systemd-backlight@backlight:228600000.dsi.0.service`
saves/restores the bar brightness across boots here.

**For sliverd:** reuse the same seat rule (or ship our own equivalent), our own
`SYSTEMD_WANTS`, the same `BindsTo` device aliases (use our own alias names), and
a handover so exactly one runs (ADR-0005: `ExecStartPre=` stops in both directions; `Conflicts=` turned
out unsafe with a start limit). Coexistence = exactly one of the two units wants the device.

## How tiny-dfr works (v0.3.7, ~1800 lines, single-threaded)

### Startup order (`main.rs`)
1. `DrmBackend::open_card()` as root (below).
2. Open `/dev/uinput` (write), find and open the bar backlight `brightness` file (write), load
   config + fonts + icons.
3. `privdrop`: become `nobody`, groups `input,video`. Everything after runs unprivileged.
4. Two libinput contexts via udev: `seat-touchbar` (the digitizer) and **`seat0`** (all
   keyboards/trackpads/lid, used for Fn, activity tracking and the lid switch). It reads seat0
   devices without grabbing them.
5. udev monitor on `power_supply` (battery refresh), inotify on `/etc/tiny-dfr/config.toml`.
6. Register **only the keys used in the config** on uinput (`set_keybit`), then create the
   device. (Upstream main later registers every key so reloads don't need a restart.)
7. One `epoll` loop over: seat0 libinput, bar libinput, inotify, udev monitor.

### DRM acquisition (`display.rs`)
- Iterates **every** `/dev/dri/card*`. For each: open RW, `UNIVERSAL_PLANES` + `ATOMIC` caps,
  `acquire_master_lock()` (SET_MASTER), take the first connected connector, first mode, and
  reject unless `height/width >= 30` ("does not look like a touchbar"). On the main card the
  master lock fails because the compositor holds it.
- Creates **one dumb buffer**, `XRGB8888`, 32 bpp, size **(64, 2008)**. Note 64, not 60: the
  width gets padded. Cairo draws into a 64x2008 `ARgb32` image surface, and only 60 columns
  are scanned out (`SRC_W/CRTC_W` = mode width).
- `add_framebuffer(depth 24, bpp 32)`. Then one atomic commit with `ALLOW_MODESET` that sets
  connector `CRTC_ID`, CRTC `MODE_ID` (blob) + `ACTIVE`, and the first plane's `FB_ID`, `CRTC_ID`,
  `SRC_*` (16.16), `CRTC_*`. It uses the first CRTC and the first plane (fine here, there's only one).
- **No page flipping, no vblank, no double buffering.** It renders into a cairo surface,
  `memcpy`s the whole thing into the mapped dumb buffer, then calls **`DRM_IOCTL_MODE_DIRTYFB`**
  with clip rects. Dirty FB is what makes the panel update (on T2, appletbdrm turns it into a USB
  transfer; on AS, adp is a command-mode MIPI DSI panel that also needs the flush).
- On `Drop` it destroys the fb and dumb buffer. Master goes away when the fd closes, i.e. when
  the process exits.

### Rendering + rotation
- Cairo context: `translate(height=60, 0)` then `rotate(90°)`, so code draws in landscape
  2008x60 coordinates.
- Layout: N "virtual" button slots (sum of `Stretch`), `BUTTON_SPACING_PX=16`, button box from
  15% to 85% of the height with radius 8, gray 0.2 inactive / 0.4 active, white 32 px font from
  fontconfig (`FontTemplate`, default `:bold`), 48 px icons (SVG via librsvg, PNG scaled to 48).
- Damage: a full redraw paints black and clips the whole fb. A partial redraw repaints only
  buttons with `changed=true`, giving one clip rect per button. The clip rects are in
  **portrait/fb coordinates** (x across the 60, y along the 2008).
- Redraw triggers: a touch state change, a layer change (Fn), a config reload, the clock
  minute/second ticking over (only if a Time button is visible), pixel shift steps. A battery
  button is marked changed on *every* loop iteration, so with a battery widget it redraws
  whenever any input event wakes the loop.
- Wakeups: the epoll timeout is `min(10 s, ms to next minute)`. So it wakes at least every 10 s
  even when idle, because the backlight dim/off logic is polled. That's an invariant-3
  (power) improvement for us: use timerfds for the dim/off deadlines.
- Pixel shift (optional): ±11 px horizontally, ±2 px vertically, one px per ~4 s animation
  every 10 s, waits 50 s at the ends.

### Touch handling
- Only `Touch` events from the device whose name contains `" Touch Bar"` count. Touches are
  ignored while the backlight is 0 (the first touch just wakes the bar, see backlight).
- Coordinates: `x_transformed(2008)`, `y_transformed(60)`. libinput already maps the long axis
  to x.
- Down: hit-test against the active layer (x inside the button span, y within 10–90% of the
  height). If it hits, store `slot → (layer, button)`, mark active, **emit key down**.
- Motion: re-hit-test *that* button. Sliding off releases the key, sliding back presses it again.
- Up: release. The key is held for as long as the finger is down, so autorepeat comes from the
  compositor. Multi-touch works: independent slots, independent buttons.
- Combos (`Action = ["LeftCtrl","LeftAlt","F1"]`): all keys down in order then one SYN, and all
  keys up in the same order (not reversed).
- No gestures, long-press or sliders.

### Keys out (uinput)
`input_event`s with zero timestamps, `EV_KEY` value 1/0, then `SYN_REPORT`. The device is
`Dynamic Function Row Virtual Input Device`, `BUS_HOST`-ish 0x19, `1209:316e`, version 1. It sits
on seat0 like any keyboard, so the compositor applies the user's xkb layout to it.

### Fn and layers
- Two layers: `[primary(F-keys), media]`, swapped if `MediaLayerDefault=true`.
- `KEY_FN` press on any seat0 keyboard → layer 1 while held, release → layer 0. That's it. No
  latch, no per-app logic. (Upstream main added `DoublePressSwitchLayers`.)
- Esc: inserted at index 0 of both layers **only when the bar length is ≥ 2170 px** (the older
  bars with no physical Esc). Not inserted on this 2008 px bar.

### Backlight / adaptive brightness (`backlight.rs`)
- Bar backlight found by name: `display-pipe`, `228600000.dsi.0`, `appletb_backlight`. Main
  backlight: `apple-panel-bl`, `gmux_backlight`, `intel_backlight`, `acpi_video0`. Writes
  `"<n>\n"` to the bar's `brightness`. 0.3.7 **panics** if either is missing (upstream main
  made the display backlight optional).
- Any seat0/bar input event (keyboard, pointer, gesture, touch) counts as activity.
- Active brightness: with `AdaptiveBrightness`, `sqrt(main/509) * ActiveBrightness + 1`
  (clamped to 255). The `509` is hardcoded (wrong for Intel). The main backlight is re-read
  every loop iteration. Without it, the brightness is `ActiveBrightness` (default 128).
- Idle: 30 s after the last activity → brightness 1 (dim), 60 s → 0 (off). Lid closed
  (`SW_LID` on) → 0. Opening the lid counts as activity.
- Note: brightness 0 just turns off the backlight. The panel and DRM stay up and nothing
  stops rendering.

### Config
- Base `/usr/share/tiny-dfr/config.toml` merged with `/etc/tiny-dfr/config.toml`, **per top-level
  key** (a user `PrimaryLayerKeys` replaces the whole list).
- Keys: `MediaLayerDefault`, `ShowButtonOutlines`, `EnablePixelShift`, `FontTemplate`,
  `AdaptiveBrightness`, `ActiveBrightness`, `PrimaryLayerKeys`, `MediaLayerKeys`.
- Button: `Text` | `Icon` (alias `Svg`) [+ `Theme` for XDG lookup] | `Time` (`24hr`, `12hr` or a
  strftime string, + `Locale`) | `Battery` (`percentage|icon|both`) | none (spacer); `Action`
  = a key name or an array (`input_linux::Key` names, e.g. `F1`, `BrightnessDown`, `PlayPause`,
  `IllumUp`, `MicMute`, `Search`); `Stretch` ≥1.
- Icon lookup without a theme: `/etc/tiny-dfr/<name>.{svg,png}`, then `/usr/share/tiny-dfr/...`.
- Hot reload: an inotify `IN_CLOSE|IN_MOVED_TO` oneshot on the user file, re-armed after each
  event. If the file doesn't exist it polls by re-arming each loop iteration.
- Errors are **panics**: a bad icon name, a bad battery mode, an empty layer, a bad time format.
  So a typo in the config sends tiny-dfr into its crash screen (below).
- Importer note for us: the default media layer uses keycodes, not actions:
  BrightnessDown/Up, MicMute, Search, IllumDown/Up, PreviousSong, PlayPause, NextSong, Mute,
  VolumeDown/Up.

### Failure behavior (important for invariant 1)
`main()` wraps `real_main` in `catch_unwind`. On **any panic** it paints the embedded
`crash_bitmap.raw` (1 bpp, 64x2008 = 16064 bytes) into the dumb buffer, flushes, and then
**blocks waiting for SIGTERM forever while still DRM master**. The process doesn't exit, so
`Restart=always` never fires. **A config typo leaves the user with no Esc/F-keys until someone
restarts the service.** sliverd must do better: a built-in safe layer rendered by code that has
no config/icon/font dependency (or a pre-rendered bitmap), and a real exit when things go
wrong so systemd can restart us.

`panic`s at startup (before `catch_unwind`, e.g. no card) exit nonzero → systemd restarts with
default rate limits (5 starts in 10 s, then the unit gives up).

## Touch Bar models (don't hardcode this machine)
This laptop is just one of several Touch Bar variants. **sliverd must discover everything at
runtime**: the card from the DRM driver (`adp`/`appletbdrm`) plus the portrait-mode check, the
geometry from the connector mode, the touch device by name or seat, the backlight by name, and
`max_brightness` and the brightness model from sysfs. Keep per-model quirks in one data table
(`DeviceProfile`: match rule → backlight model, whether Esc is needed, kernel fallback, etc.), not
scattered through the code. The fake backend should be able to simulate each row below.

| family | models | chip / drivers | bar px | physical Esc | status |
|---|---|---|---|---|---|
| T1 (2016–17) | MacBookPro13,2 13,3 14,2 14,3 | T1 iBridge. Not covered by mainline `appletbdrm`/`hid-appletb-*` (those match T2 `05ac:8302`), old out-of-tree `apple-ib` drivers | 2170 | **no** | unsupported for now (unverified, no hardware) |
| T2 (2018–19) | MacBookPro15,1 15,2 15,3 15,4 | `appletbdrm` (USB `05ac:8302`) + `hid-appletb-kbd` + `hid-appletb-bl` + `hid-multitouch` | 2170 | **no** | target #2 (t2linux) |
| T2 16" (2019) | MacBookPro16,1 16,4 | same as T2 | 2008 | yes | target #2 |
| T2 13" (2020) | MacBookPro16,2 16,3 | same as T2 | 2008 | yes | target #2 |
| Apple Silicon M1 | MacBookPro17,1 (J293) | `adp` + `apple_z2` (SPI) + `panel-summit` | 2008 | yes | target #1 |
| Apple Silicon M2 | Mac14,7 (J493), **this machine** | `adp` + `adpdrm_mipi` + `apple_z2` + `panel-summit` | 2008 | yes | target #1, the only one we can test |

The px column comes from tiny-dfr's rule (Esc is added when the bar is ≥2170 px) plus Apple's
hardware history. Only the Mac14,7 row has been observed directly. `apple_z2` binds
`apple,j293-touchbar` and `apple,j493-touchbar` (from `modinfo`). Treat everything else as
"believed" until a user with that machine confirms it. That's an `external` ask later (t2linux
community testers). T2 readiness, what's covered headless and a tester's checklist: `docs/t2.md`.

What changes per family, and what sliverd has to handle:
- **Esc:** the 2170 px bars have no physical Esc. The built-in safe layer *must* put Esc first
  there. On the 2008 px bars Esc is optional (still a good default for the safe layer).
- **Backlight model:** AS `panel-summit` is a real 0–255 range. **T2 `appletb_backlight` has
  `max_brightness` = 2** (0=off, 1=dim, 2=full; the driver maps these to HID values 4/3/1). So
  "adaptive brightness" on T2 means choosing between dim and full. tiny-dfr's `min(max_bl)`
  clamp hides this. Read `max_brightness` and scale.
- **Pixel format and damage:** `appletbdrm` natively scans out BGR888 and *emulates*
  XRGB8888. It uses the atomic damage helpers, so DIRTYFB/`FB_DAMAGE_CLIPS` translate to partial
  USB transfers. Damage tracking matters even more on T2 (USB bandwidth and power). Geometry and
  pixel format come from the device (`GET_INFORMATION` msg), so don't assume them.
- **Kernel fallback on T2:** `hid-appletb-kbd` has modes `0=ESC 1=FN 2=SPCL(media, default) 3=OFF`
  (module param `mode`, plus `fntoggle`, `autodim`, `dim_timeout`=60 s, `idle_timeout`=15 s). In
  ESC/FN/SPCL modes the T2 firmware draws a basic bar and the driver turns its touches into key
  events. So on T2 a dead daemon can still leave a usable Esc/F-key bar, which is a possible
  invariant-1 fallback that Apple Silicon doesn't have. How this mode switching interacts with
  `appletbdrm` owning the display needs a T2 tester.
- **T2 USB config:** the bar only shows up after udev switches iBridge `05ac:8302` to USB
  configuration 2 (tiny-dfr's rule). Our packaging must keep that rule (or depend on whatever
  t2linux ships).
- **Touch device names:** `Apple Inc. Touch Bar Display Touchpad` (T2), `MacBookPro17,1 Touch
  Bar` (M1), `Mac14,7 Touch Bar` (M2). Match by seat (`ID_SEAT=seat-touchbar`) first and by name
  second. tiny-dfr matches the substring `" Touch Bar"`, and the T2 name contains it too.
- **Main-display backlight** for adaptive brightness: `apple-panel-bl` max 509 on AS,
  `gmux_backlight`/`intel_backlight`/`acpi_video0` on Intel. Normalize by *its* `max_brightness`
  (tiny-dfr wrongly hardcodes 509).
- **Battery:** AS `macsmc-battery/capacity`. tiny-dfr computes `charge_now/charge_full` on
  x86_64. Prefer UPower in the agent anyway.
- **Fn key source:** AS gets it from the `Apple MTP keyboard`. T2 gets it from the internal USB
  keyboard (`hid-apple`), which `hid-appletb-kbd` also watches for its fntoggle.

## Apple Silicon vs T2 (summary)

| | Apple Silicon (this) | Intel T2 |
|---|---|---|
| display | `adp` DRM driver, MIPI DSI (command mode), on SoC | `appletbdrm`, USB (iBridge 05ac:8302 config 2) |
| touch | `Mac14,7 Touch Bar` / `MacBookPro17,1 Touch Bar` (`apple_z2`, SPI) | `Apple Inc. Touch Bar Display Touchpad` (`hid-multitouch` over USB) |
| bar backlight | `228600000.dsi.0` or `228200000.display-pipe.0` (`panel-summit`), max 255 | `appletb_backlight` (`hid-appletb-bl`), **max 2** (off/dim/full) |
| main backlight | `apple-panel-bl` (max 509) | `gmux_backlight` / `intel_backlight` / `acpi_video0` |
| battery % | `capacity` | computed from `charge_now/charge_full` (x86_64 cfg) |
| kernel fallback | **none**: if no daemon draws, the bar is blank | the T2 firmware can draw a basic Esc/F-key/media bar by itself; `hid-appletb-kbd` picks the mode (`mode` param) when no daemon drives `appletbdrm` |
| bar length | 2008 on the 13" models, which have a physical Esc | 2170 (2016–19, no physical Esc) or 2008 (2020 13") |

## License (for reuse)
- tiny-dfr code: **MIT** ("Copyright (c) 2023 WhatAmISupposedToPutHere" / The Asahi Linux
  Contributors). We can copy and adapt code as long as we keep the MIT notice. Put copied files
  under a header like `// Derived from tiny-dfr (MIT), see LICENSES/tiny-dfr-MIT.txt`.
- Icons in `share/tiny-dfr/*.svg`: Google Material Design Icons, **Apache-2.0** (some edited by
  kekrby). Fine to ship with the Apache notice.
- Cargo says `license = "MIT AND Apache-2.0"` for that reason.

## Measured on real hardware (2026-10-12, sliver-hwprobe via safe-swap)
DRM `card1` (driver `adp`, "Apple Display Pipe DRM Driver"):
- Caps: DUMB_BUFFER=1, DUMB_PREFERRED_DEPTH=24, DUMB_PREFER_SHADOW=0, PRIME=3, ASYNC_PAGE_FLIP=0,
  ADDFB2_MODIFIERS=1, CRTC_IN_VBLANK_EVENT=1. UNIVERSAL_PLANES and ATOMIC client caps are accepted.
  A non-root process became master just by opening the card once tiny-dfr was gone, as expected.
- One connector (`DSI`, id 39): mode `60x2008@60`, `size_mm` unknown. Props: EDID, DPMS,
  link-status, **`non-desktop=1`** (why compositors leave it alone), TILE, CRTC_ID.
- One CRTC (id 37): props ACTIVE, MODE_ID, OUT_FENCE_PTR, VRR_ENABLED. After tiny-dfr exits it
  stays `ACTIVE=1` with the mode set, but the plane has no fb.
- One plane (id 35, primary): **formats `XR24` only**. Props: type, FB_ID, IN_FENCE_FD, CRTC_*,
  SRC_*, IN_FORMATS. **No `FB_DAMAGE_CLIPS` and no `rotation`**: the rotation is ours to do in
  software, and partial updates go through legacy `DIRTYFB` (what tiny-dfr does). Atomic
  page-flip events with vblank are available if we ever want frame pacing.

Touch `event2` ("Mac14,7 Touch Bar"):
- ABS_X/MT_POSITION_X 0..23044, ABS_Y/MT_POSITION_Y 0..639, **resolution 100 units/mm**, so the
  active area is **230.4 x 6.4 mm**, ~11.5 units per pixel along the bar and ~10.7 across.
- MT_SLOT 0..255 (the kernel's slot cap, not the physical finger limit). TOUCH_MAJOR/MINOR,
  WIDTH_MAJOR 0..65535, ORIENTATION ±32767, TRACKING_ID 0..65535. No pressure, no hover.
- **X runs left to right** in the user's view (landscape `lx`): left-end tap x≈800, right-end tap
  x≈22472, a full slide 214→23023 in 1.6 s as one continuous contact. Map with
  `lx = x * 2008 / 23045` (≈11.5 units/px). Contacts reach within ~2 px of both ends.
- Y spans ~18..581 in practice (of 0..639). Whether y=0 is the top edge isn't verified, and it
  barely matters with 6 mm of height (tiny-dfr only uses y to drop touches in the outer 10%).
- **3 simultaneous contacts register reliably** (tested 3, repeatedly; more is untested). Slots
  0..2 got reused per contact, with a fresh TRACKING_ID each time.
- **Report rate ~60 Hz** (median 17 ms between SYN_REPORTs). Quick taps last 50–170 ms.
- Contact size: TOUCH_MAJOR ~50–190, TOUCH_MINOR ~115–445 (minor > major, so the units/axes are
  odd), WIDTH_MAJOR ~550–750, ORIENTATION mostly 16384 (= 90°). Treat these as noisy. Don't
  build palm rejection or "pressure" on them without more data.
- Events seen: EV_KEY BTN_TOUCH, EV_ABS X/Y + MT SLOT/TOUCH_MAJOR/MINOR/WIDTH_MAJOR/MINOR/
  ORIENTATION/POSITION_X/Y/TRACKING_ID.

Drawing: a tiny-dfr-style atomic modeset (`ALLOW_MODESET`, XR24 dumb buffer 60x2008) committed in
**16 ms**. A full-frame DIRTYFB took **34 ms** (~2 frames). So keep partial updates small, and
expect roughly 30 fps max for full-bar animations through DIRTYFB. The orientation test pattern
showed up correctly *in the snap* (white top-left, green bottom-right; `sliver_hw::geom` mapping
confirmed). That run used the unpadded buffer, and nobody checked it by eye; see the stride note below.

**Buffer stride (confirmed by eye, H28):** with a 60x2008 dumb buffer (240-byte pitch) the first
sliverd run looked **static but garbled** on the real panel: boxes right, labels striped. A CPU
readback (`snap`) of the same buffer was perfect. tiny-dfr allocates **64x2008** and scans out 60,
so `sliver_hw::panel` now pads the width to a multiple of 64 px (pitch 256). After that the human
saw sharp labels on the panel, so adp needs a 64-byte-aligned stride (or at least 256-byte rows). **Lesson: `snap` shows what's in the buffer, not what
the panel shows.** Confirm visuals by eye or photo.

**Compositor sees our keys:** during a sliverd run `hyprctl devices` lists `sliver-touch-bar-keys`
(us layout) as a keyboard, so keystrokes reach the session through the user's layout.

**First real sliverd runs** (2026-10-12, `sliverd -v` via the harness): safe layer rendered (full
flush 10–11 ms), taps → keys 59/63/88 down/up (50–70 ms holds), Fn → media row, double-press-hold
Fn → F-key hatch, 24 contacts, clean exit before the deadline, tiny-dfr restored every time. If a
harness run's log shows no input at all, check whether anyone touched the bar during the window:
the keyboard counters will be 0 too.

**Full stack on the real bar** (sliverd -v + sliver-agent over `$SLIVER_OUT_DIR/sliverd.sock`,
human tapping): agent frame on the panel 0.4 s after connect; every touch forwarded to the agent as
`Touch { region, phase }`; liveness ping/pong <10 ms; agent-driven keys out through uinput; killing
the agent → safe layer in 5–8 ms. Recipe (**use release builds for anything timing-related**: a
debug agent renders a full frame in ~37 ms vs ~2 ms in release): `cargo build --release -p sliverd
-p sliver-agent`, start `bar run target/release/sliverd --seconds 60 -- -v`, then run
`target/release/sliver-agent --socket /run/sliver-safeswap/out/sliverd.sock` as the user.
Debug-build latencies from that run: touch → agent present 27–38 ms, touch → panel 46–63 ms.
ash's press fast path (8cdca17) plus a release build should bring it to ~10–15 ms.

**First real install** (RPM .198, H42): sliverd from `sliverd.service` runs in cgroup
`/system.slice/sliverd.service` *as seen from the host*. Inside the unit,
`ProtectControlGroups=strict` (systemd 259) gives it a private cgroup namespace, so its own
`/proc/self/cgroup` reads `0::/` and its uid is a DynamicUser one (65132). The hardware gate
accepts that pair (59a1eb4). A gate that only accepted the host path crash-looped the .256
upgrade into the tiny-dfr fallback. Check sandbox-visible facts from inside the sandbox.
Bar backlight `root:video 0664`
via 99-z-sliverd.rules, agent connected, Hyprland lists `sliver-touch-bar-keys`, no AVCs.
**Harness on the installed system** (H43, d9789ad): a 30 s run held the bar with sliverd *and*
sliverd.socket stopped (the agent waited instead of re-activating sliverd), then restore brought back
both and the agent reconnected.

**Adaptive brightness is event-driven** (H44, bff78ed): every sysfs write to the main display's
backlight (`apple-panel-bl`, max 509) makes the kernel send a backlight `change` uevent
(`SOURCE=sysfs`); sliverd's udev monitor picks it up and re-applies the bar. Measured: screen
56 → 228 → 56 moved the bar 43 → 87 → 43 (tiny-dfr's `sqrt(screen) * 128 + 1`) with no touch.

**P0 sweep (T72):** idle dim at 30 s and off at 60 s after the last seat0 activity; the first tap
on the dark bar only wakes it, the next one fires (H45). Key repeat: `sliver-hwprobe --hold-key 114
600` holds volume-down through the uinput keyboard; Hyprland (repeat 250 ms / 35 Hz, `repeating`
bind) took the volume 0.22 → 0.00, i.e. ≥11 presses (H48). Repeat is the compositor's job: sliverd
sends key-down on touch and key-up on release.

**Idle wakeups (T101, `scripts/idle-wakeups`):** sliverd + sliver-agent with the default config on
private buses, 30 s idle with the bar on and 30 s with it off. sliverd: **0** wakeups in both.
The agent was at ~52/s because the audio feed respawned `pactl subscribe` about every 0.75 s
when no PulseAudio server answered: a successful spawn reset the backoff even though pactl exited
at once. Fixed: only a pactl that stays up 10 s resets it, otherwise it backs off to 30 s. Now
on ≈ 6/s, off ≈ 3–4/s. The floor was then **2/s from zbus's `blocking` pool thread**: zbus connects
and closes Unix sockets via `blocking::unblock`, and `blocking` 1.7 parks an idle pool thread in a
500 ms `wait_timeout` loop it never leaves below the pool limit (`BLOCKING_MAX_THREADS=1` doesn't
help: it only exits *above* the limit). **Fixed in T103:** `crates/sliver-bus` connects the socket
itself and gives zbus a write half that shuts down directly, so the pool never starts. Every D-Bus
connection in the agent process goes through it. Now on ≈ 3.9–4.8/s, off ≈ 1.3–1.4/s
(`scripts/idle-wakeups --threads`: the async-io reactor, the audio feed's 30 s `pactl` retry in the
sandbox and its spawner). On the live session the installed agent measured ~8/s before T103 (real
buses, compositor events). Budgets: on ≤ 8/s, off ≤ 3/s.

**The real system bus (T117, hazel's measurement of the installed agent: 1.45/s with the bar off,
nothing happening).** The private-bus harness has no NetworkManager or UPower, so it can't show
this. On the real laptop NetworkManager, bluez and UPower emit `PropertiesChanged` every few
seconds (signal strength, scans, battery readings), and each one wakes the async-io reactor, the
zbus executor and the session watcher thread together, then triggers a re-read (more bus
round trips). `cargo run -p sliver-session --example idle-probe -- 40 session` against the real
buses (`DBUS_SYSTEM_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket`) showed 1.9/s while the
subscriptions were on. **Fixed:** `Handle::set_awake(false)`, called when sliverd reports the
display off, removes the three chatty match rules (logind stays: lock and sleep must always work),
and `set_awake(true)` puts them back and re-reads network, Bluetooth and power, so nothing is
missed. Same measurement with the bar off: **0.03/s**. While the bar is on the subscriptions stay,
because the network/battery modules show those values live. The probe (`idle-probe`) also takes
`audio`, `notifications`, `tray`, `appearance`, `screensaver` and `dark`, to bisect other sources;
on the sandbox buses each of those is silent except audio without a PulseAudio server (one
`pactl` retry cycle every 30 s, about 25 wakeups, 0.8/s on average).

Snap of tiny-dfr through the harness: correct orientation (F1 on the left, text upright), so
`snap`'s ioctl layouts and un-rotation are confirmed.

### Guided check 2026-10-13 (human, `hwcheck pixel-shift suspend`, files in `target/hw/checks/`)
- **Pixel shift on the real panel (T79/T95):** the lit area moved x 2→4→6, y 4→2→0 (top) over about 60 s:
  1 px per 15 s along the path, so the stride and DRM path are fine with shifted frames. Nothing was
  clipped at either end (content keeps 11 px clear, and the shift is at most 11 px). The human
  didn't notice any movement. Touch reached sliverd while shifted.
- **Fn held → the agent's Fn layer:** `fn down` → `view=Agent(Fn)` in 5–13 ms, back to `Agent(Base)` on release.
- **Suspend (s2idle, lid open, 12–13 s, .313 with T120):** the bar came back lit at the same brightness
  (128) with a correct picture. Touch after wake wasn't tested.
- **Lesson: test agents must not share the session bus.** A second sliver-agent takes
  `io.github.wawow830.Sliver` from the installed one. When it exits, systemd stops the installed agent
  (`Type=dbus`, `BusName=`). hwcheck runs its agent under `dbus-run-session` (d7e6c25).

## Open questions (need hardware or the human)
1. ~~Physical Esc?~~ **Yes** (H2).
2. ~~DRM caps~~: measured above. ~~Does `ACTIVE=0` blank the panel?~~ **No** (T48, H30): the
   commit returns in ~80 µs but the bar keeps showing its last frame. The panel is command-mode
   and holds its own copy, which is also why it keeps the last image while nobody owns the card.
   **"Display off" = backlight 0** (what tiny-dfr and sliverd do). Whether ACTIVE=0 saves power
   anyway is unmeasured: SMC "Total System Power" (hwmon `macsmc_hwmon`, readable on AC) swings by
   seconds-scale watts under load, far more than the bar draws. It needs an idle machine and long
   A/B runs (`sliver-hwprobe --power-cycle`).
3. ~~Touch ranges, X direction~~: measured above. Still open: the finger limit above 3, the Y
   direction, and what the contact-size values mean.
4. Does `systemd-backlight` for the bar fight a daemon that writes brightness? Probably not (it
   only acts at boot and shutdown).
