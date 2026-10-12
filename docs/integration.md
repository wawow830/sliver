# Desktop integration survey (T7)

How `sliver-agent` learns what is on screen and what state the machine is in, so it can pick
layouts (per app / workspace / state) and drive widgets. Written against this machine
(Fedora Asahi 44, Hyprland 0.56.2, PipeWire 1.6.8, UPower, logind, quickshell as the shell) but
the backend list covers the other targets in `GOAL.md`.

Code: `crates/sliver-desktop` (`DesktopState`, `DesktopSource`, Hyprland watcher, scripted fake)
and `crates/sliver-session` (`SessionState`, `SessionSource`: logind, UPower, power-profiles,
MPRIS; `mpris::control` for transport commands; `Session::start(system_conn, session_conn)`).
Facts below marked **(verified)** were checked on this machine; the rest is from protocol docs
and should be re-verified when that backend is built.

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
  changes. Where only polling exists (sysfs brightness) poll only while a widget showing it is
  on the bar.
- Each source reduces to a flat piece of state. Layout predicates should match flat keys, not
  backend objects, so one layout works on any compositor.
- Every source must degrade to "unknown", never to an error: a dead compositor connection
  yields `DesktopState { connected: false, .. }` and the agent shows the default/safe layer
  (invariant 1).
- Sources are plain blocking threads or zbus tasks. `sliver-desktop` has no async runtime
  dependency; hand states to the agent through a channel.

### Context keys (what predicates can match)

| key | type | source |
|---|---|---|
| `app_id` | string | compositor: Wayland app_id / X11 WM_CLASS class |
| `title` | string | compositor |
| `pid` | u32 | compositor |
| `fullscreen`, `floating`, `xwayland` | bool | compositor |
| `workspace` (id + name), `special_workspace`, `monitor` | | compositor |
| `keyboard_layout` | string | compositor (see §3) |
| `submap` / `mode` | string? | compositor |
| `screencast` | bool | Hyprland event; other DEs: PipeWire/portal sessions |
| `locked` | bool | logind `LockedHint` + `Lock`/`Unlock` signals |
| `idle` | bool | logind `IdleHint` / `ext-idle-notify-v1` |
| `sleeping` | bool | logind `PrepareForSleep` |
| `on_battery`, `battery_pct`, `battery_state`, `lid_closed` | | UPower |
| `power_profile` | string | power-profiles-daemon |
| `media_state` (`playing`/`paused`/`stopped`/`none`) | enum | MPRIS |
| `mic_muted`, `output_muted`, `volume` | | PipeWire |
| `dnd`, `caffeine` | bool | notification daemon / inhibitors |

`app_id` should be matchable against the `.desktop` id too (`org.mozilla.firefox` vs class
`firefox`), because icon lookup needs the desktop entry. Resolve through
`StartupWMClass` and the id with the `.desktop` suffix stripped, case-insensitively.

## 2. Compositor backends (focus, workspace, keyboard layout)

There is **no portal or cross-desktop standard** for "which window is focused": it is
deliberately withheld from Wayland clients. Each DE needs its own backend. Backend selection
is in `sliver_desktop::Backend::detect` (env: `HYPRLAND_INSTANCE_SIGNATURE`, `NIRI_SOCKET`,
`SWAYSOCK`, `XDG_CURRENT_DESKTOP`, `DISPLAY`/`WAYLAND_DISPLAY`).

| backend | mechanism | events | effort | status |
|---|---|---|---|---|
| **Hyprland** | `socket2` event stream + `socket1` JSON queries | yes | small | **done** (`sliver_desktop::hyprland`) |
| sway / i3 | i3-ipc over `$SWAYSOCK`, `SUBSCRIBE ["window","workspace","input","mode"]` | yes | small | todo |
| niri | JSON IPC over `$NIRI_SOCKET`, `"EventStream"` request | yes | small | todo |
| wlroots generic | `wlr-foreign-toplevel-management-v1` (+ `ext-workspace-v1`) | yes | medium | todo, fallback |
| KDE Plasma 6 | KWin script (JS) calling our D-Bus service | yes | medium | todo |
| GNOME Shell | extension exporting a D-Bus interface | yes | medium-large | todo |
| X11 | `_NET_ACTIVE_WINDOW` on the root window | yes | small | todo, last |

### Hyprland (done)

- Sockets: `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` (commands) and
  `.socket2.sock` (events). **They are dotfiles**; `ls` without `-a` hides them. **(verified)**
  Several instance directories can exist (stale ones from past sessions); the env var picks the
  live one. Pre-0.40 location was `/tmp/hypr/$SIG`, kept as fallback.
- Events are lines `NAME>>DATA`. The agent uses: `activewindowv2` (address), `windowtitlev2`,
  `closewindow`, `fullscreen`, `changefloatingmode`, `workspacev2`, `focusedmonv2`,
  `activespecialv2`, `activelayout`, `submap`, `screencast`, `configreloaded`.
  All of these strings are present in the 0.56.2 binary **(verified)**. Legacy `activewindow`
  (`class,title`) is redundant with the query and ignored by the watcher.
- Gotchas handled in the parser:
  - titles contain commas: split only on the first comma;
  - `activelayout>>keyboard,layout`: keyboard names are sanitized (no commas), layout names
    can contain them (`English (US, intl.)`), so split on the first comma;
  - events carry class/title/address only. pid, floating, xwayland need `j/activewindow`, which
    the watcher runs after each focus change;
  - special workspaces have negative ids and arrive via `activespecialv2`, not `workspacev2`;
  - unknown events and malformed lines are ignored so a newer Hyprland can't break the agent.
- Subscribe to the event socket **before** snapshotting via the command socket, so changes in
  between aren't lost. After `configreloaded`, resnapshot.
- Initial keyboard layout isn't in any event. Read it from `j/devices` (`active_keymap`). On
  this machine **no keyboard has `main: true`** and virtual keyboards (fcitx5, tiny-dfr's
  `dynamic-function-row-virtual-input-device`) are listed, so pick the first physical one.
  **(verified)**
- Read-only by design. Dispatching (`hyprctl dispatch`) would be needed for actions like
  "switch workspace" widgets; that is a separate, opt-in capability for the action layer.
- Hyprland lists tiny-dfr's `dynamic-function-row-virtual-input-device` among its keyboards
  **(verified)**, i.e. keys emitted via uinput go through the compositor like any keyboard.
  Probably an F-key we emit can therefore be consumed by a user keybind; unverified, relevant to
  the action engine.

### sway / i3

i3-ipc is a tiny framed protocol (`"i3-ipc"` + u32 len + u32 type + payload); hand-roll it
rather than add `swayipc` (async-std heavy). Use `window` (change=focus/title/close),
`workspace` (focus), `input` (`xkb_layout_names` / `xkb_active_layout_name`) and `mode`
events; `GET_TREE` once for the snapshot. Focused window: `app_id` (Wayland) or
`window_properties.class` (XWayland).

### niri

`NIRI_SOCKET`, newline JSON. Send `"EventStream"`; events include `WindowFocusChanged`,
`WorkspaceActivated`, `KeyboardLayoutsChanged`, `KeyboardLayoutSwitched`, `WindowOpenedOrChanged`.
Window `app_id`, `title`, `pid` are provided. Same shape as Hyprland; cheap to add.

### wlroots generic fallback

`zwlr_foreign_toplevel_manager_v1`: per-toplevel `title`, `app_id`, `state` (activated,
fullscreen). Works on Hyprland, sway, river, labwc, niri(no), KWin(no). It has no workspace
info (`ext-workspace-v1` fills that in on newer compositors) and no pid. Use it for compositors
without a dedicated backend. Crates: `wayland-client` + `wayland-protocols-wlr` (pure-Rust
backend, no libwayland needed at build; `wayland-client` 1.25 is installed anyway **(verified)**).
`ext-foreign-toplevel-list-v1` is the standard replacement but has no "activated" state, so it
can't answer "which is focused".

### KDE Plasma 6 (KWin script)

KWin exposes no focus-query D-Bus API. Standard approach: the agent installs a small KWin
script (a `KPackage` in `/usr/share/kwin/scripts/sliver-focus/`, enabled by the user or via
`org.kde.KWin /Scripting loadScript`+`start`). The script listens to
`workspace.windowActivated` (Plasma 6 names: `workspace.activeWindow`,
`window.resourceClass`, `window.desktopFileName`, `window.caption`, `window.pid`,
`window.fullScreen`) and `workspace.currentDesktopChanged`, and calls the agent with
`callDBus("org.sliver.Agent1", ...)`. The agent therefore needs one tiny private method
(`SetFocus(...)`), only accepted from the user's own KWin: keep it off the public API surface
(cedar). Keyboard layout: `org.kde.keyboard /Layouts` `getLayout` + `layoutChanged` signal.
Brightness has PowerDevil D-Bus too (`org.kde.Solid.PowerManagement.Actions.BrightnessControl`).
KDE is also on this machine's bus (`org.kde.*` services are present) **(verified)**, so it is
testable in a nested KWin.

### GNOME Shell (extension)

Mutter does not let clients enumerate or watch windows (`org.gnome.Shell.Eval` is locked
down since 41). The only route is a **GNOME Shell extension** (ESM, GNOME 45+) that watches
`global.display` `notify::focus-window` (and `window.get_title()`, `get_wm_class()`,
`get_pid()`, `get_gtk_application_id()`) and either exports a D-Bus interface or calls the
agent. Keyboard layout: `org.gnome.desktop.input-sources` (`current`, `sources`) via GSettings,
no extension needed. Ship at `/usr/share/gnome-shell/extensions/<uuid>/` from the RPM; users
must enable it, and it must be re-validated each GNOME release (extension API churn). This is
the highest-maintenance backend; do it last of the Wayland ones.

### X11

`_NET_ACTIVE_WINDOW` on the root, `PropertyNotify` events, then `_NET_WM_NAME`, `WM_CLASS`,
`_NET_WM_PID`, `_NET_CURRENT_DESKTOP`. Crate: `x11rb` (pure Rust, `xcb` backend optional).
Only needed for Xorg sessions and t2linux users on X11 WMs. XWayland windows under Wayland
compositors are already covered by their native backend.

## 3. System sources

All D-Bus via `zbus` (ADR-0001), system bus unless noted. Names/paths **(verified present)**
on this machine.

### Lock, idle, sleep: logind (`org.freedesktop.login1`)

- `Session` (`/org/freedesktop/login1/session/auto`): properties `LockedHint`, `IdleHint`,
  `Active`, signals `Lock`/`Unlock`. Manager signal `PrepareForSleep(b)`.
- **Caveat (verified):** `LockedHint` read `true` while the session was plainly unlocked and in
  use. It is only a *hint* that the locker sets; a stale value or a locker that doesn't maintain
  it is possible. Don't rely on it alone: treat `Lock` and `Unlock` signals as edges and
  combine with "is a locker process/surface present" (e.g. `hyprlock`) when the hint looks
  stuck. Needs a per-compositor check before `locked` is trustworthy. GNOME also has
  `org.gnome.ScreenSaver.ActiveChanged`; KDE has `org.freedesktop.ScreenSaver`.
- Idle for dimming the bar: sliverd should use its own touch/key idle timer (it sees the
  input). For "user is idle on the desktop" use `ext-idle-notify-v1` (Wayland) rather than
  polling `IdleHint`.

### Power: UPower (`org.freedesktop.UPower`)

- `/org/freedesktop/UPower`: `OnBattery`, `LidIsClosed`. `/org/freedesktop/UPower/devices/DisplayDevice`:
  `Percentage`, `State`, `TimeToEmpty/Full`, `IsPresent`. Standard `PropertiesChanged` signals.
  **(verified: 100%, state 4 = fully charged, OnBattery false)**
- Power profile: `net.hadess.PowerProfiles` / `org.freedesktop.UPower.PowerProfiles`
  `ActiveProfile`.

### Brightness: sysfs read, logind write

- Two backlight devices **(verified)**: `/sys/class/backlight/apple-panel-bl` (laptop screen,
  max 509) and `/sys/class/backlight/228600000.dsi.0` (max 255; this looks like the Touch Bar
  panel itself: birch/sliverd territory, **never** expose it as "screen brightness").
  Keyboard backlight: `/sys/class/leds/kbd_backlight` (max 255).
- sysfs `brightness` is world-readable but root-writable **(verified)**. User-space writes go
  through `org.freedesktop.login1.Session.SetBrightness(subsystem, name, value)` (`ssu`)
  **(verified present)**: `("backlight","apple-panel-bl",v)` or `("leds","kbd_backlight",v)`.
  No polkit prompt for the active local session.
- Change notification: sysfs attributes don't reliably emit inotify. Options in order: udev
  `backlight`/`leds` subsystem change uevents (`udev` crate / netlink; libudev 259 installed),
  our own writes (we know the new value), DE services (GNOME
  `org.gnome.SettingsDaemon.Power.Screen`/`Keyboard`, KDE PowerDevil), else a slow poll
  (1 s) only while a slider is visible. External changes (the user's brightness keys) are the
  only case that needs the fallback, and on this laptop those keys are exactly the Touch Bar.
- Scale: map to percent in the widget; curves belong in the appearance layer, not here.

### Audio: PipeWire

Needed: default sink volume + mute, default source mute (mic toggle), and writes.

| option | verdict |
|---|---|
| `pipewire` crate (pipewire-rs) | Native, event driven. Needs `libpipewire-0.3` dev files (installed: 1.6.8 **(verified)**) and bindgen/clang at build, and reading the default sink means following WirePlumber's `default` metadata. Large API for little gain. |
| `libpulse-binding` (talks to `pipewire-pulse`) | Event driven (`subscribe`: sink/source/server), simple, works on Pulse systems too. **`libpulse` dev files are not installed here** **(verified)**: needs `dnf install pulseaudio-libs-devel` (sudo request). |
| `wpctl` subprocess | No events; spawn-per-poll. Fine for **writes** (`wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+`, `set-mute @DEFAULT_AUDIO_SOURCE@ toggle`) and as the zero-dependency MVP, bad for reading state. |

Recommendation: MVP = `wpctl` for writes and on-demand reads (read once when a
slider appears, then trust our own writes; refresh on focus/wake). Target = `pipewire` crate in
its own thread (no new sudo, dev files already present) following default-node metadata.
Revisit `libpulse-binding` if bindgen/clang cost bites. Whoever builds the audio widget
decides, and records the choice in an ADR.

### Media: MPRIS

`org.mpris.MediaPlayer2.*` on the session bus. Watch `NameOwnerChanged` for the
`org.mpris.MediaPlayer2.` prefix, then `PropertiesChanged` on `/org/mpris/MediaPlayer2`
(`org.mpris.MediaPlayer2.Player`: `PlaybackStatus`, `Metadata` (`xesam:title`/`artist`,
`mpris:artUrl`, `mpris:length`), `Volume`, `CanSeek`...). `Position` does **not** emit changes:
interpolate from the last `Seeked` + time, never poll it. Choose the "active" player: the one
`Playing`, else the most recently changed. **(verified)** `playerctld` is already registered as
`org.mpris.MediaPlayer2.playerctld` (a proxy that tracks the most-recent player); the agent
must not treat it as a real second player or it will show duplicates. Prefer talking to
players directly (zbus) over shelling out to `playerctl`.

### Notifications and DND

`org.freedesktop.Notifications` is owned by quickshell (`qs -c ii`) on this machine
**(verified)**. The spec has no DND and no "list". To count/show notifications, the agent must
not become a server: use bus monitoring (`org.freedesktop.DBus.Monitoring.BecomeMonitor`) on
`Notify` / `NotificationClosed` to keep a count, which works with any daemon on a session bus
that permits it (check dbus-broker policy; testing needed). DND is per-daemon: mako
(`makoctl mode`), dunst (`org.dunstproject.cmd0` `paused`), swaync
(`org.erikreider.swaync.cc`), GNOME (`org.gnome.desktop.notifications show-banners`), KDE
(`org.freedesktop.Notifications` `Inhibited` property). A `dnd` toggle widget should be a
small adapter list keyed on the bus-name owner, falling back to "sliver-managed" (hide our own
notification chips only).

### Caffeine / inhibit

`org.freedesktop.ScreenSaver` (`Inhibit`/`UnInhibit`; owned by hypridle on this machine
**(verified)**) and logind `Inhibit(what, who, why, mode)` (fd-based: closing the fd releases).
The caffeine toggle holds an inhibit; the state is "do we hold one". Listing others' inhibitors:
logind `ListInhibitors`.

### Keyboard layout (outside the compositor)

Compositor events win (§2). Input methods also change what the user is typing:
`fcitx5` is running here **(verified)** (`org.fcitx.Fcitx5`; `org.fcitx.Fcitx.Controller1`
`CurrentInputMethod`). Treat it as a second `keyboard_layout` source for CJK users, later.

## 4. Recommended crates

| need | crate | note |
|---|---|---|
| D-Bus | `zbus` | per ADR-0001; use the `tokio` or `async-io` feature, one runtime for the agent (cedar decides) |
| JSON | `serde`, `serde_json` | Hyprland/niri IPC, already used by `sliver-desktop` |
| Wayland protocols | `wayland-client`, `wayland-protocols`, `wayland-protocols-wlr` | foreign-toplevel, idle-notify |
| X11 | `x11rb` | |
| PipeWire | `pipewire` | see §3 |
| udev events | `udev` or `tokio-udev` | backlight/led change |
| icons/desktop entries | `freedesktop-desktop-entry`, `freedesktop-icons` | owned by the render/layout side |
| sway/i3 | none (hand-rolled) | tiny protocol |

## 5. Testing without hardware or a live session

- `sliver_desktop::fake::ScriptedSource` replays `DesktopState`s: use it for resolver and
  agent tests.
- The Hyprland backend is tested against a fake compositor on a temp Unix socket
  (`cargo test -p sliver-desktop`).
- `sliver-session` has `ScriptedSession` and `tests/dbus.rs`: mock logind/UPower/MPRIS services
  on a private `dbus-daemon` (skipped if the binary is missing). `cargo run -p sliver-session
  --example session-watch` prints live state (read-only).
- Lesson from that test: **subscribe to signals before the initial read**, or changes between the
  two are lost (the sources do this; keep it when adding new ones).
- `cargo run -p sliver-desktop --example hypr-watch` prints live JSON states; it is read-only
  and safe on a live Hyprland session.
- For D-Bus sources, test against a private `dbus-daemon --session` with mock services
  (zbus makes this easy); never against the system bus.

## 6. Open items / next tasks

1. Agent wiring: `sliver-agent` consumes `DesktopSource` on a thread and feeds the resolver.
   Needs cedar's state/predicate model; field names above are the proposal.
2. sway/i3 and niri backends (small, each ~1 file).
3. System sources crate (`sliver-session`?): logind, UPower, MPRIS, notifications, behind a
   trait with fakes, like `DesktopSource`.
4. Audio decision (ADR) and brightness widget plumbing.
5. KWin script and GNOME extension (packaging + nested-session tests).
6. Verify `locked` on Hyprland (hyprlock) and `BecomeMonitor` permission on dbus-broker.
