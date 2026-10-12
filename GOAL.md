# Sliver's goal

> **Sliver turns the Touch Bar on Linux Macs into a first-class part of the Linux desktop.**
> It is a complete Touch Bar platform and customizer that replaces tiny-dfr. Users get full
> control over what the bar shows and does: layouts, per-app contexts, widgets, gestures
> and appearance. Every app, script and desktop environment gets a standard way to put
> things on the bar. And the user is never left without Escape or the F-keys.

Everything in this repo serves that one paragraph. If a task doesn't move us toward it, don't do it.

## The machine we're building on

- MacBook Pro 13" M2 (`Mac14,7`), Fedora Asahi Remix 44, aarch64, 16k pages, Hyprland.
- Touch Bar = DRM panel `card1-DSI-1`, **60x2008** (rotated), touch = `Apple MTP multi-touch` / `Mac14,7 Touch Bar` input devices.
- Today it's driven by `tiny-dfr` 0.3.7 (`tiny-dfr.service`, udev rule `99-touchbar-tiny-dfr.rules`,
  config `/usr/share/tiny-dfr/config.toml` + `/etc/tiny-dfr/config.toml`).
- **This laptop has no physical Escape or F-key row.** The Touch Bar *is* the Escape key.

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
- **Appearance:** themes, icons (XDG themes, SVG/PNG, emoji, symbolic), fonts, colors, outlines,
  animations, idle dimming/off, brightness curve, pixel shift.
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
