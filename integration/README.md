# Desktop integrations

Compositors that Sliver's agent cannot query itself push focus to it instead, over
`io.github.wawow830.Sliver.Desktop1` (`SetFocus`, `SetWorkspaces`; docs/api.md §5).
Hyprland, sway and niri need none of this: the agent reads their IPC (`crates/sliver-desktop`).

| Desktop | What | Package: install path | Tested |
|---|---|---|---|
| KDE Plasma 6 | KWin script `kwin/sliver-focus` | `sliver-kwin`: `/usr/share/kwin/scripts/sliver-focus/` (KWin 6.7 also reads it, checked nested; 6.0–6.3 only read it) | yes: nested `kwin_wayland --virtual`, `kwin/test.sh` |
| GNOME Shell 45+ | extension `gnome-shell/sliver-focus@sliver.wawow830.github.io` | `gnome-shell-extension-sliver`: `/usr/share/gnome-shell/extensions/<uuid>/` | yes: real headless gnome-shell 50.5, `gnome-shell/test.sh` |

Apps can integrate too: `browser/` is a Firefox/Chromium extension plus a native-messaging host
that puts the focused window's tabs and back/forward/reload on the bar (see `browser/README.md`).

Both desktop integrations enable per user (KDE: System Settings → Window Management → KWin Scripts; GNOME:
`gnome-extensions enable sliver-focus@sliver.wawow830.github.io`, then log out and in on Wayland).
Packages must not enable them by default: the RPM subpackages are pulled in next to the desktop
(`Supplements:`), but each user still turns them on.

## KWin script: what the test shows

`kwin/test.sh` starts a private session bus, a mock Desktop1 service (`kwin/mock-desktop1.py`) and a
headless `kwin_wayland --virtual`, loads the script, opens a `kitty` window and checks that
`SetFocus` arrives with `app-id`, `title`, `pid`, `window` (`kwin:{uuid}`), `workspace`, `monitor`,
`fullscreen`, `floating`. It never touches the live desktop and stops every process it started.

Wire types as KWin's `callDBus` produces them (mock log): strings `s`, booleans `b`, **integers `i`**
(not `u`: the service must accept any integer type for `pid`, and it does), and **arrays arrive as
`av`, never `aa{sv}`**. So the script reports desktops with `SetWorkspacesV(av)` (elements are `a{sv}`
variants); GNOME's GJS can send `aa{sv}` to `SetWorkspaces` natively.

## GNOME extension: verified on a real (headless) gnome-shell 50.5

Written from the GNOME Shell 45+ extension API (ESM). Uses `NO_AUTO_START` so it never starts the
agent, ignores every error, and sends an empty `SetFocus` on disable so the agent can fall back.
`integration/gnome-shell/test.sh` runs it on a real gnome-shell/mutter 50.5 (`--headless
--virtual-monitor`, private session bus) with two real clients: focus, workspaces and the window list
reach a mock agent, and `Shell1.FocusWindow`, `FocusWorkspace` and `CloseWindow` really act. The
GNOME version it ran against is the only one tried: 45–49 are listed in `metadata.json` but
untested. No root needed: `dnf download --resolve` the rpms, `rpm2cpio | cpio -idm` them into a
prefix, and the script puts them over `/usr/share` and `/usr/lib64` with an overlay in a private
user namespace (see the header of the script).

## Switching workspaces (the agent calling back)

* KDE: the agent calls KWin's own `org.kde.KWin /KWin setCurrentDesktop(i)` (1-based, the same
  number the script reports as the workspace id). `integration/kwin/test.sh` checks it in the
  nested KWin.
* GNOME: the extension owns `io.github.wawow830.Sliver.Shell` while enabled and exports
  `io.github.wawow830.Sliver.Shell1.FocusWorkspace(i)` (1-based). Verified by `integration/gnome-shell/test.sh`.

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
  window's `mutter:<stable sequence>` id. Verified by `integration/gnome-shell/test.sh`.

## Live compositor checks

`scripts/live-compositors [sway|labwc|niri|gnome-shell ...]` fetches each compositor's rpms into
`target/test-compositors/` without root (`dnf download --resolve` + `rpm2cpio`) and runs
`integration/<name>/test.sh` against it, headless, on private runtime dirs and buses. About 400 MB
the first time; not part of `scripts/check`. Covered this way: the sway backend (sway 1.11), the
generic Wayland backend (labwc 0.9), niri 26.04 (nested in a headless sway) and the GNOME
extension (gnome-shell 50.5). KWin has its own nested test (`integration/kwin/test.sh`).

## Portal check

`integration/portal/test.sh` runs the real xdg-desktop-portal on a private bus with a mock backend
(`mock-backend.py`) and makes the `screenshot` module's call through the agent's own code
(`sliver-agent/examples/oneshot.rs`). It needs only the installed portal and python3-gobject, so it
needs no compositor. Skips when they're missing. A real backend (xdpw in headless sway) accepted the
call too but never answered, so grabbing pixels is unverified.
