#!/usr/bin/env bash
# The sway backend against a real, headless sway (WLR_BACKENDS=headless): the window list and
# focus follow two real clients, and the taskbar/workspace actions (control.rs) really move
# focus, switch workspace and close a window. A private runtime dir and no outputs of its own:
# the live session is never touched.
#
# Needs sway + foot. Not installed on the dev machine? Without root:
#     dnf download --resolve --destdir /tmp/rpms sway foot wlroots0.19
#     for f in /tmp/rpms/{sway-1,wlroots0,foot-1}*.rpm; do rpm2cpio $f | (cd /tmp/root && cpio -idm); done
#     SLIVER_TEST_ROOT=/tmp/root integration/sway/test.sh
# Skips (exit 0) when sway or foot is missing.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
prefix=${SLIVER_TEST_ROOT:-}
if [ -n "$prefix" ]; then
    export PATH="$prefix/usr/bin:$PATH" LD_LIBRARY_PATH="$prefix/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
command -v sway >/dev/null && command -v foot >/dev/null || { echo "SKIP: sway and foot are needed"; exit 0; }

cargo build -q -p sliver-desktop --examples --manifest-path "$root/Cargo.toml"
watch=$root/target/debug/examples/desktop-watch
act=$root/target/debug/examples/desktop-act

work=$(mktemp -d /tmp/sliver-sway.XXXX)
chmod 700 "$work"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT

printf 'output HEADLESS-1 resolution 1280x720\nfloating_modifier Mod4\n' >"$work/config"
clean=(env -u SWAYSOCK -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u NIRI_SOCKET -u DISPLAY -u XDG_CURRENT_DESKTOP
       XDG_RUNTIME_DIR="$work" WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1)
"${clean[@]}" sway -c "$work/config" >"$work/sway.log" 2>&1 & pids+=($!)
for _ in $(seq 1 50); do ls "$work"/sway-ipc.*.sock >/dev/null 2>&1 && [ -S "$work/wayland-1" ] && break; sleep 0.1; done
sock=$(ls "$work"/sway-ipc.*.sock | head -1)
clean+=(SWAYSOCK="$sock" WAYLAND_DISPLAY=wayland-1)

"${clean[@]}" "$watch" >"$work/states.jsonl" 2>"$work/watch.err" & pids+=($!)
sleep 1

# Two clients, the second one gets focus.
"${clean[@]}" foot -a sliver-a -T "title a" sh -c 'sleep 60' >/dev/null 2>&1 & pids+=($!)
sleep 1
"${clean[@]}" foot -a sliver-b -T "title b" sh -c 'sleep 60' >/dev/null 2>&1 & pids+=($!)

last() { tail -n 1 "$work/states.jsonl"; }
field() { last | python3 -c "import json,sys; s=json.load(sys.stdin); print($1)" 2>/dev/null || true; }
wait_for() { # description, python expression over s
    for _ in $(seq 1 60); do [ "$(field "$2")" = "True" ] && return 0; sleep 0.2; done
    echo "FAIL: $1"; last | head -c 1500; echo; head -5 "$work/watch.err"
    echo "sway's own tree:"; "${clean[@]}" swaymsg -t get_tree | grep -E '"app_id": "sliver' || true
    echo "states seen: $(wc -l <"$work/states.jsonl")"; exit 1
}

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
