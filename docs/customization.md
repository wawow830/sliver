# Customization reference (waybar model)

Owner: elm (T20 + T11). Status: **spec v1**. sliver-config (T11), the module runtime (T21), the
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
| `/usr/share/sliver/config.toml` | Shipped defaults, used only if neither file above exists. Users can `include` it and override. |
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
include = ["/usr/share/sliver/config.toml"]   # start from the defaults, override below
stylesheet = "style.css"

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
  It gets a generated id (`<type>#<layout>-<zone>-<n>`). The tiny-dfr importer uses this.
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
| `menu` / `menu-trigger` | array of `{label, icon, on-tap}` / gesture | ≈ `menu` + `menu-file` | Opens a temporary sub-bar with these entries and a ✕. Replaces GtkBuilder menus. |
| `swipe-threshold` | px | `smooth-scrolling-threshold` | |
| `interval` | duration or `"once"` | same | For modules that poll. The floor is 100 ms and anything under 1 s gets a warning (invariant 3). |

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
| `mpris` | `mpris` | P1 | `{title}`, `{artist}`, `{status_icon}`, `player`, `ignored-players`. |
| `workspaces` | `hyprland/workspaces`, `sway/workspaces`, `niri/workspaces` | P1 | The backend is chosen automatically. The waybar names are accepted as aliases. |
| `window` | `hyprland/window`… | P2 | Focused app/title. `rewrite` rules as in waybar. |
| `mode` | `hyprland/submap`, `sway/mode` | P2 | |
| `language` | `hyprland/language`, `sway/language` | P2 | Tap = next layout. |
| `idle-inhibitor` | `idle_inhibitor` | P1 | Caffeine. |
| `dnd` | (none; swaync etc. via custom) | P1 | |
| `power-profiles` | `power-profiles-daemon` | P2 | |
| `cpu`, `memory`, `temperature`, `disk`, `load`, `network`, `bluetooth` | same | P2 | |
| `graph` (`custom-graph/<name>`) | `custom-graph` | P2 | A sparkline over `percentage` history. Suits the strip well. |
| `privacy` | `privacy` | P2 | Screenshare/mic/camera in-use indicator. |
| `systemd-failed-units` | same | P3 | |
| `taskbar` | `wlr/taskbar` | P2 | App switcher/dock. |
| `notifications` | (none) | P2 | |
| `emoji` | (none) | P2 | A scrolling strip that opens a layer. |
| `timer` | (none) | P2 | Pomodoro. |
| `image` | `image` | P1 | `path` or `exec` → path, `interval`/`signal`. |
| `group/<name>` | `group/<name>` | P1 | §3.1. |
| `app-controls`, `status-items` | (none) / `tray` | P1 | Placeholders for D-Bus-published items (api.md). `tray` is accepted as an alias, so StatusNotifierItems show here (gap G6). |
| `app/<provider>#<item-id>` | (none) | P1 | **Pin** one D-Bus-published item anywhere, like any module (the stable key from api.md). |
| `custom/<name>` | `custom/<name>` | P1 | §4. Static text when only `format` is set, which is the "button" of other tools. |

### 3.1 Groups and drawers
`group/<name>` takes `modules = [...]`, `orientation = "horizontal" | "vertical"`, and `drawer = {...}`.
- `vertical` stacks children as two 30 px rows. That's fine for display-only modules (cpu over
  mem). The validator warns if a stacked child has actions, because 30 px is below the touch-target advice.
- `drawer`: the first module is the leader and the rest stay hidden until revealed. `reveal =
  "tap" | "hold" | "swipe"` (no hover here). `mode = "inline"` expands in place and pushes neighbors
  (the macOS control strip, Q9/H11). `mode = "bar"` opens a temporary layer with a ✕ (MTMR/BTT
  folders). Also `start-expanded`, `collapse-after`, `transition-duration`, `children-class`, and
  `transition-left-to-right`, all named as in waybar.
- The collapsible control strip in the default config is just `group/control-strip`. Nothing
  about it is hard-coded.

## 4. User modules (`custom/*`): the runtime contract (T21 implements)

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
  `fixed-center`), and `class`. Any number and any names are allowed. The shipped default uses
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
  Themes are just stylesheets. The default `sliver` theme
  (Q8/H10: macOS look plus the desktop accent) is a stylesheet using `@accent`, which the agent
  defines from the desktop.

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
   first on bars that have no physical Esc. Pending H14; this is the recommended option. No
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
| Pango markup | The renderer isn't Pango (ADR-0002) | A markup subset (gap G4) |

## 11. Gaps → tasks

| Gap | Task |
|---|---|
| G1 Config language: includes, deep merge, aliases, conditions, validation, resolver, tiny-dfr import | T11 (elm) |
| G2 Module runtime for `custom/*` with the shared output contract for built-ins | T21 |
| G3 Stylesheet engine and selectors from §7 | T22 |
| G4 Markup subset in format strings (`<b>`, `<i>`, `<span color=…>`) | new |
| G5 `format` engine: placeholders, `{name:spec}`, format-icons/states/alt/status resolution, shared by all modules | new |
| G6 StatusNotifierItem host → `status-items`/`tray` | new |
| G7 Hot reload watcher + last-good fallback + `sliverctl reload`/SIGUSR2 | new |
| G8 waybar module-block importer (JSONC → TOML, alias names) | new, P3 |
| G9 Settings app writes these exact files with comments preserved (toml_edit), and shows unknown-key warnings | settings (later) |
