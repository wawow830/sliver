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

| Feature | Tier | Prior art | Notes |
|---|---|---|---|
| Built-in **safe layer** inside sliverd: F1–F12 (with Esc first on bars that have no physical Esc), plus media on Fn | P0 | dfr (it's the only layer) | Shown whenever the agent, config or a widget fails. It needs no config and no fonts beyond the built-in ones. |
| Esc and F-keys always reachable via sliverd's **safe hatch** (double-press Fn and hold → built-in F-keys; H14), even though every layout including Fn is fully editable (H4 revised). Defaults put Esc first on 2170 px bars | P0 | mac, dfr | dfr adds Esc only on bars ≥2170 px. This 2008 px Mac14,7 has a physical Esc (confirmed in H2). See ux.md §7. |
| Fn held → the user's Fn layer (fully editable). Fn-Fn-hold → built-in F-keys, from any state | P0 | mac, dfr | The hatch works even with app modals, custom layers, and a config with no F-keys. |
| Per-app "show F-keys by default" | P1 | mac (Keyboard prefs) | For terminals, IDEs and games. Expressed as a rule (see §5). |
| Render only on change, idle dim → off | P0 | dfr | Invariant 3. The bar is OLED, so this is also the main burn-in protection. Defaults: dim at 30 s, off at 60 s, like dfr. |
| Wake on touch: the first touch on a dark bar only wakes it and doesn't fire an item | P0 | dfr | Avoids accidental presses on a bar you can't see. |
| Headless renderer to PNG and simulated touch | P0 | (none) | Invariant 4. The settings preview uses the same renderer (ux.md). |
| tiny-dfr config import (`/usr/share` + `/etc` merge) | P0 | dfr | Covers every dfr key: `MediaLayerDefault`, `ShowButtonOutlines`, `EnablePixelShift`, `FontTemplate`, `AdaptiveBrightness`, `ActiveBrightness`, `PrimaryLayerKeys`, `MediaLayerKeys` (Text/Icon/Theme/Time/Locale/Battery/Stretch/Action, key combos, spacers). |
| Coexist with or replace tiny-dfr cleanly | P0 | (none) | Packaging (systemd `Conflicts=`), restored by the safe-swap harness. |

## 2. Items and widgets

"Item" means anything that sits on the bar. A widget is an item with live content.

| Widget | Tier | Prior art | Gestures | Needs |
|---|---|---|---|---|
| Key button: text and/or icon, single key or combo | P0 | dfr, MTMR staticButton | tap, hold = key repeat | uinput (daemon) |
| Spacer: fixed or flexible | P0 | dfr `{Stretch}`, MTMR | none | |
| Clock: 12h/24h/strftime, locale | P0 | dfr Time, MTMR timeButton | tap → action (e.g. calendar) | |
| Battery: percent/icon/both, charging and low colors | P0 | dfr Battery, MTMR | tap → power settings | UPower in the agent; sysfs fallback in the daemon |
| Media keys: brightness ±, kbd backlight ±, volume ±, mute, mic mute, prev/play/next, search | P0 | dfr media layer | tap, hold-repeat | uinput keycodes |
| **Brightness / volume / kbd-backlight slider** | P1 | mac control strip, BTT | press-and-slide, drag, tap to expand | logind backlight, PipeWire/wpctl |
| **Now playing (MPRIS)**: title/artist/art, prev/play/next, scrub | P1 | mac, Pock, MTMR music | tap = play/pause, swipe = next/prev, long-press = player picker | MPRIS |
| **Workspaces**: Hyprland first, then sway, GNOME, KDE | P1 | Pock (Dock), BTT | tap = switch, swipe = next/prev | compositor backend (T7) |
| **Toggle** with state: mic, DND, caffeine (idle inhibit), dark mode, Wi-Fi/BT | P1 | MTMR dnd/darkMode, mac | tap; state shown by icon and color | per-toggle backend |
| Screenshot / lock / suspend / power menu | P1 | mac control strip | tap; power uses a confirm sub-bar | logind, portal |
| **Script widget**: command output → text/icon/color, refresh interval or on-signal | P1 | MTMR shellScriptTitledButton, BTT | any | agent runs it sandboxed with a timeout |
| **App controls placeholder**: where focused-app items go | P1 | mac app region | from the app | public API (cedar) |
| Group / folder: opens a sub-bar with an auto close button | P1 | MTMR group, BTT folders | tap opens, Esc or close exits | |
| Keyboard layout indicator and switcher | P2 | MTMR inputsource | tap = next, long-press = list | compositor |
| Notifications: count plus latest, tap to open | P2 | (none, our own) | tap, swipe = dismiss | freedesktop notifications (needs a monitor; check per DE) |
| Timer / pomodoro | P2 | MTMR pomodoro | tap start/pause, long-press reset | |
| System stats: CPU/RAM/temp/net, sparkline | P2 | MTMR cpu/network | tap → system monitor | /proc, hwmon |
| Emoji picker: scrolling strip, recents, category tabs | P2 | mac | tap = type, swipe = scroll | uinput or text input (keymap issue: see T7) |
| App switcher / dock: running and pinned apps | P2 | Pock, MTMR dock | tap = focus, long-press = close | compositor plus icon lookup |
| Scrubber / candidate strip for apps (text suggestions, tabs) | P2 | mac (NSScrubber) | swipe, tap | public API |
| Weather, currency, calendar "up next" | P3 | MTMR | | Only as presets built on the script widget. Not core. |

## 3. Actions

| Action | Tier | Prior art | Notes |
|---|---|---|---|
| Key press, key combo, hold-to-repeat | P0 | dfr | |
| Switch layer: momentary (while held), toggle, one-shot (back after the next press) | P0/P1 | dfr Fn | P0 has only Fn. |
| Shell command (detached, no shell by default, optional `sh -c`) | P1 | MTMR, BTT | Runs in the user session (agent), never as root. |
| Open app (desktop ID) / open URL or file (xdg-open, portal) | P1 | BTT | |
| Toggle with state (bound to a toggle widget) | P1 | MTMR | |
| D-Bus method call | P2 | BTT | Includes a picker for common targets (MPRIS, logind, notifications). |
| Switch layout or profile | P2 | BTT | |
| Macro: a sequence of actions with delays | P2 | BTT | Max length and total time are capped. |
| Open group / close group | P1 | MTMR | |
| Send to app: an app's own published item callback | P1 | mac | Via the API (cedar). |

## 4. Gestures

Hardware limits are **pending birch** (slot count, resolution, any pressure). These are the defaults
we want, if the hardware allows them.

| Gesture | Tier | Prior art | Notes |
|---|---|---|---|
| Tap: fires on release, cancels on slide-off | P0 | dfr | dfr fires on press. Decide this per item: keys fire on press for latency, everything else on release. |
| *(no pressure/force, no hover, no vertical swipes)* | n/a | | The hardware reports none of these, and the bar is only 6 mm tall. Never design for them. |
| Hold: key repeat for key buttons | P0 | (none) | |
| Long-press (≈500 ms, configurable) | P1 | MTMR longTap | Doesn't delay tap. Tap fires on release before the threshold. |
| Press-and-slide on a collapsed slider | P1 | mac | Touch the volume icon and slide without lifting. |
| Swipe left/right on an item | P1 | Pock, MTMR | Item scope. Paged widgets (workspaces, media). |
| Double-tap | P2 | BTT | Off by default. When it's set, it delays that item's single tap. |
| Two-finger swipe across the whole bar (→ volume/brightness/custom) | P2 | MTMR swipe | MT slots exist (count pending). There's little room for 3+ fingers on a 6 mm strip, so ship two-finger only. |
| Edge swipe from Esc → reveal a hidden layer | P3 | (none) | Experimental. |

## 5. Layers, layouts, contexts

| Feature | Tier | Prior art | Notes |
|---|---|---|---|
| Two layers: Base and Fn, plus `MediaLayerDefault` | P0 | dfr | |
| Custom layers (Media, Emoji, "Dev"…) entered by an action | P1 | BTT groups | |
| Zones: **Esc** (locked only when there's no physical Esc) · **Main** · **Control strip** (collapsible) | P1 | mac | ux.md §Model. |
| Per-app layout (app_id or class) | P1 | mac, BTT | Hyprland first (T7). |
| Window title regex, per workspace, per monitor | P2 | BTT | |
| State contexts: media playing, screen locked, on battery, low battery, fullscreen, DND, external display, time of day | P2 | BTT conditional groups | Locked screen gets a privacy-safe layout (ux.md). |
| Inheritance: a layout extends a parent and overrides zones | P2 | (none) | ux.md §Inheritance. |
| Rule priority: first match in user order (settings inserts by specificity) | P1 | (none) | Evaluated in the agent (api.md). |
| App-published items and contexts (NSTouchBar-like), via App controls + Status items placeholders | P1 (API v1) / P2 (modal) | mac | Per-app allow/deny (ux.md §6, api.md §2). |

## 6. Appearance

| Feature | Tier | Prior art | Notes |
|---|---|---|---|
| Outlines on/off, font (fontconfig pattern), icons (SVG/PNG, XDG theme) | P0 | dfr | |
| Pixel shift (±11 px x, ±2 px y) | P0 | dfr | Default off, like dfr. Exposed as an *advanced* option. The OLED burn-in risk is real, but idle dim/off is the main protection. |
| Themes: built-in light/dark/high-contrast/"dfr classic", plus user TOML themes | P1 | BTT | |
| Accent color from the desktop (GNOME/KDE accent, else a fixed default) | P1 | (none) | |
| Per-item colors (background, foreground, state colors) | P1 | MTMR, BTT | |
| Emoji and symbolic icons (recolored to the foreground) | P1 | mac | |
| Press feedback: highlight and quick fade (the bar has no haptics) | P1 | mac | |
| Animations: layer switch slide/fade, slider expand. Off when "reduce motion" is set | P2 | mac | Animations only run while something is changing (power). |
| Brightness: adaptive (follows the main display) or fixed, a curve editor, idle dim → off timeouts, an on-battery profile | P0 (dfr keys) / P2 (curve, profiles) | dfr | |

## 7. Portability

| Feature | Tier | Notes |
|---|---|---|
| tiny-dfr import | P0 | Also offered in the settings onboarding. |
| Profiles: switchable named full configs | P2 | For example "Work" and "Presenting". |
| Export/import a preset (`.sliver` bundle = TOML + icons) | P2 | On import, list the scripts and commands and require a review (ux.md §Presets). |
| Shareable gallery | P3 | External, so ask the human first. |
| Config readable and editable by hand. The settings app preserves comments (toml_edit) | P1 | Settings and hand edits can coexist. |

## 8. Platform and integration

These come from GOAL.md and get detailed in T5 (api.md) and T7 (integration.md). Listed here for
tiering only.

| Feature | Tier |
|---|---|
| sliverd (system) + sliver-agent (user) under systemd, privilege separated | P0 (daemon) / P1 (agent) |
| Session D-Bus API v1: publish items/contexts, subscribe to presses, live updates | P1 |
| `sliverctl` CLI: `show`, `set`, `push`, `layer`, `reload`, `status`, `render --png` | P1 |
| Rust client crate, plus Python and shell examples | P1/P2 |
| Hyprland backend | P1 |
| sway/wlroots, GNOME extension, KWin script | P2 |
| MPRIS, PipeWire, logind backlight, UPower, notifications | P1/P2 |
| RPM + COPR, udev, polkit, SELinux-clean, man pages | P1 (RPM) / P2 (rest) |
| Intel T2 (t2linux) | P2 |

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
