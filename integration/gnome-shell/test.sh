#!/usr/bin/env bash
# The GNOME extension against a real gnome-shell/mutter running headless (--headless
# --virtual-monitor) on a private session bus: focus, workspaces and the window list reach a
# mock Desktop1 agent, and the extension's Shell1 methods (FocusWindow, FocusWorkspace,
# CloseWindow) really act. Never touches the live session.
#
# Needs gnome-shell, mutter, gjs and a Wayland client (kitty or foot). Without root:
#     dnf download --resolve --destdir /tmp/g gnome-shell mutter gjs gsettings-desktop-schemas
#     for f in /tmp/g/*.rpm; do rpm2cpio $f | (cd /tmp/groot && cpio -idm); done
#     SLIVER_GNOME_ROOT=/tmp/groot integration/gnome-shell/test.sh
# Skips (exit 0) when gnome-shell or a client is missing.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=${SLIVER_GNOME_ROOT:-}
if [ -n "$root" ]; then
    export PATH="$root/usr/bin:$PATH"
    [ -f "$root/usr/share/glib-2.0/schemas/gschemas.compiled" ] || glib-compile-schemas "$root/usr/share/glib-2.0/schemas" 2>/dev/null
    export GSETTINGS_SCHEMA_DIR="$root/usr/share/glib-2.0/schemas"
fi
# gnome-shell has /usr/share/gnome-shell compiled in. Unprivileged, an overlay in a private user
# and mount namespace puts the extracted files over the real /usr/share and /usr/lib64 (libgweather's
# location database lives under lib64); nothing outside the namespace changes.
if [ -n "$root" ] && [ -z "${SLIVER_IN_NS:-}" ] && [ ! -f /usr/share/gnome-shell/gnome-shell-dbus-interfaces.gresource ]; then
    export SLIVER_IN_NS=1
    exec unshare -Urm bash -c '
        for d in share lib64; do
            mount -t overlay overlay -o lowerdir="$1/usr/$d:/usr/$d",userxattr /usr/$d || exit 1
        done
        shift; exec "$@"' _ "$root" "$0" "$@"
fi
client=$(command -v kitty || command -v foot || true)
command -v gnome-shell >/dev/null && [ -n "$client" ] || { echo "SKIP: gnome-shell and a Wayland client (kitty/foot) are needed"; exit 0; }
exec dbus-run-session --config-file="$here/../kwin/session.conf" -- bash ${BASH_FLAGS:-} "$here/test-inner.sh" "$here" "$client"
