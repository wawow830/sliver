#!/usr/bin/env bash
# The sway backend against a real, headless sway (WLR_BACKENDS=headless): the window list and
# focus follow two real clients, and the taskbar/workspace actions (control.rs) really move
# focus, switch workspace and close a window. A private runtime dir and no outputs of its own.
#
# Needs sway + foot. Without root:
#     dnf download --resolve --destdir /tmp/rpms sway foot
#     for f in /tmp/rpms/{sway-1,wlroots0,foot-1,libliftoff}*.rpm; do rpm2cpio $f | (cd /tmp/root && cpio -idm); done
#     SLIVER_TEST_ROOT=/tmp/root integration/sway/test.sh
# Skips (exit 0) when sway or foot is missing.
. "$(dirname "$0")/../lib.sh"
need sway foot
setup sway sliver-desktop
on_fail() { echo "sway's own tree:"; "${clean[@]}" swaymsg -t get_tree | grep -E '"app_id": "sliver' || true; }

printf 'output HEADLESS-1 resolution 1280x720\nfloating_modifier Mod4\n' >"$work/config"
spawn sway -c "$work/config"
wait_until "sway is up" bash -c 'ls "$1"/sway-ipc.*.sock >/dev/null 2>&1 && [ -S "$1/wayland-1" ]' _ "$work"
clean+=(SWAYSOCK="$(ls "$work"/sway-ipc.*.sock | head -1)" WAYLAND_DISPLAY=wayland-1)
act=$examples/desktop-act

watch desktop-watch
sleep 1
# Two clients, the second one gets focus.
client sliver-a
sleep 1
client sliver-b

wait_for "both windows are listed" "sorted(w['class'] for w in s['windows'])==['sliver-a','sliver-b']"
wait_for "the newest window has focus" "s['window'] and s['window']['class']=='sliver-b'"
echo "ok: window list and focus follow two real windows"

addr_a=$(field "[w['address'] for w in s['windows'] if w['class']=='sliver-a'][0]")
addr_b=$(field "[w['address'] for w in s['windows'] if w['class']=='sliver-b'][0]")
"${clean[@]}" "$act" focus "$addr_a" >/dev/null
wait_for "focus moved to the first window" "s['window'] and s['window']['class']=='sliver-a'"
echo "ok: FocusWindow moved focus"

"${clean[@]}" "$act" workspace 2 2 >/dev/null
wait_for "workspace 2 is current" "s['workspace'] and s['workspace']['name']=='2'"
wait_for "workspace 2 is in the list as active" "any(w['name']=='2' and w['active'] for w in s['workspaces'])"
"${clean[@]}" "$act" workspace 1 1 >/dev/null
wait_for "back on workspace 1" "s['workspace'] and s['workspace']['name']=='1'"
echo "ok: FocusWorkspace switched workspaces and back"

"${clean[@]}" "$act" close "$addr_b" >/dev/null
wait_for "the second window is gone" "[w['class'] for w in s['windows']]==['sliver-a']"
echo "ok: CloseWindow closed it"
