# Browser integration (Firefox, Chromium)

The focused browser window gets Touch Bar controls, the way Safari does on macOS:

    ◀  ▶  ⟳  [ tab │ tab │ tab ›]  +

back, forward and reload buttons, the window's tabs as a scrubber (swipe to scroll, tap to
switch), and a new-tab button. It's an ordinary client of Sliver's public D-Bus API
(`docs/api.md`): an `app` context with `include-children`, so it shows while the browser is
focused. Users can restyle and rearrange it like any module (`app/io.github.wawow830.Sliver.Browser#tabs`,
`#back`, …), and hide it in the settings app's Apps page.

| part | what it is |
|---|---|
| `extension/` | a WebExtension (Manifest V3; `manifest.firefox.json`, `manifest.chromium.json`, one `background.js`) that reports the focused window's tabs and runs the commands |
| `host/sliver-browser-host` | the native-messaging host (Python + Gio). The browser starts it; it publishes on the Touch Bar and sends taps back |
| `native/` | native-messaging manifests (Firefox: by extension id; Chromium: by extension origin) |
| `install.sh` | per-user install from a checkout (registers the host, builds loadable extension folders) |

Tabs are coalesced (one message per 50 ms burst) and the host only calls `SetItems` when the
extension reports a change, so an idle browser costs nothing (invariant 3). The host exits when
the browser closes the port, and its context goes with its D-Bus connection.

Tested headless in `crates/sliver-client/tests/examples.rs` (`browser_native_host`): the host is
fed native-messaging frames on stdin against the real API on a private bus. `scripts/check`
syntax-checks the extension. **Not yet run in a real browser** (that needs a browser session on the
human's desktop). Flatpak browsers need the host inside the sandbox's reach (not handled). Not yet done: a signed AMO/Chrome Web Store build and RPM
packaging of the host and manifests (system paths: `/usr/lib64/mozilla/native-messaging-hosts`,
`/etc/chromium/native-messaging-hosts`).
