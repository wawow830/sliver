# Feature inventory

GOAL.md's "complete", as a prioritized list. The UX is in [ux.md](ux.md), API shapes in
[api.md](api.md), hardware facts in [hardware.md](hardware.md).

## Tiers

| Tier | Meaning | Exit criterion |
|---|---|---|
| **P0 Parity** | sliverd can replace tiny-dfr with zero regressions. | A tiny-dfr user swaps and notices nothing, except that crashes now fall back to the safe layer. |
| **P1 MVP** | The first release worth switching for. | The editor, per-app layouts on Hyprland, sliders, now playing, scripts, API v1. |
| **P2 Complete** | Everything GOAL.md lists. | Every GOAL.md bullet is checked off. |
| **P3 Polish** | Beyond the goal, or community-driven. | Optional. |

Prior art key: **mac** = macOS Touch Bar, **BTT** = BetterTouchTool, **MTMR** = My TouchBar My
Rules, **Pock**, **dfr** = tiny-dfr 0.3.7 (config in `/usr/share/tiny-dfr/config.toml`).

## 1. Safety and core (P0, non-negotiable)

| Feature | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Built-in **safe layer** inside sliverd: F1–F12 (with Esc first on bars that have no physical Esc), plus media on Fn | P0 | done | dfr (it's the only layer) | Shown whenever the agent, config or a widget fails. Needs no config and only built-in fonts. Returns 5–8 ms after the agent is killed. |
| Esc and F-keys always reachable via sliverd's **safe hatch** (double-press Fn and hold → built-in F-keys), even though every layout including Fn is fully editable. Defaults put Esc first on 2170 px bars | P0 | done | mac, dfr | dfr adds Esc only on bars ≥2170 px. The 2008 px Mac14,7 has a physical Esc. See ux.md §7. |
| Fn held → the user's Fn layer (fully editable). Fn-Fn-hold → built-in F-keys, from any state | P0 | done | mac, dfr | The hatch works even with app modals, custom layers, and a config with no F-keys. |
| Per-app "show F-keys by default" | P1 | done | mac (Keyboard prefs) | For terminals, IDEs and games: a rule pointing at the built-in `fkeys` layout (§5). |
| Render only on change, idle dim → off | P0 | done | dfr | Invariant 3, and the main OLED burn-in protection. Defaults: dim at 30 s, off at 60 s, like dfr. Idle: sliverd 0 wakeups/s, agent ≈ 3–6/s (`scripts/idle-wakeups`). |
| Wake on touch: the first touch on a dark bar only wakes it and doesn't fire an item | P0 | done | dfr | Avoids accidental presses on a bar you can't see. |
| Headless renderer to PNG and simulated touch | P0 | done | (none) | Invariant 4. The settings preview uses the same renderer. |
| tiny-dfr config import (`/usr/share` + `/etc` merge) | P0 | done | dfr | Covers every dfr key, including `PrimaryLayerKeys`/`MediaLayerKeys` (Text/Icon/Theme/Time/Locale/Battery/Stretch/Action, combos, spacers). |
| Coexist with or replace tiny-dfr cleanly | P0 | done | (none) | The RPM replaces tiny-dfr (ExecStartPre= handover both ways); a failing sliverd falls back to tiny-dfr; the safe-swap harness borrows the bar and gives it back. |

## 2. Items and widgets

"Item" means anything that sits on the bar. A widget is an item with live content.

| Widget | Tier | Status | Prior art | Gestures | Needs | Notes |
|---|---|---|---|---|---|---|
| Key button: text and/or icon, single key or combo | P0 | done | dfr, MTMR staticButton | tap, hold = key repeat | uinput (daemon) | Keys come from the `sliver-touch-bar-keys` device; held keys use the compositor's normal repeat. |
| Spacer: fixed or flexible | P0 | done | dfr `{Stretch}`, MTMR | none |  |  |
| Clock: 12h/24h/strftime, locale, time zones | P0 | done | dfr Time, MTMR timeButton | tap → action (e.g. calendar) |  |  |
| Battery: percent/icon/both, charging and low colors | P0 | done | dfr Battery, MTMR | tap → power settings | UPower in the agent; sysfs fallback in the daemon |  |
| Media keys: brightness ±, kbd backlight ±, volume ±, mute, mic mute, prev/play/next, search | P0 | done | dfr media layer | tap, hold-repeat | uinput keycodes | Whether a media key repeats is up to the compositor bind, as with a physical keyboard. |
| **Brightness / volume / kbd-backlight slider** | P1 | done | mac control strip, BTT | press-and-slide, drag, tap to expand | logind backlight, PipeWire/wpctl |  |
| **Now playing (MPRIS)**: title/artist/art, prev/play/next, scrub | P1 | done | mac, Pock, MTMR music | tap = play/pause, swipe = next/prev, long-press = player picker | MPRIS | Art is PNG/JPEG, local or fetched. Scrub bar with `scrub = true`. |
| **Workspaces**: Hyprland first, then sway, GNOME, KDE | P1 | done | Pock (Dock), BTT | tap = switch, swipe = next/prev | compositor backend | Hyprland, sway, niri, KDE (KWin script) and GNOME (extension), all with switching. |
| **Toggle** with state: mic, DND, caffeine (idle inhibit), dark mode, Wi-Fi/BT | P1 | done | MTMR dnd/darkMode, mac | tap; state shown by icon and color | per-toggle backend | DND on dunst, mako, swaync and KDE; `dark-mode` via portal + gsettings/Plasma. mako/swaync/KDE/dark-mode are tested only against mocks. |
| Privacy indicator: mic / camera / screen share in use | P2 | done | waybar privacy | none by default | PipeWire native protocol | `privacy`: one icon per kind in use, hidden when idle; `{app}` names them. Event-driven, closed while the bar is off. V4L2 cameras found via `/dev/video*`. |
| Install diagnosis (`sliverctl doctor`) | P2 | done | (none) | none | systemctl, D-Bus names, files | Read-only checks of daemon, agent, config, desktop helper, optional sources and bar hardware, with a fix per line; `--json`, exit 1 on failure. |
| Screenshot / lock / suspend / power menu | P1 | done | mac control strip | tap; power uses a confirm sub-bar | logind, portal | Built-in `screenshot` (portal), `lock` and `power` (tap: lock/suspend; hold: reboot/off). Script variant: `examples/config/power-menu.toml`. |
| **Script widget**: command output → text/icon/color, refresh interval or on-signal | P1 | done | MTMR shellScriptTitledButton, BTT | any | agent runs it sandboxed with a timeout | `custom/*`: exec, interval, continuous, signal, JSON. Pictures via the `image` module or a published `image` item. |
| **App controls placeholder**: where focused-app items go | P1 | done | mac app region | from the app | public API (api.md) |  |
| Group / folder: opens a sub-bar with an auto close button | P1 | done | MTMR group, BTT folders | tap opens, Esc or close exits |  | Inline drawers, or `drawer = { mode = "bar" }` for a sub-bar with an automatic ✕. |
| Keyboard layout indicator and switcher | P2 | done | MTMR inputsource | tap = next, long-press = list | compositor | Hold-to-pick on sway, niri and Hyprland; KDE/GNOME tap only. With fcitx5 it shows and toggles the input method (polled every 2 s while on). |
| Notifications: count plus latest, tap to open | P2 | done | (none, our own) | tap, swipe = dismiss | freedesktop notifications | Shows the latest summary, never the body. Tap invokes the default action (`makoctl`), else dismisses. |
| Timer / pomodoro | P2 | done | MTMR pomodoro | tap start/pause, long-press reset |  |  |
| System stats: CPU/RAM/temp/net, sparkline | P2 | done | MTMR cpu/network | tap → system monitor | /proc, hwmon | cpu, memory, load, temperature, disk, network throughput, `{spark}` text sparklines; drawn graphs via the `graph` module. |
| Emoji picker: scrolling strip, recents, category tabs | P2 | done | mac | tap = type, swipe = scroll | wtype or the clipboard | Tabs, paging, recents, skin tones, name search with an on-bar letter row (`sliver-emoji`). |
| App switcher / dock: running and pinned apps | P2 | done | Pock, MTMR dock | tap = focus, long-press = close | compositor plus icon lookup | `taskbar`: windows of the active workspace plus `pinned` launchers. Hyprland, sway, niri, generic Wayland, KDE (no close) and GNOME. |
| Scrubber / candidate strip for apps (text suggestions, tabs) | P2 | done | mac (NSScrubber) | swipe, tap | public API | `scrubber` item (api.md §3): paged candidates, tap picks, `selected`/`scrolled` events. `sliverctl publish scrubber:ID:a\|b\|c`. |
| Audio spectrum | P3 | done | waybar cava | any, user-bound | the `cava` program | `cava`: drawn bars, waybar's keys. cava runs only while the module is shown on a lit bar and sleeps on silence. |
| Weather, currency, calendar "up next" | P3 | done | MTMR |  | script widget | Presets only: `examples/config/{weather,currency,calendar}.toml` on `custom/*` scripts. Offline they keep the last reading (`stale`). |

## 3. Actions

| Action | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Key press, key combo, hold-to-repeat | P0 | done | dfr |  |
| Switch layer: momentary (while held), toggle, one-shot (back after the next press) | P0/P1 | done | dfr Fn | P0 has only Fn. |
| Shell command (detached, no shell by default, optional `sh -c`) | P1 | done | MTMR, BTT | Runs in the user session (agent), never as root. |
| Open app (desktop ID) / open URL or file (xdg-open, portal) | P1 | done | BTT | `{ open = "id" }` (gtk-launch) and `{ url = "…" }` (xdg-open). |
| Toggle with state (bound to a toggle widget) | P1 | done | MTMR | Mute, mic mute, caffeine, Wi-Fi, Bluetooth, DND, dark mode. |
| D-Bus method call | P2 | done | BTT | `{ dbus = ... }` with typed `args`. The settings picker offers presets (logind, MPRIS, dunst, GNOME/KDE lock). |
| Switch layout or profile | P2 | done | BTT | `{ layout }`, `{ profile }`, the API, sliverctl and the settings app. |
| Macro: a sequence of actions with delays | P2 | done | BTT | Max length and total time are capped. |
| Open group / close group | P1 | done | MTMR |  |
| Send to app: an app's own published item callback | P1 | done | mac | Published items deliver interactions to their owner (api.md). |

## 4. Gestures

The digitizer reports reliable multi-touch (3 contacts tested), ~60 Hz, 100 units/mm. It has no
pressure, no hover, and the bar is only 6 mm tall, so vertical swipes are meaningless. Never design
for those.

| Gesture | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Tap: fires on release, cancels on slide-off | P0 | done | dfr | Plain keys fire on press for latency (as dfr does for everything); other items fire on release. |
| Hold: key repeat for key buttons | P0 | done | (none) |  |
| Long-press (≈500 ms, configurable) | P1 | done | MTMR longTap | Doesn't delay tap. Tap fires on release before the threshold. |
| Press-and-slide on a collapsed slider | P1 | done | mac | Opens on press; the same touch slides it relative to the current value. Closes 5 s after the last touch. Not yet tried on the real bar. |
| Swipe left/right on an item | P1 | done | Pock, MTMR | Item scope. Paged widgets (workspaces, media). |
| Double-tap | P2 | done | BTT | Off by default. When set, it delays that item's single tap. |
| Two-finger swipe across the whole bar (→ volume/brightness/custom) | P2 | done | MTMR swipe | `[gestures] two-finger-swipe-left/right`. Only over agent modules, not plain keys (they press on touch-down). Not yet tried on the real bar. |
| Edge swipe from Esc → reveal a hidden layer | P3 | done | (none) | `[gestures] edge-swipe-from-left/right` with any action. Starts within 48 px of a bar end, over an agent module. |

## 5. Layers, layouts, contexts

| Feature | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Two layers: Base and Fn, plus `MediaLayerDefault` | P0 | done | dfr |  |
| Custom layers (Media, Emoji, "Dev"…) entered by an action | P1 | done | BTT groups |  |
| Zones: **Esc** (locked only when there's no physical Esc) · **Main** · **Control strip** (collapsible) | P1 | done | mac | ux.md §2. |
| Per-app layout (app_id or class) | P1 | done | mac, BTT | Hyprland, sway and niri; KDE/GNOME through their integration scripts. |
| Window title regex, per workspace, per monitor | P2 | done | BTT |  |
| State contexts: media playing, screen locked, on battery, low battery, fullscreen, DND, external display, time of day | P2 | done | BTT conditional groups | Built-in `locked` layout; the agent hides app items, script output, notification/media text and titles while locked. Unknown screen lockers aren't detected (integration.md §3). |
| Inheritance: a layout extends a parent and overrides zones | P2 | done | (none) | ux.md §2 Inheritance. |
| Rule priority: first match in user order (settings inserts by specificity) | P1 | done | (none) | Evaluated in the agent (api.md). |
| App-published items and contexts (NSTouchBar-like), via App controls + Status items placeholders | P1 (API v1) / P2 (modal) | done | mac | API v1: all item types, modal contexts with ✕, per-app/provider allow/deny (ux.md §6, api.md §2). |

## 6. Appearance

| Feature | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Outlines on/off, font (fontconfig pattern), icons (SVG/PNG, XDG theme) | P0 | done | dfr |  |
| Pixel shift (±11 px x, ±2 px y) | P0 | done | dfr | Advanced, off by default. Moves the picture 1 px per 15 s along tiny-dfr's path (60 s rest at the ends), never while dark; touches are mapped back. |
| Themes: built-in light/dark/high-contrast/"dfr classic", plus user TOML themes | P1 | done | BTT | Built in: `sliver` (dark, default), `light`, `high-contrast`, `classic`; user CSS stylesheets go on top. |
| Accent color from the desktop (GNOME/KDE accent, else a fixed default) | P1 | done | (none) | Portal `accent-color` (live) feeds `@accent`; KDE `kdeglobals` `AccentColor` is the fallback. Tested only against a mock portal. |
| Per-item colors (background, foreground, state colors) | P1 | done | MTMR, BTT | Stylesheets and `class`/state selectors. |
| Emoji and symbolic icons (recolored to the foreground) | P1 | done | mac | Colour emoji (COLRv1 and bitmap) and XDG symbolic icons. |
| Press feedback: highlight and quick fade (the bar has no haptics) | P1 | done | mac | sliverd swaps in a pressed overlay and fades it on release. |
| Animations: layer switch slide/fade, slider expand. Off when "reduce motion" is set | P2 | done | mac | CSS `transition` and `-sliver-layer-transition`; reduce-motion read from GNOME/KDE turns them off. Run only while something changes. Not yet seen on the real bar. |
| Brightness: adaptive (follows the main display) or fixed, a curve editor, idle dim → off timeouts, an on-battery profile | P0 (dfr keys) / P2 (curve, profiles) | done | dfr | dfr keys, `[display] curve` (replaces √) and `[display.on-battery]` go to sliverd as `display_policy`, resent on reload and AC ↔ battery. |

## 7. Portability

| Feature | Tier | Status | Notes |
|---|---|---|---|
| tiny-dfr import | P0 | done | Also offered in the settings onboarding. |
| Profiles: switchable named full configs | P2 | done | `profiles/<name>.toml`, active one persisted; switch from the bar, sliverctl, D-Bus or settings (customization.md §1b). |
| Export/import a preset (`.sliver` bundle = TOML + icons) | P2 | done | `sliverctl preset export\|show\|import` and the settings app. Import lists every command and needs review (ux.md §9). |
| Shareable gallery | P3 | todo | Needs an external service. Not started. |
| Config readable and editable by hand. The settings app preserves comments (toml_edit) | P1 | done | Settings and hand edits coexist. |

## 8. Platform and integration

From GOAL.md; details in api.md and integration.md.

| Feature | Tier | Status | Notes |
|---|---|---|---|
| sliverd (system) + sliver-agent (user) under systemd, privilege separated | P0 (daemon) / P1 (agent) | done | sliverd is a DynamicUser system unit with a cgroup-checked hardware gate; the agent is a user unit. No root after start. |
| Session D-Bus API v1: publish items/contexts, subscribe to presses, live updates | P1 | done | docs/api.md, introspection XML, drift test. |
| `sliverctl` CLI: `show`, `set`, `push`, `layer`, `reload`, `status`, `render` | P1 | done |  |
| Rust client crate, plus Python and shell examples | P1/P2 | done | `sliver-client`, Python and shell examples. |
| Hyprland backend | P1 | done |  |
| sway/wlroots, GNOME extension, KWin script | P2 | partial | sway, niri, KWin, GNOME and generic Wayland (labwc etc.) done. Missing: river workspaces (private `river-status`), river 0.4, wayfire (untested). |
| MPRIS, PipeWire, logind backlight, UPower, notifications | P1/P2 | done | Plus Bluetooth, network and the tray. |
| RPM + COPR, udev, polkit, SELinux-clean, man pages | P1 (RPM) / P2 (rest) | partial | RPM with man pages, udev, SELinux-clean, no polkit needed; install/upgrade/uninstall tested in a container. Missing: COPR publishing. |
| Intel T2 (t2linux) | P2 | partial | Supported and covered headless (docs/t2.md), but never run on a real T2 Mac. Needs a tester (`docs/t2-testing.md`). |

## 9. Hardware facts that shape features (docs/hardware.md)

- **Size:** 2008×60 px over 230×6 mm, so ~8.7 px/mm. dfr F-keys are ~152 px (17 mm) with 16 px
  gaps. **Minimum target is 80 px (~9 mm); 60 px is the hard floor.** The editor warns below 80
  and warns more strongly below 60, but never refuses. At 80 px, at most ~22 items fit, which
  matches dfr's "breaks around 24".
- **Touch:** multitouch slots and contact size/orientation are reported; 3 simultaneous contacts
  are reliable (more is untested). 100 units/mm, ~60 Hz reports. No pressure, no hover, and
  vertical motion is meaningless.
- **Backlight:** raw 0–255. dfr uses 1 = dim and 0 = off. DRM ACTIVE=0 doesn't blank the panel,
  so "off" means backlight 0. While it's 0, touches are ignored and the first touch wakes the bar.
- **Updates:** memcpy + DIRTYFB with per-button damage rects, with no vsync or page flip. A
  full-frame DIRTYFB takes ~34 ms, so render damage per item, keep animations short (~30 fps max
  for full-bar ones), and skip them when idle.
- **OLED:** burn-in from static items is real. Rely on idle dim/off. Pixel shift is advanced
  and opt-in.
- **Physical Esc:** present on the Mac14,7. Esc on the bar is optional there, and the F-keys are
  the only bar-only keys.
