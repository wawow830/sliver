# Sliver's goal

> **Sliver turns the Touch Bar on Linux Macs into a first-class part of the Linux desktop.**
> It is a complete Touch Bar platform and customizer that replaces tiny-dfr. Users get full
> control over what the bar shows and does: layouts, per-app contexts, widgets, gestures
> and appearance. Every app, script and desktop environment gets a standard way to put
> things on the bar. And the user is never left without Escape or the F-keys.

Everything in this repo serves that one paragraph. If a task doesn't move us toward it, don't do it.

## Design principle: as customizable as possible (think waybar)

The human's direction: **sliver should be as customizable as possible. The reference point is
[waybar](https://github.com/Alexays/Waybar).** Ship great defaults so nobody *has* to configure
anything, but never stop someone who wants to. When a choice is between hard-coding something and
making it configurable, make it configurable. **The invariants below are the only things a user
cannot change.**

What that means here (waybar's model, translated to a touch strip):
- **Everything is a module.** Every button, slider and widget, built-in or not, is a module
  instance with its own config block. Built-ins get no special powers that user modules lack.
- **Users write modules without Rust.** Like waybar's `custom/*`: `exec` a script (once, on an
  `interval`, continuously streaming, or on a `signal`) that returns text or JSON (`text`, `icon`,
  `class`, `percentage`, …). The D-Bus API is the same thing for long-running programs.
- **Every module is shaped by config**, not code: `format` strings with placeholders, `format-icons`
  per state or level, `states` thresholds (`warning`/`critical`), per-gesture actions (`on-tap`,
  `on-hold`, `on-swipe-left/right`, `on-drag` for sliders, …), and `max-length`/width.
- **Styling is a stylesheet**, like waybar's `style.css`: target modules by id, class and state
  (`#battery.critical`, `.custom-vpn.connected`, `:active` while pressed). Themes are just
  stylesheets. *How* to get CSS-like styling onto the pure-Rust renderer (ADR-0002) is the swarm's call.
- **Plain-text config is the source of truth.** It's hand-editable and dotfile-friendly, supports
  comments and `include`s, and hot-reloads on save. The settings app edits the same files, so
  nothing is GUI-only and nothing is file-only.
- **Structure is configurable too:** zones, layers, contexts/rules and gesture bindings are
  user-definable, not fixed lists, as far as the invariants allow.
- **Scriptable from outside:** `sliverctl` and D-Bus can trigger, update, show/hide and reload
  anything, and send signals to modules (like `pkill -RTMIN+N waybar`).

## The machine we're building on

- MacBook Pro 13" M2 (`Mac14,7`), Fedora Asahi Remix 44, aarch64, 16k pages, Hyprland.
- Touch Bar = DRM panel `card1-DSI-1`, **60x2008** (rotated), touch = `Apple MTP multi-touch` / `Mac14,7 Touch Bar` input devices.
- Today it's driven by `tiny-dfr` 0.3.7 (`tiny-dfr.service`, udev rule `99-touchbar-tiny-dfr.rules`,
  config `/usr/share/tiny-dfr/config.toml` + `/etc/tiny-dfr/config.toml`).
- This laptop **has a physical Escape key** (confirmed in H2) but **no F-key row**. The F-keys exist
  only on the Touch Bar. 2170 px Touch Bar models have no physical Esc, and there the bar *is* Escape.

Targets: Apple Silicon Macs with a Touch Bar (Asahi) first, Intel T2 Macs (t2linux) second.

## What "complete" means

Sliver is done when all of these are true and polished.

### For users
- **Settings app** (native, GNOME/KDE/Hyprland-friendly) with a WYSIWYG editor: drag and drop
  buttons, resize, reorder, and a live preview both on screen and on the real bar.
- **Layers and contexts:** Fn/media/custom layers. Layouts can switch per app (active window), per
  workspace, per state (media playing, screen locked, on battery…), with sensible inheritance.
- **Widgets:** Esc, F1–F12, brightness/volume/keyboard-backlight sliders, now playing (MPRIS),
  clock, battery, workspaces/app switcher, emoji picker, notifications, timers, system stats,
  mic/DND/caffeine toggles, keyboard layout, screenshot/lock/power, custom text/script output.
- **Actions:** key presses and combos, shell commands, D-Bus calls, open apps/URLs, switch
  layer/layout, toggles with state, macros.
- **Gestures:** tap, long-press, swipe, slider drag, multi-touch where the hardware allows.
- **Appearance:** user stylesheets and themes, icons (XDG themes, SVG/PNG, emoji, symbolic), fonts,
  colors, outlines, animations, idle dimming/off, brightness curve, pixel shift.
- **User modules:** script/exec modules and D-Bus-published modules that are first-class, i.e.
  styleable, gesture-bindable and placeable anywhere, exactly like built-ins.
- **Portability:** import tiny-dfr config, profiles, export/share presets.

### For the rest of Linux ("everything can use it")
- A **system daemon** owns the display and touch input. A **per-user session agent** bridges it
  to the user's session and compositor. Both are privilege-separated and run under systemd.
- A **public, documented D-Bus API** on the session bus. Apps and scripts can publish items and
  contexts (an NSTouchBar-like model where the focused app can provide its own controls),
  subscribe to presses, and update widgets live.
- **`sliverctl`** CLI, a Rust client crate, and examples for Python/shell. Plain D-Bus, so GTK, Qt
  and Electron apps can integrate without our libraries.
- **Desktop integration:** Hyprland, sway/wlroots, GNOME (extension), KDE (KWin script), plus
  MPRIS, PipeWire, backlight via logind, UPower, freedesktop notifications.
- **Packaging:** RPM spec plus COPR, systemd units, udev, polkit, SELinux-clean, clean
  coexistence with or replacement of `tiny-dfr`, docs and man pages.

### Invariants (never trade these away)
1. **Escape and the F-keys are always reachable.** If the agent, a widget, the config or the
   compositor dies, the daemon falls back to a built-in safe layer. If the daemon dies, systemd
   brings back tiny-dfr or sliverd.
2. **Never strand the user.** Hardware experiments go through the safe-swap harness. It must
   automatically restore the previous Touch Bar daemon within a bounded time.
3. **Cheap on power.** Render only on change, no busy loops, dim and sleep when idle.
4. **Testable without hardware.** Headless renderer to PNG snapshots, simulated touch input,
   and CI. Hardware only confirms.

## Non-goals
macOS support. Rewriting the kernel drivers. Anything that needs the user to disable security
features.
