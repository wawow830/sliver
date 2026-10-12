# Customization reference (waybar model)

Owner: elm (T20 + T11). Status: **spec v1**, implemented by `crates/sliver-config` (ADR-0004). sliver-config (T11), the module runtime (T21), the
stylesheet (T22), sliverctl and the D-Bus API all follow this doc. Change it via a PR to this doc
plus a message to the owners of the parts it touches.

GOAL.md says to be **as customizable as possible, with waybar as the model**. Everything is a module,
every module is shaped by config, styling is a stylesheet, the structure (zones, layers, rules) is
user-defined, and the text config is the source of truth. The **only** things a user can't change
are the invariants in §9.

## 1. Files

| Path | Role |
|---|---|
| `$XDG_CONFIG_HOME/sliver/config.toml` (default `~/.config/sliver/config.toml`) | User config. **The first file found wins**, as in waybar. |
| `/etc/xdg/sliver/config.toml` | System-wide config, used only if there's no user file. |
| `/usr/share/sliver/config.toml` | Packaged config, used only if neither file above exists. |
| built-in defaults (`crates/sliver-config/src/builtin.toml`) | **Always merged underneath** whichever file is used, so the `base`/`fn`/`media` layers, the `default`/`fkeys`/`media` layouts and the control strip exist even with an empty config. Any of them can be redefined. |
| `style.css` next to the active config (or `stylesheet = "..."`) | Stylesheet (T22). `style-dark.css` and `style-light.css` follow the desktop color scheme when present. |

- The format is **TOML** (ADR-0001). Comments are allowed. Keys are **kebab-case** and use waybar's
  names wherever the concept maps, so module blocks are easy to port.
- `include = ["modules.toml", "~/dotfiles/sliver/*.toml"]` takes paths relative to the including
  file, plus `~` and globs. Includes are **deep-merged** tables. As in waybar, the **including file
  wins** over included files, and earlier includes win over later ones. Arrays are replaced, not
  concatenated. Nested includes are allowed, but cycles and more than 8 levels are errors.
- **Hot reload:** the agent watches the config, every include and the stylesheet with inotify,
  with no polling. `sliverctl reload` and `SIGUSR2` to `sliver-agent` force a reload. An invalid
  config is **never applied**: the last good config stays live, and the error shows up in the
  settings Status page and `sliverctl status`, with an optional desktop notification.
- Unknown keys produce a **warning** (they catch typos), not an error.

## 2. Shape of a config

```toml
version = 1
include = ["modules.toml"]   # optional; the built-in defaults are always underneath
stylesheet = "style.css"
theme = "sliver"          # base theme under your stylesheet: sliver (default) | light | high-contrast | classic

[bar]                     # bar-wide options (waybar's per-bar options that make sense here)
name = "main"             # becomes a CSS class on the bar
spacing = 8               # px gap between modules
margin = 0                # px margin around the strip
fn-mode = "hold"          # Fn key: hold | toggle
fire-on = "release"       # default for taps: press | release (each module can override)

[display]
brightness = "adaptive"   # adaptive | fixed
max-brightness = 128      # 0-255
dim-after = "30s"
off-after = "60s"
pixel-shift = false

[display.on-battery]      # optional: replaces these [display] keys while on battery (UPower)
max-brightness = 60
off-after = "20s"

[layers]                  # layer name -> its default layout. base and fn are reserved names.
base = "default"
fn = "fkeys"
media = "media"           # custom layer, opened by an action

[[layouts.default.zones]]
name = "esc"
modules = []
[[layouts.default.zones]]
name = "main"
width = "fill"
modules = ["app-controls"]
[[layouts.default.zones]]
name = "controls"
modules = ["status-items", "mpris", "group/control-strip"]

[layouts.firefox]
extends = "default"
[[layouts.firefox.zones]]     # overrides the zone with the same name and keeps its position
name = "main"
modules = ["key/Back", "key/Forward", "key/Refresh", "app-controls"]

[[rules]]                     # top to bottom, first match wins (ux.md §2)
name = "Terminals get F-keys"
when = { app-id = ["foot", "kitty", "org.wezfurlong.wezterm"] }
layer = "base"                # optional, default "base"
layout = "fkeys"

[modules.clock]
format = "{:%H:%M}"
format-alt = "{:%a %d %b}"    # a tap toggles to it when on-tap isn't set (waybar behavior)

[modules.battery]
states = { warning = 30, critical = 15 }
format = "{icon} {capacity}%"
format-charging = "󰂄 {capacity}%"
format-icons = ["󰁺", "󰁼", "󰁾", "󰂀", "󰁹"]

[modules."group/control-strip"]
modules = ["backlight/slider", "audio/slider", "audio#mute"]
drawer = { reveal = "tap", mode = "inline", collapse-after = "5s" }

[modules."custom/vpn"]
exec = "~/bin/vpn-status --json"
return-type = "json"
interval = "5s"
signal = 8
on-tap = "~/bin/vpn-toggle"
on-hold = { open = "org.gnome.Settings.desktop" }
```

The old tiny-dfr `MediaLayerDefault = true` is just `[layers] base = "media"`, `fn = "fkeys"`.

## 3. Modules

### Naming and instances
- A module reference is `type`, `type#instance`, or `namespace/name[#instance]`, exactly as in
  waybar. `battery#bat2` is a second battery and is configured by `[modules."battery#bat2"]`. The
  instance name becomes a CSS class (`#battery.bat2`).
- **Parametric built-ins** need no block. `key/<KeyName>` is a key button with label and icon
  defaults and `on-tap = { key = "<KeyName>" }`. Key names are `input_linux::Key` variant names
  (what tiny-dfr accepts, e.g. `F1`, `BrightnessDown`, `PlayPause`), matched case-insensitively.
  A `[modules."key/F1"]` block, if present, is merged on top.
- **Inline modules:** a zone's `modules` list may contain an inline table instead of a name, for
  example `{ type = "custom", format = "TTY1", on-tap = { keys = ["LeftCtrl","LeftAlt","F1"] } }`.
  `type` is a module reference without an instance (`"key/F1"`, `"clock"`, `"custom"`). It gets a
  generated instance (`<type>#<layout>-<zone>-<n>`). The tiny-dfr importer uses this. An empty
  `icon = ""` or `format = ""` clears a default.
- **Built-ins have no special powers.** Every built-in uses the same runtime interface as
  `custom/*`: it produces the same output fields (§4), has the same common keys, and can be
  styled, gesture-bound and placed the same way.

### Common keys (every module)

| Key | Type | waybar | Meaning |
|---|---|---|---|
| `format` | string | same | `{placeholder}` and `{name:spec}` (strftime for clock). The placeholders depend on the module. |
| `format-alt` | string | same | Toggled by a tap if `on-tap` is unset, or by the `{ format = "toggle-alt" }` action. |
| `format-<state>` / `format-<status>` / `format-<status>-<state>` | string | same | E.g. `format-charging`, `format-critical`. |
| `format-icons` | array / table / string | same | An array is indexed by `percentage` from low to high. A table is keyed by `alt` or state and can contain nested arrays. **Sliver extension:** an entry can be text (a glyph) or `{ icon = "name-or-path" }` for a themed/SVG/PNG image. |
| `states` | table name → number | same | Thresholds on `percentage`. They add a class and select `format-<state>`. |
| `icon` | string | ≈ `image-name`/`image-path` | XDG icon name, file path, or `tiny-dfr:<name>` (the dfr icon dirs). |
| `icon-theme` | string | (none) | Overrides the theme for this module. |
| `icon-size` | int | same | px |
| `width` | `"auto"` \| px int \| `"fill"` \| `{ fill = N }` | ≈ `expand` | Q4/H6. `expand = true` is accepted as `"fill"`. |
| `min-length` / `max-length` | int (chars) | same | Labels are ellipsized past max-length. |
| `align` / `justify` | float / string | same | Label alignment within the module. |
| `class` | string or array | (none) | Static extra CSS classes. |
| `when` | condition (§6) | (none) | Shows the module only while the condition holds (e.g. "battery only when on battery"). |
| `hide-empty-text` | bool | same | Hides the module when its text is empty. |
| `detail` / `detail-format` | bool / string | `tooltip` / `tooltip-format` | There's no hover on a touch strip. Detail is used for the accessible description, the settings preview, and the hold-to-peek overlay when `on-hold` is unbound. |
| `fire-on` | `press` \| `release` | (none) | Overrides `[bar] fire-on`. `key/*` defaults to press. |
| `repeat` | bool | (none) | Hold-to-repeat, the default for `key/*`. |
| `on-tap`, `on-double-tap`, `on-hold`, `on-release`, `on-swipe-left`, `on-swipe-right`, `on-drag`, `on-update` | action (§5) | `on-click`, `on-double-click`, `on-click-right`, `on-click-release`, `on-scroll-down`, `on-scroll-up`, n/a, `on-update` | The waybar names are accepted as aliases, so ported blocks work. Double-tap delays that module's single tap, so it's opt-in. |
| `actions` | table gesture → module-action name | same | Bind gestures to a module's *built-in* actions, e.g. `actions = { on-hold = "mode" }` on clock. |
| `menu` / `menu-trigger` | array of `{label, icon, on-tap}` / gesture | ≈ `menu` + `menu-file` | Done. Opens a temporary sub-bar with these entries and a ✕ (it covers the layout like a modal context: the first zone stays). The trigger defaults to tap, or hold when the module also has a tap action. Picking an entry runs its action and closes the sub-bar. Replaces GtkBuilder menus. Preset: `examples/config/power-menu.toml` (a `custom/power` button with lock, suspend, reboot and power off, and a `custom/screenshot` button for the Print key); copy it or `include` it. |
| `swipe-threshold` | px | `smooth-scrolling-threshold` | |
| `pass-through` | bool | (none) | App-published modules (`app/<provider>#<id>`): also send the gesture to the app when a user `on-*` binding exists. Default false: the user binding replaces the app's event (api.md §5). |
| `interval` | duration or `"once"` | same | For modules that poll. The floor is 100 ms and anything under 1 s gets a warning (invariant 3). |

**Format semantics** (`sliver_config::format`, same as waybar):
- The format is chosen in this order: `format-alt` (if toggled), `format-<status>-<state>`,
  `format-<status>`, `format-<state>`, `format`, then the module default.
- `states` use *lower is worse* for battery-like modules (`warning = 30` means ≤ 30%) and *higher
  is worse* for cpu, memory and temperature.
- An array `format-icons` is indexed by `floor(pct / (100 / n))`. A table is looked up by alt,
  then status, then state, then `default`.
- `{icon}` that resolves to an image renders as empty text, and the image goes to the renderer.
- Specs: the module's own first (strftime for the clock), else `[[fill]align][0][width][.precision]`.
- Unknown placeholders render literally. `max-length` cuts with `…`, and a bad format string is a
  validation error.

Durations are `"250ms"`, `"5s"`, `"2m"`, or a bare number of seconds (waybar compat).

### Built-in module types (v1 target set)

| sliver | waybar equivalent | Tier (features.md) | Notes |
|---|---|---|---|
| `key/<Key>` | (none, from tiny-dfr) | P0 | Esc, F1–F24 and media keys. Emitted by sliverd through uinput. |
| `spacer` | (CSS margins) | P0 | `width` decides its size. |
| `clock` | `clock` | P0 | `{:%strftime}`, `locale`, `timezones` with swipe to cycle (like waybar's scroll). |
| `battery` | `battery`, `upower` | P0 | `{capacity}`, `{time}`, `{status}` states, `format-charging`… |
| `backlight`, `backlight/slider` | same | P0/P1 | Slider: press-and-slide. `on-drag` gets `{value}`. |
| `kbd-backlight`, `kbd-backlight/slider` | (none) | P0/P1 | |
| `audio`, `audio/slider` | `pulseaudio`, `wireplumber`, `pulseaudio/slider` | P0/P1 | `audio#mic` with `node = "source"` for the mic. |
| `mpris` | `mpris` | P1 | Done. `{title}` `{artist}` `{status_icon}` `{album}` `{player}`; tap play/pause, swipe next/previous, **hold switches to the next player** when several run. Album art (`mpris:artUrl`: `file://`, or `http(s)://` fetched once with curl into `~/.cache/sliver/art`; PNG and JPEG) is the icon (`art = false` turns it off). `scrub = true` turns the module into a progress slider (position extrapolated while playing, no polling) that seeks on drag. |
| `workspaces` | `hyprland/workspaces`, `sway/workspaces`, `niri/workspaces` | P1 | Done. One button per workspace (the focused output's, or all with `all-outputs = true`); classes `.active` `.urgent` `.empty` `.default`, `:checked` on the active one; `{name}` `{id}` `{windows}`; tap switches workspace. The backend is chosen automatically (Hyprland, sway, niri). The waybar names are accepted as aliases. |
| `window` | `hyprland/window`… | P2 | Focused app/title. `rewrite` rules as in waybar. |
| `mode` | `hyprland/submap`, `sway/mode` | P2 | |
| `language` | `hyprland/language`, `sway/language` | P2 | Tap = next layout. |
| `idle-inhibitor` | `idle_inhibitor` | P1 | Caffeine. |
| `dnd` | (none; swaync etc. via custom) | P1 | Shows Do Not Disturb when the notification daemon reports it (dunst, KDE); hidden otherwise. Tap switches it on dunst (`paused`); other daemons have no standard control, so tapping logs that and does nothing. |
| `power-profiles` | `power-profiles-daemon` | P2 | |
| `cpu`, `memory`, `temperature`, `disk`, `load` | same | P2 | Done (`sliver-stats`). Read from /proc and /sys when drawn, refreshed on `interval` (defaults: cpu/temperature/load 10 s, memory/disk 30 s) only while on the bar and the display is on. Placeholders: cpu `{usage}` `{load}` `{avg_frequency}` `{spark}` (a text sparkline `▁▃▅█` of the last 16 samples); memory `{percentage}` `{used}` `{total}` `{avail}` (GiB) `{swapPercentage}` `{swapUsed}` `{swapTotal}` `{swapAvail}`; load `{load1}` `{load5}` `{load15}`; temperature `{temperatureC}` `{temperatureF}` `{temperatureK}`, keys `thermal-zone`, `hwmon-path`, `critical-threshold` (adds `.critical`; default source is the hottest sensor); disk `{percentage_used}` `{percentage_free}` `{used}` `{free}` `{total}` (GiB) `{path}`, key `path` (default `/`). `states` use `percentage` (cpu, memory, disk). |
| `network` | same | P2 | Done (NetworkManager over D-Bus, `sliver-session`). `{essid}` `{ifname}` `{signalStrength}` `{ipaddr}` `{state}`; classes `.wifi` `.ethernet` `.vpn` `.disconnected` `.connecting`; percentage = Wi-Fi signal for `states`; icon follows the signal. Tap switches the Wi-Fi radio (`.off` class when it is off). Hidden without NetworkManager. Throughput placeholders `{bandwidthDownBytes}` `{bandwidthUpBytes}` `{bandwidthTotalBytes}` (e.g. `1.5MB/s`), the same with `Bits`, and `{spark}` (download history) are read from /proc/net/dev **only when the module has an explicit `interval`** (default: none, so a plain network cell never wakes the bar). |
| `bluetooth` | same | P2 | Done (BlueZ over D-Bus). `{device_alias}` `{device_battery_percentage}` `{num_connections}` `{status}`; classes `.off` `.on` `.connected`. Tap switches the adapter on or off. Hidden without an adapter. |
| `graph` (`custom-graph/<name>`) | `custom-graph` | P2 | A sparkline over `percentage` history. Suits the strip well. |
| `privacy` | `privacy` | P2 | Screenshare/mic/camera in-use indicator. |
| `systemd-failed-units` | same | P3 | |
| `taskbar` | `wlr/taskbar` | P2 | Done (with `pinned = [\"org.mozilla.firefox\", \"kitty\"]`: launchers first; a running pinned app is its window's button, an idle one starts the app on tap; `.pinned` `.running` `.idle`). One icon per window of the active workspace (`all-workspaces = true` for all), icon from the app's `.desktop` entry (first letters if none); `.focused` / `.urgent`; `{title}` `{app_id}` for a custom `format`; tap focuses, hold closes the window. |
| `notifications` | (none) | P2 | Done. A bell with the latest notification's summary (`{summary}` `{app}`; never the body) or the number of open notifications (`{count}`); classes `.pending`/`.none` and `.dnd`. Tap invokes the default action (via `makoctl` where installed, else it dismisses); swipe left/right dismisses (`CloseNotification`). Hidden while the screen is locked like every module that could leak content. |
| `emoji` | (none) | P2 | Done. As a button (default) it opens the built-in `emoji` layer; the layer is the same module in `mode = "strip"`: 10 tabs (recents + Unicode groups), one page of emoji (`columns`, default 14), swipe left/right to page, ✕ to close. Tapping an emoji types it (`insert` = `auto` (wtype if installed, else clipboard), `wtype`, `paste` (wl-copy then Ctrl+V; replaces the clipboard) or `copy`). Recents persist in `$XDG_STATE_HOME/sliver/emoji-recents`. Data: Unicode emoji up to 15.1 (`crates/sliver-emoji`). Open it from any button with `{ layer = "emoji", mode = "toggle" }`. |
| `timer` | (none) | P2 | Done. Countdown (`duration`, default 25m; `{remaining}` `{minutes}` `{seconds}`, `percentage` for `states`); tap starts/pauses, hold resets; `notify-send` at zero. Status classes `.idle` `.running` `.paused` `.finished`. |
| `image` | `image` | P1 | `path` or `exec` → path, `interval`/`signal`. |
| `group/<name>` | `group/<name>` | P1 | §3.1. |
| `app-controls`, `status-items` | (none) / `tray` | P1 | Placeholders for D-Bus-published items (api.md). `tray` is accepted as an alias, so StatusNotifierItems show here (gap G6). |
| `app/<provider>#<item-id>` | (none) | P1 | **Pin** one D-Bus-published item anywhere, like any module (the stable key from api.md). |
| `custom/<name>` | `custom/<name>` | P1 | §4. Static text when only `format` is set, which is the "button" of other tools. |

### 3.1 Groups and drawers
`group/<name>` takes `modules = [...]`, `orientation = "horizontal" | "vertical"`, `module-width`
(the default width of its children, as on a zone), and `drawer = {...}`.
- `vertical` stacks children as two 30 px rows. That's fine for display-only modules (cpu over
  mem). The validator warns if a stacked child has actions, because 30 px is below the touch-target advice.
- `drawer`: the first module is the leader and the rest stay hidden until revealed. `reveal =
  "tap" | "hold" | "swipe"` (no hover here). `mode = "inline"` expands in place and pushes neighbors
  (the macOS control strip, Q9/H11). `mode = "bar"` opens a temporary layer with a ✕ (MTMR/BTT
  folders). Also `start-expanded`, `collapse-after`, `transition-duration`, `children-class`, and
  `transition-left-to-right`, all named as in waybar.
- The default base layout's main zone is `app-controls` followed by `group/fkeys` (F1–F12,
  `module-width = "fill"`, `when = { not = { app-controls = true } }`). The bar shows F-keys
  until the focused app publishes controls, then gives the app the space. This came from birch's
  real-bar run: few Linux apps publish, and this Mac has no F-key row.
- The collapsible control strip in the default config is just `group/control-strip`. Nothing
  about it is hard-coded.

## 4. User modules (`custom/*`): the runtime contract (`crates/sliver-modules`)

Runtime behavior: `Runtime::start(specs)` gives one thread per module with an `exec`, asleep on a
channel until its deadline, a trigger (`update MODULE`, `exec-on-event`) or a signal
(`forward_realtime_signals`). Updates are sent only when the output changed. `set_active(false)`
(display off, screen locked) arms no timers at all and `set_active(true)` re-runs everything
once. Commands run as `sh -c` in their own process group (a timeout kills the whole tree), never
as root, with `no_new_privs`, no stdin and at most 64 KiB of output. A failed run shows `!` with
the `error` class and the last stderr line as the detail. Continuous scripts restart after
`restart-interval` (min 1 s), doubling up to 60 s while they keep dying quickly.

| Key | waybar | Meaning |
|---|---|---|
| `exec` | same | A command (`sh -c`, user session, never root). |
| `exec-if` | same | Runs `exec` only if this exits 0. |
| `interval` | same | Poll period, or `"once"`. **No interval and no signal = continuous**: each stdout line is an update. |
| `restart-interval` | same | For continuous scripts that exit. Has backoff, and a floor of 1 s. |
| `signal` | same | `pkill -RTMIN+N sliver-agent` or `sliverctl signal N` re-runs it. |
| `exec-on-event` | same | Re-run `exec` after any `on-*` action fires (default true). |
| `return-type` | same | `""` = i3blocks-style lines (`text\ndetail\nclass`). `"json"` = one JSON object per line. |
| `escape` | same | Escape markup in the output. |
| `timeout` | (none) | Per-run timeout (default 2 s). On timeout, the module shows its `error` class and "!". |
| `env` | (none) | Extra environment. Every command also gets `SLIVER_MODULE`, `SLIVER_LAYER`, `SLIVER_LAYOUT`, `SLIVER_APP_ID`. |

JSON output fields are waybar's `text`, `alt`, `tooltip` (→ detail), `class` (string or array) and
`percentage`, plus the sliver extensions `icon` (name or path), `value` (0–100, drives a
`custom` slider via `slider = true`) and `hidden` (bool). **Built-in modules produce exactly
these fields.** That's the "no special powers" rule, and it's also what the D-Bus API carries for
long-running programs.

## 5. Actions

Every `on-*` key takes one of these:

| Form | Example | Meaning |
|---|---|---|
| string | `"pavucontrol"` | Shell command (waybar compat). `{value}` is substituted for `on-drag`. |
| `{ exec = [argv] }` | `{ exec = ["foot", "-e", "htop"] }` | No shell. |
| `{ key = K }` | `{ key = "F5" }` | Key press, emitted by sliverd through uinput. Honors `repeat`. |
| `{ keys = [K…] }` | `{ keys = ["LeftCtrl","LeftAlt","F1"] }` | Combo: all pressed in order, released in reverse. |
| `{ layer = L, mode = M }` | `{ layer = "media", mode = "toggle" }` | `M` is `hold` / `toggle` / `oneshot`. |
| `{ close-layer = true }` | | Returns to the layer underneath. |
| `{ layout = N }` | | Forces a layout until the context changes. |
| `{ open = desktop-id }` / `{ url = U }` | `{ open = "org.gnome.Nautilus.desktop" }` | Launches through the portal or xdg-open. |
| `{ dbus = { bus, dest, path, interface, method, args } }` | | `bus` is `session` (default) or `system`. |
| `{ signal = N }` / `{ update = "custom/vpn" }` | | Refreshes modules. |
| `{ format = "toggle-alt" }` | | Toggles `format-alt`. |
| `{ module-action = NAME }` | `{ module-action = "next-player" }` | A built-in module action (also reachable via `actions`). |
| array | `[{ keys = ["LeftCtrl","c"] }, { sleep = "100ms" }, "notify-send copied"]` | Macro, capped at 32 steps and 10 s total. |

Bar-wide gestures live in `[gestures]`: `two-finger-swipe-left/right`, `edge-swipe-from-left`, and
so on, with the same action forms.

## 6. Structure: zones, layers, layouts, rules

- **Zones** are user-defined per layout: an ordered `zones` array, where each zone has `name`,
  `modules`, `width` (`"auto"`/px/`"fill"`), `align` (`start`/`center`/`end`, like waybar's
  `fixed-center`), `module-width` (the default width for its modules, e.g. `"fill"` for
  tiny-dfr-style equal keys), and `class`. Any number and any names are allowed. The shipped default uses
  `esc`/`main`/`controls`, which is a convention, not a rule. A zone becomes a stylesheet class
  `.zone-<name>` (waybar's `.modules-left`).
- **Layouts:** `[layouts.<name>]` with optional `extends`. A child's zone **overrides the
  parent's zone with the same name in place** (Q5/H7: whole zones). New zone names append to the
  end unless `zone-order = [...]` is given. Built-in layouts `fkeys` and `media` (dfr's layers)
  always exist and can be overridden by defining them.
- **Layers:** `[layers]` maps each layer name to its default layout. `base` and `fn` are reserved
  (Fn key, `fn-mode`). Any other name is a custom layer opened by a `{ layer = … }` action.
  Per-layer options (`timeout`, `close-button`) go in `[layer-options.<name>]`.
- **Rules:** `[[rules]]` with `name`, `when`, `layer` (default `base`) and `layout`. **First match
  top to bottom wins.** If none matches, the `[layers]` default is used.
- **Conditions** (`when`, here and on modules) form a table whose keys are context keys
  (integration.md §1; `app-id` and `app_id` are the same key). All keys must match. `any = [ {…},
  {…} ]` means at least one of the sub-tables matches, and `not = {…}` negates. A value can be:
  - a literal (bool, number, or string; `app-id` compares case-insensitively and also matches
    the .desktop id)
  - an array (one of)
  - `{ regex = "…" }` / `{ contains = "…" }`
  - `{ below = N }` / `{ above = N }`

  An unknown or unavailable key never matches.

## 6b. Apps and providers on the bar (api.md §4)

```toml
[apps."*"]                     # defaults for every app
whole-bar = true               # modal app contexts may cover the bar beyond the first zone

[apps."org.mozilla.firefox"]   # app id, case-insensitive, .desktop suffix ignored (like conditions)
controls = true                # may this app's (or a provider's) items fill App controls
pin = "org.example.Tabs"       # only this provider fills App controls for the app

[providers."org.example.Pomodoro"]   # D-Bus provider id (a Flatpak/Snap id when sandboxed)
show = false                   # hide it everywhere: Status items and App controls
allow = true                   # needed before a *sandboxed* provider may show status items
order = 1                      # Status items order: lower first, unlisted after by id

[api]
target = true                  # allow target-scope contexts (scripts that target another app)
```

`sliver_config::Config::app_policy(app_id)` resolves `[apps.*]` with the `"*"` defaults, and
`provider_prefs(id)` resolves `[providers.*]`. The agent turns them into the API registry's policy
(cedar).

## 7. Styling (T22 implements; renderer side)

These are the selectors sliver guarantees, mirroring waybar:
- `#<type>` (`#clock`, `#custom-vpn`, `#key-f1`, `#group-control-strip`) and `#<type>.<instance>`
  (`#battery.bat2`).
- `.<class>` from `states`, module status (`.charging`, `.muted`, `.playing`), JSON `class`,
  static `class`, and `.drawer-child`.
- `:active` while pressed, `:disabled`, `:first-child`/`:last-child`, and `.error` for a failed script.
- The style tree is `bar > zone > module`. The agent passes `(type, id, classes)` and the renderer
  does the cascade (ash, T22).
- `.zone-<name>`, and `bar.layer-<name>`, `bar.layout-<name>`, `bar.<bar name>`, `bar.locked`,
  `bar.dimmed` on the strip root.
- Properties (initial set): `color`, `background`/`background-color`, `border`, `border-radius`,
  `border-color`, `border-width`, `padding`, `margin`, `font-family`, `font-size`, `font-weight`,
  `opacity`, `min-width`, `transition`. GTK-isms that waybar users paste are accepted: `@define-color`, `@import`, `alpha()`, `shade()`,
  `mix()`. Unsupported properties or selectors warn with file:line and never fail the load.
  Themes are just stylesheets. `theme = "…"` in config.toml picks the built-in base theme; your
  stylesheet goes on top of it, and can also `@import "sliver:NAME"`. Built in:
  - `sliver` (default; Q8/H10): the macOS look with `@accent`, which the agent defines from the desktop.
  - `light`: dark text on light keys, also with `@accent`. Lit pixels cost power on the OLED
    panel, so this one draws more than the dark themes.
  - `high-contrast`: white outlines, bold larger text, pressed inverts and "on" is solid yellow, so
    no state depends on hue alone. Black background, so as cheap on power as the default.
  - `classic`: tiny-dfr 0.3.7's look (what the tiny-dfr importer picks).

  Every theme gets the desktop's `@accent`. An unknown name warns and falls back to `sliver`.

## 8. Scripting from outside

`sliverctl` and D-Bus Control1 (api.md) can:
- `reload`, `signal N`, `update MODULE`
- `tap|hold MODULE`
- `show|hide MODULE`
- `layer push|pop|toggle NAME`, `layout NAME`
- `bar show|hide|toggle`. Hiding blanks the bar, but Fn → F-keys still works (§9).
- `get/set KEY.PATH VALUE` (writes the config file, which then hot-reloads)
- `render --png` (headless preview)

## 9. The only things you can't configure (GOAL invariants, enforced by sliver-config's validator and the resolver)

1. **F1–F12 and Esc stay reachable through a hard-wired hatch, not through config.** The Fn layer
   and every layout are **fully editable** (Q2/H4, revised to "fully editable"). A config may
   drop the F-keys and Esc entirely. Reachability is guaranteed by **the safe hatch in sliverd**:
   double-press Fn and hold, and the built-in F1–F12 safe layer shows while Fn is held, with Esc
   first on bars that have no physical Esc. Decided in H14 (the human delegated it; elm picked this). ≤400 ms between the two
   presses, implemented in `sliver_ipc::hatch` (api.md §6). No
   config can change or disable the hatch. The validator only **warns** when `base ∪ fn` has no
   F1–F12, or when a no-physical-Esc bar has no Esc.
2. **Esc default on bars with no physical Esc** (2170 px models): the shipped default config and
   the tiny-dfr importer put `key/Esc` first in base and fn, as dfr did. Users can remove it, and
   the hatch covers that. On this 2008 px Mac14,7 (physical Esc, H2) it's absent by default.
3. **Touch targets** are advice, not an invariant: the validator *warns* when a module with an
   action is narrower than 80 px, and warns loudly below 60 px (birch, features.md §9). It never
   refuses.
4. **Privacy while locked:** app items, `custom/*` output and notification text are never shown
   on the lock screen, whatever the config says.
5. **Power:** interval floor 100 ms, restart floor 1 s, script timeouts, and no busy loops.
6. **Apps can't override the user's layout outside placeholders, and they can't block the hatch.**
   Modal app contexts never cover the user's first zone. A config error never reaches the bar
   (the last good config stays, and sliverd's safe layer is the backstop).

## 10. waybar parity map: features that don't apply

| waybar | Why it doesn't apply | Closest sliver thing |
|---|---|---|
| `layer`, `output`, `position`, `height`, `width`, `exclusive`, `passthrough`, `mode`, `ipc`, `id`, `modifier-reset` | One fixed 2008×60 panel, not a Wayland surface | `[bar]` margin/spacing. Rules on `monitor` for per-output behavior. |
| Multiple bars in one config | One physical bar | Layouts + rules + layers |
| `start_hidden`, `on-sigusr1` | | `sliverctl bar hide/toggle`, `SIGUSR1` → toggle. Fn still works. |
| Hover and `tooltip` | No hover on touch | `detail` (§3) |
| Mouse buttons (middle/back/forward, triple-click) | Touch has none of them | tap, double-tap, hold, swipe, drag |
| `rotate` | The bar is 60 px tall | none |
| `cffi` (in-process C plugins) | Crash safety: a plugin must never take the bar down | `custom/*` exec, or the D-Bus API for long-running programs |
| `cava` | Power cost (invariant 3) | P3, maybe an opt-in with a frame-rate cap |
| GtkBuilder `menu-file` | No GTK on the bar | `menu = [...]` sub-bar |
| Pango markup | The renderer isn't Pango (ADR-0002) | A markup subset: tags `b i u s big small sub sup tt span`; `span` attributes `foreground/fgcolor/color`, `weight`, `style`, `size` (keywords, `NN%`, `larger`/`smaller`), `underline`, `strikethrough`, `font_family/face`; entities `&amp; &lt; &gt; &quot; &apos; &#N;`. Background colours, baseline shift and font descriptions are ignored (stylesheets cover them). Bad markup keeps its text and reports a warning (`markup::parse(..).warnings`); it never blanks a module. Output that must stay literal is escaped (`escape = true`) and still goes through the markup path. |

## 11. Gaps → tasks

| Gap | Task |
|---|---|
| G1 Config language: includes, deep merge, aliases, conditions, validation, resolver, tiny-dfr import | T11 (elm) |
| G2 Module runtime for `custom/*` with the shared output contract for built-ins | T21 |
| G3 Stylesheet engine and selectors from §7 | T22 |
| G4 Markup subset in format strings (`<b>`, `<i>`, `<span color=…>`) | done: `sliver_render::markup` + `Content::Markup` / `IconMarkup` (T28) |
| G5 `format` engine: placeholders, `{name:spec}`, format-icons/states/alt/status resolution, shared by all modules | done: `sliver_config::format` (T25) |
| G6 StatusNotifierItem host → `status-items`/`tray` | new |
| G7 Hot reload watcher + last-good fallback + `sliverctl reload`/SIGUSR2 | done (T26, docs/agent.md) |
| G8 waybar module-block importer (JSONC → TOML, alias names) | done: `crates/sliver-waybar` (`import_path`, example `waybar-import`) (T29) |
| G9 Settings app writes these exact files with comments preserved (toml_edit), and shows unknown-key warnings | settings (later) |
