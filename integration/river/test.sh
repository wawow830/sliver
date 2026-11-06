#!/usr/bin/env bash
# The generic Wayland backend (wlr-foreign-toplevel) against a real, headless river-classic 0.3:
# the window list and focus follow two real clients, and the focus/close actions work. river has
# no ext-workspace (its "tags" are a private protocol, river-status), so workspaces stay empty:
# that is checked too, as the known gap. Private runtime dir; the live session is never touched.
#
# river 0.4 ("river", not "river-classic") is a different thing: it manages no windows itself and
# needs a separate window-manager client, so nothing maps without one. Not covered.
#
# Needs river-classic + foot. Without root see integration/sway/test.sh and set SLIVER_TEST_ROOT.
# Skips (exit 0) when either is missing.
. "$(dirname "$0")/../lib.sh"
need river foot
river -version 2>/dev/null | grep -q '^0\.3' || { echo "SKIP: this river is not river-classic 0.3 (0.4 needs a separate window manager)"; exit 0; }
setup river

spawn river
wait_until "river is up" test -S "$work/wayland-1"
clean+=(WAYLAND_DISPLAY=wayland-1)

client sliver-a
sleep 1
client sliver-b
sleep 1

watch wlr-watch
wait_for "both windows are listed" "sorted(w['class'] for w in s['windows'])==['sliver-a','sliver-b']"
wait_for "the newest window has focus" "s['window'] and s['window']['class']=='sliver-b'"
echo "ok: window list and focus come from the real compositor"
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

watch wlr-watch
sleep 1
[ "$(field "s['workspaces']")" = "[]" ] || { echo "FAIL: expected no workspaces (river has no ext-workspace); river-status support may have landed: update this test"; last; exit 1; }
echo "ok: no workspaces, as documented (river's tags use a private protocol)"
unwatch
