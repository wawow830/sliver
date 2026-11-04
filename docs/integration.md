# Desktop integration

How `sliver-agent` learns what is on screen and what state the machine is in, so it can pick
layouts (per app, workspace or state) and drive modules. It covers the desktops listed in
`GOAL.md`. Notes about specific software versions come from a Fedora Asahi 44 system running
Hyprland 0.56.2, PipeWire 1.6.8, UPower, logind and quickshell as the shell.

Code: `crates/sliver-desktop` (`DesktopState`, `DesktopSource`, the compositor backends, a scripted
fake) and `crates/sliver-session` (`SessionState`, `SessionSource`: logind, UPower,
power-profiles, MPRIS and more; `mpris::control` for transport commands;
`Session::start(system_conn, session_conn)`).

## 1. Architecture

```
compositor backend ─┐
logind / UPower    ─┤                     ┌─> layout resolver (context predicates)
MPRIS / PipeWire   ─┼─> sliver-agent state ┤
sysfs brightness   ─┤   (flat key -> value)└─> widgets (clock, volume, now playing ...)
notifications      ─┘
```

- Every source is **event driven** (socket read, D-Bus signal, netlink); none polls on a timer
  except where noted. This keeps invariant 3 (cheap on power): the agent sleeps until something
  changes. Where only polling exists (sysfs brightness), it polls only while a widget showing the
  value is on the bar.
- Each source reduces to a flat piece of state. Layout conditions match flat keys, not backend
  objects, so one layout works on any compositor.
- Every source degrades to "unknown", never to an error: a dead compositor connection yields
  `DesktopState { connected: false, .. }` and the agent shows the default/safe layer
  (invariant 1).
- Sources are plain blocking threads or zbus connections. `sliver-desktop` has no async runtime
  dependency; states reach the agent through a channel.

### Context keys (what predicates can match)

The full list is `sliver_config::condition::CONTEXT_KEYS`. Keys are matched case-insensitively,
with `-` and `_` interchangeable (`app-id` = `app_id`). A key with no known value is left out of
the context, so it never matches.

| key | type | source |
|---|---|---|
| `app_id` | string | compositor: Wayland app_id / X11 WM_CLASS class |
| `desktop_id` | string | the focused app's `.desktop` entry (`sliver-icons`) |
| `title` | string | compositor |
| `pid` | number | compositor |
| `fullscreen`, `floating`, `xwayland` | bool | compositor |
| `workspace` (id), `workspace_name`, `special_workspace`, `monitor` | | compositor |
| `monitors` (count), `external_display` (bool) | | compositor (Hyprland: `monitoraddedv2`/`monitorremovedv2` + `j/monitors`; internal = `eDP`/`LVDS`/`DSI` prefix) |
| `keyboard_layout` | string | compositor (see §3) |
| `submap` | string | compositor (Hyprland submap, sway mode) |
| `screencast` | bool | Hyprland event |
| `locked` | bool | see §3 |
| `idle` | bool | logind `IdleHint` |
| `sleeping` | bool | logind `PrepareForSleep` |
| `on_battery`, `battery_pct`, `battery_state`, `lid_closed` | | UPower |
| `power_profile` | string | power-profiles-daemon |
| `media_status` (`playing`/`paused`/`stopped`/`none`), `media_player`, `media_title`, `media_artist` | string | MPRIS |
| `mic_muted`, `output_muted`, `volume` | | PipeWire |
| `brightness` | number | screen backlight, percent |
| `dnd` | bool | notification daemon |
| `caffeine` | bool | idle inhibitors |
| `dark_mode` | bool | XDG portal `color-scheme` |
| `time` (`HH:MM`), `weekday` (`mon`…) | string | agent |
| `physical_esc` | bool | agent: does this Mac have a physical Esc key |
| `layer` | string | agent: the current layer |
| `app_controls` | bool | agent: the focused app fills App controls |

`app_id` also matches the `.desktop` id (`org.mozilla.firefox` vs class `firefox`), because icon
lookup needs the desktop entry. It's resolved through `StartupWMClass` and the id with the
`.desktop` suffix stripped, case-insensitively.

## 2. Compositor backends (focus, workspace, keyboard layout)

There is **no portal or cross-desktop standard** for "which window is focused": it is
deliberately withheld from Wayland clients, so each desktop needs its own backend.
`sliver_desktop::Backend::detect` picks one from the environment (`HYPRLAND_INSTANCE_SIGNATURE`,
`NIRI_SOCKET`, `SWAYSOCK`, `XDG_CURRENT_DESKTOP`, `DISPLAY`/`WAYLAND_DISPLAY`).

| backend | mechanism | status | tested against |
|---|---|---|---|
| **Hyprland** | `socket2` event stream + `socket1` JSON queries | implemented (`sliver_desktop::hyprland`) | fake compositor; live Hyprland 0.56.2 |
| sway / i3 | i3-ipc over `$SWAYSOCK`, `SUBSCRIBE ["window","workspace","input","mode"]` | implemented (`sliver_desktop::sway`) | fake server; headless sway 1.11 (`integration/sway/test.sh`) |
| niri | JSON IPC over `$NIRI_SOCKET`, `"EventStream"` request | implemented (`sliver_desktop::niri`) | fake server; niri 26.04 nested in a headless sway (`integration/niri/test.sh`): list, focus, workspaces, focus/switch/close |
| wlroots generic | `wlr-foreign-toplevel-management-v1` (+ `ext-workspace-v1`) | implemented (`sliver_desktop::wlr`) | mock compositor; headless labwc (`integration/labwc/test.sh`); river-classic 0.3 (`integration/river/test.sh`) |
| KDE Plasma 6 | KWin script calling our D-Bus service | implemented (`integration/kwin`) | nested `kwin_wayland --virtual` |
| GNOME Shell | extension calling our D-Bus service | implemented (`integration/gnome-shell`); also switches workspaces for the agent | headless gnome-shell 50.5 (`integration/gnome-shell/test.sh`; see integration/README.md) |
| X11 | `_NET_ACTIVE_WINDOW` on the root window | not implemented | |

### Hyprland

- Sockets: `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` (commands) and
  `.socket2.sock` (events). **They are dotfiles**, so `ls` without `-a` hides them. Several
  instance directories can exist (stale ones from past sessions); the env var picks the live one.
  The pre-0.40 location, `/tmp/hypr/$SIG`, is kept as a fallback.
- Events are lines `NAME>>DATA`. The agent uses `activewindowv2` (address), `windowtitlev2`,
  `closewindow`, `fullscreen`, `changefloatingmode`, `workspacev2`, `focusedmonv2`,
  `activespecialv2`, `activelayout`, `submap`, `screencast` and `configreloaded`. The legacy
  `activewindow` (`class,title`) is redundant with the query and ignored.
- Gotchas handled in the parser:
  - titles contain commas: split only on the first comma;
  - `activelayout>>keyboard,layout`: keyboard names are sanitized (no commas), but layout names
    can contain them (`English (US, intl.)`), so split on the first comma;
  - events carry class/title/address only. pid, floating and xwayland need `j/activewindow`,
    which the watcher runs after each focus change;
  - special workspaces have negative ids and arrive via `activespecialv2`, not `workspacev2`;
  - unknown events and malformed lines are ignored, so a newer Hyprland can't break the agent.
- The watcher subscribes to the event socket **before** snapshotting via the command socket, so
  changes in between aren't lost. After `configreloaded` it takes a new snapshot.
- The initial keyboard layout isn't in any event, so it's read from `j/devices`
  (`active_keymap`). On Apple laptops no keyboard has `main: true`, and virtual keyboards (fcitx5,
  tiny-dfr's `dynamic-function-row-virtual-input-device`) are listed, so the first physical one is
  used.
- Actions (switch workspace, focus or close a window, change keyboard layout) are dispatcher
  commands on the command socket (`sliver_desktop::control`), sent only on an explicit user action.
- Hyprland lists sliverd's virtual input device among its keyboards: keys emitted through uinput
  go through the compositor like any other keyboard. The keymap Hyprland applies to it is what
  makes key combos follow the keyboard layout (customization.md §3).

### sway / i3

i3-ipc is a tiny framed protocol (`"i3-ipc"` + u32 len + u32 type + payload), hand-rolled rather
than pulling in `swayipc` (which depends on async-std). The backend uses the `window`
(change=focus/title/close), `workspace` (focus), `input` (`xkb_layout_names` /
`xkb_active_layout_name`) and `mode` events, and `GET_TREE` once for the snapshot. The focused
window's id is `app_id` (Wayland) or `window_properties.class` (XWayland).

### niri

`NIRI_SOCKET`, newline-delimited JSON. The backend sends `"EventStream"`; events include
`WindowFocusChanged`, `WorkspaceActivated`, `KeyboardLayoutsChanged`, `KeyboardLayoutSwitched` and
`WindowOpenedOrChanged`. Windows carry `app_id`, `title` and `pid`.

### wlroots generic fallback

`zwlr_foreign_toplevel_manager_v1` gives each toplevel's `title`, `app_id` and `state`
(activated, fullscreen). It has no pid and no workspace info (`ext-workspace-v1` fills that in on
newer compositors). `ext-foreign-toplevel-list-v1` is the standard replacement, but it has no
"activated" state, so it can't answer "which window is focused". The crates are `wayland-client`
+ `wayland-protocols-wlr` (pure-Rust backend, no libwayland needed at build time).

`sliver-desktop/src/wlr.rs` (`Backend::Wlroots`) is used for any Wayland session that isn't
Hyprland, sway, niri, GNOME or KDE. One thread owns the connection and sleeps in `poll(2)` on it
and on a wake pipe. Actions (focus, close, switch workspace) are requests on that same connection,
handed over by `control::run` through a channel, because the protocols' object ids only mean
something on the connection that received them. The protocols don't say which workspace a window
is on, so all windows show on the active workspace. There is no pid, floating or keyboard-layout
info. On a compositor without `wlr-foreign-toplevel-management` the watcher stops at once (no
retry loop).

- **labwc:** windows, focus and workspaces work.
- **river-classic 0.3:** windows and focus work. river has no `ext-workspace` (its tags use the
  private `river-status` protocol, which isn't implemented), so there are no workspaces.
- **river 0.4** manages no windows without a separate window-manager client, so nothing shows.

The test (`crates/sliver-desktop/tests/wlr.rs`) runs a mock compositor (wayland-server) speaking
both protocols and checks the initial burst, live changes and the three actions over a real
socket. `integration/labwc/test.sh` checks the whole chain against a real labwc running headless
(`WLR_BACKENDS=headless WLR_RENDERER=pixman`). Neither labwc nor sway needs root:
`dnf download --resolve` the rpms, `rpm2cpio | cpio -idm` them into a prefix and point
`SLIVER_TEST_ROOT` at it (the scripts explain how, and skip when the compositor is missing).
`integration/sway/test.sh` does the same for the sway backend.

### KDE Plasma 6 (KWin script)

KWin has no D-Bus API for querying focus, so a small KWin script (`integration/kwin/sliver-focus`,
installed by the `sliver-kwin` package to `/usr/share/kwin/scripts/sliver-focus/` and enabled by
the user) listens to `workspace.windowActivated` and `workspace.currentDesktopChanged` and pushes
focus, workspaces and windows to the agent with `callDBus` over
`io.github.wawow830.Sliver.Desktop1` (integration/README.md). The agent switches the keyboard
layout through `org.kde.keyboard /Layouts` (`switchToNextLayout`) and workspaces through KWin's
own `setCurrentDesktop`.

### GNOME Shell (extension)

Mutter doesn't let clients enumerate or watch windows (`org.gnome.Shell.Eval` has been locked down
since GNOME 41). The only route is a **GNOME Shell extension** (ESM, GNOME 45+):
`integration/gnome-shell/sliver-focus@sliver.wawow830.github.io`, installed by the
`gnome-shell-extension-sliver` package and enabled by the user. It watches `global.display`
`notify::focus-window` and reads the window's GTK application id, sandboxed app id or WM class,
title and pid. Extensions must be re-checked each GNOME release (the extension API changes), so
this is the highest-maintenance backend.

### X11 (not implemented)

The approach would be `_NET_ACTIVE_WINDOW` on the root window, `PropertyNotify` events, then
`_NET_WM_NAME`, `WM_CLASS`, `_NET_WM_PID`, `_NET_CURRENT_DESKTOP`, with the `x11rb` crate. It's
only needed for Xorg sessions (for example t2linux users on X11 window managers). XWayland windows
under Wayland compositors are already covered by the native backends.

## 3. System sources

All over D-Bus via `zbus` (ADR-0001), on the system bus unless noted.

### Lock, idle, sleep: logind (`org.freedesktop.login1`)

- `Session` (`/org/freedesktop/login1/session/auto`): properties `LockedHint`, `IdleHint`,
  `Active`; signals `Lock`/`Unlock`. Manager signal: `PrepareForSleep(b)`.
- **`LockedHint` can't be trusted on its own.** It's only a hint that the locker sets, and some
  lockers leave it stale. For example, quickshell's lock calls `loginctl lock-session` (which sets
  the hint and emits `Lock`) but never `unlock-session`, so the hint stays `yes` after unlocking.
  There's no client-side way to observe `ext-session-lock`.
- **How the agent decides `locked`**, most trustworthy first:
  1. the desktop's own `ActiveChanged` signal on `org.gnome.ScreenSaver` /
     `org.freedesktop.ScreenSaver` (read once with `GetActive`, then followed;
     `sliver-session::screensaver`);
  2. on GNOME and KDE without that signal, the logind hint, which those shells keep accurate;
  3. everywhere else, whether a known locker process (`hyprlock`, `swaylock`, `gtklock`,
     `waylock`, `i3lock`, `physlock`) is running, **regardless of the hint**. So a locker started
     by a keybind or `swayidle` (which never touches logind) still brings up the privacy layout,
     and a stuck `LockedHint` doesn't. The scan reads `/proc/*/comm`, is cached for 1.5 s and is
     redone whenever the bar wakes from off.

  **Limits:** a locker that starts while the bar is on and nothing else changes is only noticed at
  the next render or wake; there is no event for it, and nothing polls (invariant 3). Unknown
  lockers (such as a shell's built-in lock, like quickshell's) aren't detected on those
  compositors unless they emit `ActiveChanged`.
- For dimming the bar, sliverd uses its own touch/key idle timer (it sees the input). For "the
  user is idle on the desktop", `ext-idle-notify-v1` (Wayland) is better than polling `IdleHint`.

### Power: UPower (`org.freedesktop.UPower`)

- `/org/freedesktop/UPower`: `OnBattery`, `LidIsClosed`.
  `/org/freedesktop/UPower/devices/DisplayDevice`: `Percentage`, `State`, `TimeToEmpty/Full`,
  `IsPresent`. Changes arrive as standard `PropertiesChanged` signals.
- Power profile: `net.hadess.PowerProfiles` / `org.freedesktop.UPower.PowerProfiles`
  `ActiveProfile`.

### Brightness: sysfs read, logind write

`sliver_session::brightness`: `Sysfs` finds the screen backlight and the keyboard LED, and never
picks the Touch Bar (matched by tiny-dfr's device names or the `panel-summit` driver).
`Handle::refresh_brightness` pushes levels (called after our own writes, on wake, and on a slow
timer only while a slider shows). `brightness::set` writes through logind. udev change events
aren't used yet.

- Apple Silicon example: `/sys/class/backlight/apple-panel-bl` is the laptop screen (max 509);
  `/sys/class/backlight/228600000.dsi.0` (max 255) is the Touch Bar panel itself. That one belongs
  to sliverd and is **never** exposed as "screen brightness". Keyboard backlight:
  `/sys/class/leds/kbd_backlight` (max 255).
- sysfs `brightness` is world-readable but root-writable. User-space writes go through
  `org.freedesktop.login1.Session.SetBrightness(subsystem, name, value)` (`ssu`), e.g.
  `("backlight","apple-panel-bl",v)` or `("leds","kbd_backlight",v)`. There's no polkit prompt for
  the active local session.
- Change notification: sysfs attributes don't reliably emit inotify. The options, in order: udev
  `backlight`/`leds` change uevents, our own writes (we know the new value), desktop services
  (GNOME `org.gnome.SettingsDaemon.Power.Screen`/`Keyboard`, KDE PowerDevil), else a slow poll
  (1 s) only while a slider is visible. Only external changes (the user's brightness keys) need
  the fallback, and on Touch Bar Macs those keys are on the Touch Bar itself.
- Levels are percent of the device's range; curves belong in the appearance layer, not here.

### Audio: PipeWire

`sliver_session::audio` (`Handle::start_audio(Tools)`): default sink volume and mute, default
source (mic) volume and mute. Reads and writes use `wpctl` (`wpctl set-volume
@DEFAULT_AUDIO_SINK@ 5%+`, `set-mute @DEFAULT_AUDIO_SOURCE@ toggle`); writes are
`audio::set_volume/change_volume/set_mute/set_mic_mute`. Change events come from a long-lived
`pactl subscribe` (pipewire-pulse), so the agent sleeps until something changes. There are no
build-time dependencies and nothing is polled.

Alternatives that weren't picked:

| option | why not (yet) |
|---|---|
| `pipewire` crate (pipewire-rs) | Native and event driven, but needs `libpipewire-0.3` dev files and bindgen/clang at build time, and reading the default sink means following WirePlumber's `default` metadata. A large API for little gain. It could replace the CLI backend behind the same `Audio` state later. |
| `libpulse-binding` (talks to `pipewire-pulse`) | Event driven and simple, and works on Pulse systems too, but needs the `pulseaudio-libs-devel` dev files at build time. |

### Media: MPRIS

`org.mpris.MediaPlayer2.*` on the session bus. The source watches `NameOwnerChanged` for the
`org.mpris.MediaPlayer2.` prefix, then `PropertiesChanged` on `/org/mpris/MediaPlayer2`
(`org.mpris.MediaPlayer2.Player`: `PlaybackStatus`, `Metadata` (`xesam:title`/`artist`,
`mpris:artUrl`, `mpris:length`), `Volume`, `CanSeek`...). `Position` doesn't emit changes, so
it's interpolated from the last `Seeked` and the elapsed time, never polled. The "active" player
is the one `Playing`, else the most recently changed. `playerctld` registers as
`org.mpris.MediaPlayer2.playerctld` (a proxy that tracks the most recent player); it isn't
treated as a second player, which would show duplicates. Players are talked to directly over
D-Bus rather than through `playerctl`.

### Notifications and DND

`sliver_session::notifications`. `org.freedesktop.Notifications` has no DND and no "list", and
the agent must not become the notification server. So it counts open notifications by **bus
monitoring** (`org.freedesktop.DBus.Monitoring.BecomeMonitor`, which a user may do on their own
session bus): `Notify` calls, the server's replies and `NotificationClosed` signals. It keeps the
latest notification's app and summary for the `notifications` module, never the body.

DND has no standard, so there's a small adapter list: dunst (`org.dunstproject.cmd0` `paused`),
KDE (the server's `Inhibited` property), mako (`makoctl mode`) and swaync (`swaync-client`).
Other servers (GNOME, quickshell) report DND as unknown.

### Caffeine / inhibit

`SessionState.idle_inhibited` follows logind's `BlockInhibited`. `Handle::set_caffeine` holds a
logind `idle` inhibitor (`Inhibit(what, who, why, mode)`, fd-based: closing the fd releases it, so
it's released when the agent dies). hypridle honours logind inhibitors only if
`ignore_systemd_inhibit` is false (the default). The other mechanism,
`org.freedesktop.ScreenSaver` `Inhibit`/`UnInhibit` (owned by hypridle on Hyprland setups), isn't
used. logind's `ListInhibitors` lists other programs' inhibitors.

### System tray: StatusNotifierItem

`sliver_session::tray` and `sliver_session::menu`. `Handle::start_tray(conn)` registers an extra
host with the existing `org.kde.StatusNotifierWatcher` (KDE, quickshell, waybar…). It follows
`RegisteredStatusNotifierItems`, the watcher's Registered/Unregistered signals and the items' `New*`
signals, and fills `SessionState.tray` (id, title, status, icon name + theme path, the best pixmap
converted from ARGB to RGBA, tooltip, menu path). Controls: `tray::poke` (`Activate`,
`SecondaryActivate`, `ContextMenu`) and `tray::scroll`. `menu::layout` / `menu::click` decode and
drive the dbusmenu for a sub-bar.

Not implemented: a fallback watcher owned by sliver when the bus has none (GNOME, bare sway: the
tray stays empty until one appears), and live refresh of an open menu on `LayoutUpdated`.

### Keyboard layout (outside the compositor)

Compositor events win (§2), but input methods also change what the user is typing.
`sliver_session::fcitx` (`SessionState.input_method`) follows fcitx5 (`org.fcitx.Fcitx5`;
`org.fcitx.Fcitx.Controller1` `CurrentInputMethod`): while it runs, the `language` module shows
fcitx5's current input method and label, and the compositor's layout otherwise.

`CurrentInputMethod` is a method, and fcitx5 sends no signal when the user switches with the
hotkey (`Controller1` only has `InputMethodGroupsChanged`, and there are no tray signals either).
So besides `NameOwnerChanged`, the groups signal, tray `New*` signals, window focus changes and
our own switches, fcitx5 is asked every 2 s (one cheap `CurrentInputMethod` call; the group is
read only when that changed). This happens only while the bar is on, fcitx5 is running and a
layout uses `language` (invariant 3).

## 4. Crates

| need | crate | note |
|---|---|---|
| D-Bus | `zbus` | per ADR-0001; blocking API, no async runtime |
| JSON | `serde`, `serde_json` | Hyprland/niri IPC |
| Wayland protocols | `wayland-client`, `wayland-protocols`, `wayland-protocols-wlr` | foreign-toplevel, ext-workspace |
| icons/desktop entries | **`sliver-icons`** (own crate, std only) | `IconLoader` (freedesktop theme spec: Inherits, hicolor, sizes/scales, `-symbolic`, dash fallback, pixmaps, cache), `DesktopEntries::find_by_app_id` (id, StartupWMClass, reverse-DNS tail, Exec); `cargo run -p sliver-icons --example icon-find -- kitty firefox 40` |
| sway/i3 | none (hand-rolled) | tiny protocol |

Candidates for parts not implemented yet: `x11rb` (X11), `pipewire` (native audio), `udev` or
`tokio-udev` (backlight/LED change events).

## 5. Testing without hardware or a live session

- `sliver_desktop::fake::ScriptedSource` replays `DesktopState`s, for resolver and agent tests.
- The Hyprland backend is tested against a fake compositor on a temp Unix socket
  (`cargo test -p sliver-desktop`).
- `sliver-session` has `ScriptedSession` and `tests/dbus.rs`: mock logind/UPower/MPRIS services
  on a private `dbus-daemon` (skipped if the binary is missing). `cargo run -p sliver-session
  --example session-watch` prints live state (read-only).
- **Subscribe to signals before the initial read**, or changes between the two are lost. The
  existing sources do this; new ones must too.
- `cargo run -p sliver-desktop --example desktop-watch` (any supported compositor; `hypr-watch`
  for Hyprland only) prints live JSON states. It's read-only and safe on a live session.
- Test D-Bus sources against a private `dbus-daemon --session` with mock services, never against
  the system bus.
- Tests against real compositors, the portal and PipeWire: integration/README.md.

## 5b. `sliver-feeds`: the wiring

`crates/sliver-feeds` runs the desktop watcher (`watch_auto`), the session sources and the
`custom/*` runtime on threads, forwards everything on one channel with a wake fd, and folds it into
the rule `Context`: `Feeds::new()`, `start_desktop(env)`, `start_session()`, `start_modules(specs)`,
then poll `feeds.as_fd()` and call `feeds.drain()`. `Aggregate::apply(feed)` returns whether rules
or a module output changed, and `Aggregate::context()` gives the `CONTEXT_KEYS` (unknown values
are left out, so they never match). `cargo run -p sliver-feeds --example context-dump` prints the
live context.

## 6. Not implemented yet

1. X11 backend (§2).
2. udev change events for brightness (§3).
3. A sliver-owned StatusNotifierWatcher when the bus has none, and live menu refresh on
   `LayoutUpdated` (§3).
4. Workspaces on river (its tags are the private `river-status` protocol).
5. Detecting lockers that neither set `ActiveChanged` nor appear in the known-locker list (§3).
