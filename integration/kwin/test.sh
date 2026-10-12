#!/usr/bin/env bash
# Run the KWin script in a nested, headless kwin_wayland on a private session bus and check that
# it tells a (mock) agent about the focused window. Never touches the live desktop.
# Needs: kwin_wayland (Plasma 6), dbus-run-session, python3-gobject, kitty (any Wayland client).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
exec dbus-run-session -- bash "$here/test-inner.sh" "$here"
