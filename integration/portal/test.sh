#!/usr/bin/env bash
# The `screenshot` module against the REAL xdg-desktop-portal (the frontend), on a private session
# bus. The portal's backend is a small mock (mock-backend.py) because the real ones (xdpw, gtk,
# kde...) need a compositor and a screen; what's checked is everything Sliver is responsible for:
# the call Sliver makes (`Screenshot("", {interactive: true})`, signature `sa{sv}`) is accepted by
# the real frontend, reaches a backend with its options intact, and the Response comes back.
#
# Needs xdg-desktop-portal (/usr/libexec), python3-gobject, dbus-daemon. Skips (exit 0) without.
. "$(dirname "$0")/../lib.sh"
here=$root/integration/portal
portal=${XDG_DESKTOP_PORTAL_BIN:-/usr/libexec/xdg-desktop-portal}
[ -x "$portal" ] || { echo "SKIP: $portal not installed"; exit 0; }
need dbus-daemon gdbus
python3 -c 'import gi; from gi.repository import Gio' 2>/dev/null || { echo "SKIP: python3-gobject is needed"; exit 0; }
setup portal sliver-agent

mkdir -p "$work"/{portals,cfg/xdg-desktop-portal,data,cache}
cat >"$work/portals/sliver_test.portal" <<'P'
[portal]
DBusName=org.freedesktop.impl.portal.desktop.sliver_test
Interfaces=org.freedesktop.impl.portal.Screenshot;org.freedesktop.impl.portal.Access;
UseIn=sliver-test
P
printf '[preferred]\ndefault=sliver_test\n' >"$work/cfg/xdg-desktop-portal/sliver-test-portals.conf"

bus
spawn python3 "$here/mock-backend.py" "$work/calls.jsonl"
wait_until "the mock backend is on the bus" sh -c \
    'gdbus introspect --session --dest org.freedesktop.impl.portal.desktop.sliver_test --object-path / >/dev/null 2>&1'
spawn env XDG_CONFIG_HOME="$work/cfg" XDG_DATA_HOME="$work/data" XDG_CACHE_HOME="$work/cache" \
    XDG_CURRENT_DESKTOP=sliver-test XDG_DESKTOP_PORTAL_DIR="$work/portals" "$portal" -v
for _ in $(seq 1 100); do
    gdbus introspect --session --dest org.freedesktop.portal.Desktop --object-path /org/freedesktop/portal/desktop 2>/dev/null \
        | grep -q 'interface org.freedesktop.portal.Screenshot' && break
    sleep 0.1
done
fail() { echo "FAIL: $1"; tail -15 "$work/xdg-desktop-portal.log"; cat "$work/python3.log" 2>/dev/null; cat "$work/calls.jsonl" 2>/dev/null; exit 1; }
gdbus introspect --session --dest org.freedesktop.portal.Desktop --object-path /org/freedesktop/portal/desktop 2>/dev/null \
    | grep -q 'interface org.freedesktop.portal.Screenshot' || fail "the portal never offered Screenshot"

out=$(timeout 20 "$examples/oneshot" screenshot 5000) || fail "the agent's screenshot call failed: $out"
printf '%s\n' "$out" | head -1 | grep -qx ok || fail "unexpected result: $out"
printf '%s\n' "$out" | grep -q '^response 0 .*uri=.*sliver-test-shot.png' || fail "no successful Response with the uri: $out"
python3 - "$work/calls.jsonl" <<'PY' || fail "the backend did not get the options"
import json, sys
calls = [json.loads(l) for l in open(sys.argv[1])]
shots = [c for c in calls if c["method"] == "Screenshot"]
assert len(shots) == 1, calls
assert shots[0]["options"].get("interactive") is True, shots
PY
echo "ok: the screenshot tap reaches a portal backend (interactive = true) and the Response returns the uri"
