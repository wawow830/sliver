#!/usr/bin/env bash
# The generic Wayland backend (wlr-foreign-toplevel + ext-workspace) against a real, headless
# labwc: window list, focus and workspaces follow two real clients, and the control path
# (focus, close, switch workspace) works. Private runtime dir.
#
# Needs labwc + foot. Without root see integration/sway/test.sh (dnf download --resolve, rpm2cpio)
# and set SLIVER_TEST_ROOT. Skips (exit 0) when either is missing.
. "$(dirname "$0")/../lib.sh"
need labwc foot
setup labwc sliver-desktop

mkdir -p "$work/cfg"
cat >"$work/cfg/rc.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config>
  <desktops number="2"/>
</labwc_config>
XML
spawn labwc -C "$work/cfg"
wait_until "labwc is up" test -S "$work/wayland-0"
clean+=(WAYLAND_DISPLAY=wayland-0)

client sliver-a
sleep 1
client sliver-b
sleep 1

watch wlr-watch
wait_for "both windows are listed" "sorted(w['class'] for w in s['windows'])==['sliver-a','sliver-b']"
wait_for "the newest window has focus" "s['window'] and s['window']['class']=='sliver-b'"
wait_for "two workspaces, one active" "len(s['workspaces'])==2 and sum(w['active'] for w in s['workspaces'])==1"
echo "ok: window list, focus and workspaces come from the real compositor"
unwatch

watch wlr-watch --focus-first-window
wait_for "focus moved to the first window" "s['window'] and s['window']['class']=='sliver-a'"
grep -q "focus: Ok" "$work/watch.err" || { echo "FAIL: the focus request errored"; cat "$work/watch.err"; exit 1; }
echo "ok: FocusWindow moved focus"
unwatch

watch wlr-watch --close-focused
sleep 1
wait_for "a window was closed" "len(s['windows'])==1"
echo "ok: CloseWindow closed the focused window"
unwatch

watch wlr-watch --other-workspace
wait_for "the other workspace is active" "s['workspace'] and s['workspaces'] and [w['active'] for w in s['workspaces']].index(True)==1"
echo "ok: FocusWorkspace switched workspace"
unwatch
