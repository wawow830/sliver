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
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
prefix=${SLIVER_TEST_ROOT:-}
if [ -n "$prefix" ]; then
    export PATH="$prefix/usr/bin:$PATH" LD_LIBRARY_PATH="$prefix/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
command -v river >/dev/null && command -v foot >/dev/null || { echo "SKIP: river-classic and foot are needed"; exit 0; }
river -version 2>/dev/null | grep -q '^0\.3' || { echo "SKIP: this river is not river-classic 0.3 (0.4 needs a separate window manager)"; exit 0; }

cargo build -q -p sliver-desktop --examples --manifest-path "$root/Cargo.toml"
watch=$root/target/debug/examples/wlr-watch

work=$(mktemp -d /tmp/sliver-river.XXXX)
chmod 700 "$work"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT

clean=(env -u SWAYSOCK -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u NIRI_SOCKET -u DISPLAY -u XDG_CURRENT_DESKTOP
       XDG_RUNTIME_DIR="$work" WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1)
"${clean[@]}" river >"$work/river.log" 2>&1 & pids+=($!)
for _ in $(seq 1 50); do [ -S "$work/wayland-1" ] && break; sleep 0.1; done
clean+=(WAYLAND_DISPLAY=wayland-1)

# A watcher per scenario: its actions go through its own Wayland connection.
run_watch() { # flags...; output in $work/states.jsonl
    : >"$work/states.jsonl"
    "${clean[@]}" "$watch" "$@" >"$work/states.jsonl" 2>"$work/watch.err" & watcher=$!
    pids+=($watcher)
}
stop_watch() { kill "$watcher" 2>/dev/null || true; wait "$watcher" 2>/dev/null || true; }
last() { tail -n 1 "$work/states.jsonl"; }
field() { last | python3 -c "import json,sys; s=json.load(sys.stdin); print($1)" 2>/dev/null || true; }
wait_for() {
    for _ in $(seq 1 60); do [ "$(field "$2")" = "True" ] && return 0; sleep 0.2; done
    echo "FAIL: $1"; last | head -c 1500; echo; head -5 "$work/watch.err"; exit 1
}

"${clean[@]}" foot -a sliver-a -T "title a" sh -c 'sleep 90' >/dev/null 2>&1 & pids+=($!)
sleep 1
"${clean[@]}" foot -a sliver-b -T "title b" sh -c 'sleep 90' >/dev/null 2>&1 & pids+=($!)
sleep 1

run_watch
wait_for "both windows are listed" "sorted(w['class'] for w in s['windows'])==['sliver-a','sliver-b']"
wait_for "the newest window has focus" "s['window'] and s['window']['class']=='sliver-b'"
echo "ok: window list and focus come from the real compositor"
stop_watch

run_watch --focus-first-window
wait_for "focus moved to the first window" "s['window'] and s['window']['class']=='sliver-a'"
grep -q "focus: Ok" "$work/watch.err" || { echo "FAIL: the focus request errored"; cat "$work/watch.err"; exit 1; }
echo "ok: FocusWindow moved focus"
stop_watch

run_watch --close-focused
sleep 1
wait_for "a window was closed" "len(s['windows'])==1"
echo "ok: CloseWindow closed the focused window"
stop_watch

run_watch
sleep 1
[ "$(field "s['workspaces']")" = "[]" ] || { echo "FAIL: expected no workspaces (river has no ext-workspace); river-status support may have landed: update this test"; last; exit 1; }
echo "ok: no workspaces, as documented (river's tags use a private protocol)"
stop_watch
