# Feature inventory

Owner: elm (T6). This turns GOAL.md's "complete" into a prioritized, buildable list. The UX for
all of this is in [ux.md](ux.md). API shapes belong to cedar ([api.md](api.md)), hardware facts to
birch ([hardware.md](hardware.md)). Anything marked **(pending birch)** or **(pending cedar)** is an
assumption waiting on their docs.

## Tiers

| Tier | Meaning | Exit criterion |
|---|---|---|
| **P0 Parity** | sliverd can replace tiny-dfr with zero regressions. | A tiny-dfr user swaps and notices nothing, except that crashes now fall back to the safe layer. |
| **P1 MVP** | The first release worth switching for. | The editor, per-app layouts on Hyprland, sliders, now playing, scripts, API v1. |
| **P2 Complete** | Everything GOAL.md lists. | Every GOAL.md bullet is checked off. |
| **P3 Polish** | Beyond the goal, or community-driven. | Optional. |

Prior art key: **mac** = macOS Touch Bar, **BTT** = BetterTouchTool, **MTMR** = My TouchBar My
Rules, **Pock**, **dfr** = tiny-dfr 0.3.7 (the config on this machine is
`/usr/share/tiny-dfr/config.toml`).

## 1. Safety and core (P0, non-negotiable)

| Feature | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Built-in **safe layer** inside sliverd: F1–F12 (with Esc first on bars that have no physical Esc), plus media on Fn | P0 | partial | dfr (it's the only layer) | Shown whenever the agent, config or a widget fails. It needs no config and no fonts beyond the built-in ones. **Now:** Fallback and layout work and are tested headless. Real-bar runs showed garbled labels (stride fix landed, re-check pending). |
| Esc and F-keys always reachable via sliverd's **safe hatch** (double-press Fn and hold → built-in F-keys; H14), even though every layout including Fn is fully editable (H4 revised). Defaults put Esc first on 2170 px bars | P0 | partial | mac, dfr | dfr adds Esc only on bars ≥2170 px. This 2008 px Mac14,7 has a physical Esc (confirmed in H2). See ux.md §7. **Now:** The hatch logic lives in sliverd with tests; not yet confirmed on the real bar. |
| Fn held → the user's Fn layer (fully editable). Fn-Fn-hold → built-in F-keys, from any state | P0 | partial | mac, dfr | The hatch works even with app modals, custom layers, and a config with no F-keys. **Now:** Fn layer and hatch work in the core; the real Fn key path is untested on hardware. |
| Per-app "show F-keys by default" | P1 | done | mac (Keyboard prefs) | For terminals, IDEs and games. Expressed as a rule (see §5). **Now:** A rule pointing at the built-in `fkeys` layout. |
| Render only on change, idle dim → off | P0 | partial | dfr | Invariant 3. The bar is OLED, so this is also the main burn-in protection. Defaults: dim at 30 s, off at 60 s, like dfr. **Now:** Idle dim/off in sliverd, render on change plus animation deadlines in the agent; real power draw is unmeasured. |
| Wake on touch: the first touch on a dark bar only wakes it and doesn't fire an item | P0 | partial | dfr | Avoids accidental presses on a bar you can't see. **Now:** Implemented and tested headless; unverified on hardware. |
| Headless renderer to PNG and simulated touch | P0 | done | (none) | Invariant 4. The settings preview uses the same renderer (ux.md). |
| tiny-dfr config import (`/usr/share` + `/etc` merge) | P0 | done | dfr | Covers every dfr key: `MediaLayerDefault`, `ShowButtonOutlines`, `EnablePixelShift`, `FontTemplate`, `AdaptiveBrightness`, `ActiveBrightness`, `PrimaryLayerKeys`, `MediaLayerKeys` (Text/Icon/Theme/Time/Locale/Battery/Stretch/Action, key combos, spacers). |
| Coexist with or replace tiny-dfr cleanly | P0 | partial | (none) | Packaging (systemd `Conflicts=`), restored by the safe-swap harness. **Now:** sliverd units, udev rules and the tiny-dfr fallback ladder exist (ADR-0005); the package is not installable yet. |

## 2. Items and widgets

"Item" means anything that sits on the bar. A widget is an item with live content.

| Widget | Tier | Status | Prior art | Gestures | Needs | Notes |
|---|---|---|---|---|---|---|
| Key button: text and/or icon, single key or combo | P0 | partial | dfr, MTMR staticButton | tap, hold = key repeat | uinput (daemon) | Done headless; the uinput key path is untested on the real bar. |
| Spacer: fixed or flexible | P0 | done | dfr `{Stretch}`, MTMR | none |  |  |
| Clock: 12h/24h/strftime, locale | P0 | done | dfr Time, MTMR timeButton | tap → action (e.g. calendar) |  |  |
| Battery: percent/icon/both, charging and low colors | P0 | done | dfr Battery, MTMR | tap → power settings | UPower in the agent; sysfs fallback in the daemon | UPower in the agent. |
| Media keys: brightness ±, kbd backlight ±, volume ±, mute, mic mute, prev/play/next, search | P0 | partial | dfr media layer | tap, hold-repeat | uinput keycodes | Done headless; uinput path untested on the real bar. |
| **Brightness / volume / kbd-backlight slider** | P1 | done | mac control strip, BTT | press-and-slide, drag, tap to expand | logind backlight, PipeWire/wpctl | Backlight (logind), audio (PipeWire) and keyboard-backlight sliders. |
| **Now playing (MPRIS)**: title/artist/art, prev/play/next, scrub | P1 | done | mac, Pock, MTMR music | tap = play/pause, swipe = next/prev, long-press = player picker | MPRIS | Title, art (PNG/JPEG, local or fetched), play/pause, next/previous, scrub bar (`scrub = true`), player switch on hold. |
| **Workspaces**: Hyprland first, then sway, GNOME, KDE | P1 | partial | Pock (Dock), BTT | tap = switch, swipe = next/prev | compositor backend (T7) | Hyprland, sway and niri done (with focus actions). GNOME and KDE workspace lists are not wired. |
| **Toggle** with state: mic, DND, caffeine (idle inhibit), dark mode, Wi-Fi/BT | P1 | partial | MTMR dnd/darkMode, mac | tap; state shown by icon and color | per-toggle backend | Mute, mic mute, caffeine, Wi-Fi, Bluetooth and DND (dunst only) done. Missing: dark mode, DND on KDE/mako/swaync. |
| Screenshot / lock / suspend / power menu | P1 | done | mac control strip | tap; power uses a confirm sub-bar | logind, portal | The `menu` sub-bar is implemented; `examples/config/power-menu.toml` is a ready preset (power menu, screenshot key). Not built in, because the validator warns about unplaced modules. |
| **Script widget**: command output → text/icon/color, refresh interval or on-signal | P1 | done | MTMR shellScriptTitledButton, BTT | any | agent runs it sandboxed with a timeout | `custom/*` runtime: exec, interval, continuous, signal, JSON. |
| **App controls placeholder**: where focused-app items go | P1 | done | mac app region | from the app | public API (cedar) |  |
| Group / folder: opens a sub-bar with an auto close button | P1 | partial | MTMR group, BTT folders | tap opens, Esc or close exits |  | Inline drawers and groups done; the sub-bar mode with an automatic close button is not verified. |
| Keyboard layout indicator and switcher | P2 | partial | MTMR inputsource | tap = next, long-press = list | compositor | The indicator and tap-to-cycle (Hyprland, sway, niri) are done; the long-press layout list is not. |
| Notifications: count plus latest, tap to open | P2 | done | (none, our own) | tap, swipe = dismiss | freedesktop notifications (needs a monitor; check per DE) | Count, latest summary (never the body), tap invokes the default action (`makoctl` where installed, else dismisses), swipe dismisses on any server. The DND switch itself is dunst-only (see Toggles). |
| Timer / pomodoro | P2 | done | MTMR pomodoro | tap start/pause, long-press reset |  |  |
| System stats: CPU/RAM/temp/net, sparkline | P2 | partial | MTMR cpu/network | tap → system monitor | /proc, hwmon | cpu, memory, load, temperature, disk and network done. Missing: sparkline graph and rates. |
| Emoji picker: scrolling strip, recents, category tabs | P2 | partial | mac | tap = type, swipe = scroll | uinput or text input (keymap issue: see T7) | Rendering is ready (COLRv1); the module and the insertion method are open (T51). **Now:** Done: tabs, paging, recents, typing via wtype or the clipboard (`sliver-emoji`). Missing: search and skin-tone selection. |
| App switcher / dock: running and pinned apps | P2 | done | Pock, MTMR dock | tap = focus, long-press = close | compositor plus icon lookup | The `taskbar` module: one icon per window of the active workspace, `pinned = [...]` launchers first (idle ones start the app). |
| Scrubber / candidate strip for apps (text suggestions, tabs) | P2 | todo | mac (NSScrubber) | swipe, tap | public API |  |
| Weather, currency, calendar "up next" | P3 | todo | MTMR |  | Only as presets built on the script widget. Not core. | Possible today as `custom/*` scripts. |

## 3. Actions

| Action | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Key press, key combo, hold-to-repeat | P0 | done | dfr |  |
| Switch layer: momentary (while held), toggle, one-shot (back after the next press) | P0/P1 | done | dfr Fn | P0 has only Fn. |
| Shell command (detached, no shell by default, optional `sh -c`) | P1 | done | MTMR, BTT | Runs in the user session (agent), never as root. |
| Open app (desktop ID) / open URL or file (xdg-open, portal) | P1 | done | BTT | `{ open = "id" }` (gtk-launch) and `{ url = "…" }` (xdg-open) actions. |
| Toggle with state (bound to a toggle widget) | P1 | partial | MTMR | Mute, mic mute, caffeine, Wi-Fi, Bluetooth, DND (dunst). |
| D-Bus method call | P2 | done | BTT | Includes a picker for common targets (MPRIS, logind, notifications). **Now:** Picker in the settings app is not built. |
| Switch layout or profile | P2 | partial | BTT | Layout switching done (config, API, sliverctl); profiles exist in the API but have no editor. |
| Macro: a sequence of actions with delays | P2 | done | BTT | Max length and total time are capped. |
| Open group / close group | P1 | done | MTMR |  |
| Send to app: an app's own published item callback | P1 | done | mac | Via the API (cedar). **Now:** Published items deliver interactions to their owner. |

## 4. Gestures

Hardware limits are **pending birch** (slot count, resolution, any pressure). These are the defaults
we want, if the hardware allows them.

| Gesture | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Tap: fires on release, cancels on slide-off | P0 | done | dfr | dfr fires on press. Decide this per item: keys fire on press for latency, everything else on release. |
| *(no pressure/force, no hover, no vertical swipes)* | n/a |  |  | The hardware reports none of these, and the bar is only 6 mm tall. Never design for them. |
| Hold: key repeat for key buttons | P0 | done | (none) |  |
| Long-press (≈500 ms, configurable) | P1 | done | MTMR longTap | Doesn't delay tap. Tap fires on release before the threshold. |
| Press-and-slide on a collapsed slider | P1 | partial | mac | Touch the volume icon and slide without lifting. **Now:** Collapsed sliders open on tap and then drag; true press-and-slide without lifting is not verified. |
| Swipe left/right on an item | P1 | done | Pock, MTMR | Item scope. Paged widgets (workspaces, media). |
| Double-tap | P2 | done | BTT | Off by default. When it's set, it delays that item's single tap. |
| Two-finger swipe across the whole bar (→ volume/brightness/custom) | P2 | todo | MTMR swipe | MT slots exist (count pending). There's little room for 3+ fingers on a 6 mm strip, so ship two-finger only. |
| Edge swipe from Esc → reveal a hidden layer | P3 | todo | (none) | Experimental. |

## 5. Layers, layouts, contexts

| Feature | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Two layers: Base and Fn, plus `MediaLayerDefault` | P0 | done | dfr |  |
| Custom layers (Media, Emoji, "Dev"…) entered by an action | P1 | done | BTT groups |  |
| Zones: **Esc** (locked only when there's no physical Esc) · **Main** · **Control strip** (collapsible) | P1 | done | mac | ux.md §Model. |
| Per-app layout (app_id or class) | P1 | done | mac, BTT | Hyprland first (T7). **Now:** Hyprland, sway and niri; KDE/GNOME through their integration scripts. |
| Window title regex, per workspace, per monitor | P2 | done | BTT |  |
| State contexts: media playing, screen locked, on battery, low battery, fullscreen, DND, external display, time of day | P2 | partial | BTT conditional groups | Locked screen gets a privacy-safe layout (ux.md). **Now:** All keys exist (media, battery, fullscreen, DND, external display, time). `locked` is advisory: logind's hint goes stale with lockers that never call unlock-session. |
| Inheritance: a layout extends a parent and overrides zones | P2 | done | (none) | ux.md §Inheritance. |
| Rule priority: first match in user order (settings inserts by specificity) | P1 | done | (none) | Evaluated in the agent (api.md). **Now:** Most specific match wins. |
| App-published items and contexts (NSTouchBar-like), via App controls + Status items placeholders | P1 (API v1) / P2 (modal) | partial | mac | Per-app allow/deny (ux.md §6, api.md §2). **Now:** API v1 done; modal contexts are in progress. |

## 6. Appearance

| Feature | Tier | Status | Prior art | Notes |
|---|---|---|---|---|
| Outlines on/off, font (fontconfig pattern), icons (SVG/PNG, XDG theme) | P0 | done | dfr |  |
| Pixel shift (±11 px x, ±2 px y) | P0 | partial | dfr | Default off, like dfr. Exposed as an *advanced* option. The OLED burn-in risk is real, but idle dim/off is the main protection. **Now:** Implemented in sliverd/config; not verified on the real panel. |
| Themes: built-in light/dark/high-contrast/"dfr classic", plus user TOML themes | P1 | partial | BTT | `classic` and `sliver` built in, plus user CSS stylesheets. No light/high-contrast presets yet. |
| Accent color from the desktop (GNOME/KDE accent, else a fixed default) | P1 | done | (none) | The settings portal's `accent-color` (with live changes) feeds `@accent` in the default theme; KDE's `kdeglobals` `AccentColor` is the fallback. Tested against a mock portal; no portal runs on the dev machine, so not yet seen live. |
| Per-item colors (background, foreground, state colors) | P1 | done | MTMR, BTT | Stylesheets and `class`/state selectors. |
| Emoji and symbolic icons (recolored to the foreground) | P1 | done | mac | Colour emoji (COLRv1 and bitmap) and XDG symbolic icons. |
| Press feedback: highlight and quick fade (the bar has no haptics) | P1 | done | mac | Pressed overlay swapped by sliverd, with a fade on release. |
| Animations: layer switch slide/fade, slider expand. Off when "reduce motion" is set | P2 | partial | mac | Animations only run while something is changing (power). **Now:** CSS `transition` on colour and opacity done. Layer slide/fade and slider expand are not; reduce-motion is not read. |
| Brightness: adaptive (follows the main display) or fixed, a curve editor, idle dim → off timeouts, an on-battery profile | P0 (dfr keys) / P2 (curve, profiles) | partial | dfr | The tiny-dfr keys (adaptive, active, dim/off timers) work in config/sliverd. Missing: curve editor and on-battery profile. |

## 7. Portability

| Feature | Tier | Status | Notes |
|---|---|---|---|
| tiny-dfr import | P0 | done | Also offered in the settings onboarding. |
| Profiles: switchable named full configs | P2 | partial | For example "Work" and "Presenting". **Now:** The API has `SetProfile`; no profile editor or storage UX. |
| Export/import a preset (`.sliver` bundle = TOML + icons) | P2 | todo | On import, list the scripts and commands and require a review (ux.md §Presets). **Now:** A waybar config importer exists (`sliverctl config import-waybar`); `.sliver` bundles do not. |
| Shareable gallery | P3 | todo | External, so ask the human first. |
| Config readable and editable by hand. The settings app preserves comments (toml_edit) | P1 | done | Settings and hand edits can coexist. **Now:** The writer preserves comments (toml_edit). |

## 8. Platform and integration

These come from GOAL.md and get detailed in T5 (api.md) and T7 (integration.md). Listed here for
tiering only.

| Feature | Tier | Status | Notes |
|---|---|---|---|
| sliverd (system) + sliver-agent (user) under systemd, privilege separated | P0 (daemon) / P1 (agent) | partial | Both have systemd units and packaging files; the package has not been built and installed end to end. |
| Session D-Bus API v1: publish items/contexts, subscribe to presses, live updates | P1 | done | docs/api.md, introspection XML, drift test. |
| `sliverctl` CLI: `show`, `set`, `push`, `layer`, `reload`, `status`, `render --png` | P1 | done |  |
| Rust client crate, plus Python and shell examples | P1/P2 | done | `sliver-client`, Python and shell examples. |
| Hyprland backend | P1 | done |  |
| sway/wlroots, GNOME extension, KWin script | P2 | partial | sway, niri and the KWin script (tested nested) done; the GNOME extension is untested on a real shell; no generic wlroots backend. |
| MPRIS, PipeWire, logind backlight, UPower, notifications | P1/P2 | done | Plus Bluetooth, network and the tray. |
| RPM + COPR, udev, polkit, SELinux-clean, man pages | P1 (RPM) / P2 (rest) | partial | An RPM spec exists (T41); COPR, polkit, SELinux checks and man pages are open. |
| Intel T2 (t2linux) | P2 | todo |  |

## 9. Hardware facts that shape features (from birch, docs/hardware.md)

- **Size:** 2008×60 px over 230×6 mm, so ~8.7 px/mm. dfr F-keys are ~152 px (17 mm) with 16 px
  gaps. **Minimum target is 80 px (~9 mm); 60 px is the hard floor.** The editor warns below 80
  and warns more strongly below 60, but never refuses. At 80 px, at most ~22 items fit, which matches dfr's "breaks around 24".
- **Touch:** multitouch slots and contact size/orientation are reported. There's no pressure, no
  hover, and vertical motion is meaningless. The slot count and coordinate resolution are still
  pending (they need a harness run).
- **Backlight:** raw 0–255. dfr uses 1 = dim and 0 = off. While the backlight is 0, touches are
  ignored and the first touch wakes the bar. Whether DRM ACTIVE=0 truly powers the panel off is
  pending.
- **Updates:** memcpy + DIRTYFB with per-button damage rects, with no vsync or page flip. Damage
  is cheap, so the renderer should damage per item. Refresh rate and latency haven't been
  measured. Assume ~60 Hz, keep animations short, and skip them when idle.
- **OLED:** burn-in from static items is real. Rely on idle dim/off. Pixel shift is advanced
  and opt-in.
- **Physical Esc:** present on this machine (confirmed in H2). Esc on the bar is
  optional, and the F-keys are the only bar-only keys.
