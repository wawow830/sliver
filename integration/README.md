# Desktop integrations

Compositors that Sliver's agent cannot query itself push focus to it instead, over
`io.github.wawow830.Sliver.Desktop1` (`SetFocus`, `SetWorkspaces`; docs/api.md §5).
Hyprland, sway and niri need none of this: the agent reads their IPC (`crates/sliver-desktop`).

| Desktop | What | Install path (packaging) | Tested |
|---|---|---|---|
| KDE Plasma 6 | KWin script `kwin/sliver-focus` | `/usr/share/kwin/scripts/sliver-focus/` | yes: nested `kwin_wayland --virtual`, `kwin/test.sh` |
| GNOME Shell 45+ | extension `gnome-shell/sliver-focus@sliver.wawow830.github.io` | `/usr/share/gnome-shell/extensions/<uuid>/` | syntax only (`node --check`); no GNOME Shell on the dev machine |

Apps can integrate too: `browser/` is a Firefox/Chromium extension plus a native-messaging host
that puts the focused window's tabs and back/forward/reload on the bar (see `browser/README.md`).

Both desktop integrations enable per user (KDE: System Settings → Window Management → KWin Scripts; GNOME:
`gnome-extensions enable sliver-focus@sliver.wawow830.github.io`, then log out and in on Wayland).
Packages must not enable them by default.

## KWin script: what the test shows

`kwin/test.sh` starts a private session bus, a mock Desktop1 service (`kwin/mock-desktop1.py`) and a
headless `kwin_wayland --virtual`, loads the script, opens a `kitty` window and checks that
`SetFocus` arrives with `app-id`, `title`, `pid`, `window` (`kwin:{uuid}`), `workspace`, `monitor`,
`fullscreen`, `floating`. It never touches the live desktop and stops every process it started.

Wire types as KWin's `callDBus` produces them (mock log): strings `s`, booleans `b`, **integers `i`**
(not `u`: the service must accept any integer type for `pid`, and it does), and **arrays arrive as
`av`, never `aa{sv}`**. So the script reports desktops with `SetWorkspacesV(av)` (elements are `a{sv}`
variants); GNOME's GJS can send `aa{sv}` to `SetWorkspaces` natively.

## GNOME extension: untested on a real shell

Written from the GNOME Shell 45+ extension API (ESM). Uses `NO_AUTO_START` so it never starts the
agent, ignores every error, and sends an empty `SetFocus` on disable so the agent can fall back.
Please run it on a real GNOME session before relying on it.

## Switching workspaces (the agent calling back)

* KDE: the agent calls KWin's own `org.kde.KWin /KWin setCurrentDesktop(i)` (1-based, the same
  number the script reports as the workspace id). `integration/kwin/test.sh` checks it in the
  nested KWin.
* GNOME: the extension owns `io.github.wawow830.Sliver.Shell` while enabled and exports
  `io.github.wawow830.Sliver.Shell1.FocusWorkspace(i)` (1-based). Not yet run on a real shell.

Both calls use `NoAutoStart`: with no script or extension running, tapping does nothing but log.

## The window list (taskbar)

Both the KWin script and the GNOME extension push **every** window with `Desktop1.SetWindows`
(`SetWindowsV` on KDE): `window`, `app-id`, `title`, `pid`, `workspace` (the `SetWorkspaces` id),
`focused`, `urgent`. KWin's script sends it only when the list changed; the extension coalesces
signal bursts over 50 ms. The agent turns focus + workspaces + windows into its desktop state, so
the `taskbar` module and pinned apps work there.

The agent's taskbar actions go back like this:

* KDE focus: KWin's window runner, `org.kde.KWin /WindowsRunner org.kde.krunner1 Run("0_{internalId}", "")`
  (what KRunner uses). Verified in the nested KWin test. **KWin has no close action**, so
  long-pressing a taskbar button on KDE only logs.
* GNOME focus and close: `Sliver.Shell1.FocusWindow(s)` / `CloseWindow(s)` on the extension, by the
  window's `mutter:<stable sequence>` id. Not yet run on a real shell.
