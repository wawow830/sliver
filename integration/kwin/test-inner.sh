#!/usr/bin/env bash
set -euo pipefail
here=$1
work=$(mktemp -d)
log=$work/calls.jsonl
pids=()
cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
    # Services the private bus activated (portals, ...) outlive dbus-run-session: stop exactly
    # the processes that belong to this bus, nothing from the real session.
    for e in /proc/[0-9]*/environ; do
        if tr '\0' '\n' <"$e" 2>/dev/null | grep -qxF "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"; then
            pid=${e#/proc/}; pid=${pid%/environ}
            [ "$pid" != "$$" ] && kill "$pid" 2>/dev/null || true
        fi
    done
    wait 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

touch "$log"
python3 "$here/mock-desktop1.py" "$log" & pids+=($!)
sleep 0.5

sock=wl-sliver-test-$$
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
kwin_wayland --virtual --no-lockscreen --no-global-shortcuts --socket "$sock" --width 1280 --height 720 \
    >"$work/kwin.log" 2>&1 & pids+=($!)
for _ in $(seq 1 50); do
    [ -S "$XDG_RUNTIME_DIR/$sock" ] && busctl --user status org.kde.KWin >/dev/null 2>&1 && break
    sleep 0.2
done
busctl --user status org.kde.KWin >/dev/null || { echo "kwin did not start"; cat "$work/kwin.log"; exit 1; }

id=$(busctl --user call org.kde.KWin /Scripting org.kde.kwin.Scripting loadScript ss \
    "$here/sliver-focus/contents/code/main.js" sliver-focus-test | awk '{print $2}')
busctl --user call org.kde.KWin "/Scripting/Script$id" org.kde.kwin.Script run >/dev/null

WAYLAND_DISPLAY=$sock kitty --class sliver-test-app --title "hello sliver" sh -c 'sleep 8' \
    >"$work/kitty.log" 2>&1 & pids+=($!)

for _ in $(seq 1 50); do
    grep -q 'sliver-test-app' "$log" && break
    sleep 0.2
done
cat "$log"
grep -q '"app-id"' "$log" && grep -q 'sliver-test-app' "$log" || { echo "FAIL: no SetFocus for the test window"; cat "$work/kwin.log" | tail -20; exit 1; }
grep -q '"method": "SetWorkspacesV"' "$log" || { echo "FAIL: no SetWorkspacesV"; exit 1; }
echo "ok: kwin script reported the focused window and the desktops"

# The write side: the agent switches desktops with KWin's own setCurrentDesktop (control::bus_call).
busctl --user call org.kde.KWin /VirtualDesktopManager org.kde.KWin.VirtualDesktopManager createDesktop us 1 "Two" >/dev/null
sleep 0.5
first=$(busctl --user get-property org.kde.KWin /VirtualDesktopManager org.kde.KWin.VirtualDesktopManager current)
[ "$(busctl --user call org.kde.KWin /KWin org.kde.KWin setCurrentDesktop i 2)" = "b true" ] || { echo "FAIL: setCurrentDesktop 2"; exit 1; }
sleep 0.5
second=$(busctl --user get-property org.kde.KWin /VirtualDesktopManager org.kde.KWin.VirtualDesktopManager current)
[ "$first" != "$second" ] || { echo "FAIL: the current desktop did not change ($first)"; exit 1; }
grep -q '"Two"' "$log" || { echo "FAIL: the script did not report the new desktop"; exit 1; }
echo "ok: setCurrentDesktop switched desktops and the script reported them"
