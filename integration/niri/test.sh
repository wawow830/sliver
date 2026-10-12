#!/usr/bin/env bash
# The niri backend against a real niri 25.11+: niri runs nested (winit backend) inside a headless
# sway, so no display or GPU session is needed. Window list, focus and workspaces follow two real
# clients, and the actions (focus, switch workspace, close) really act. Private runtime dir; the
# live session is never touched.
#
# Needs niri, sway, foot. Without root see integration/sway/test.sh (dnf download --resolve,
# rpm2cpio into a prefix, SLIVER_TEST_ROOT). Skips (exit 0) when one is missing.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
prefix=${SLIVER_TEST_ROOT:-}
if [ -n "$prefix" ]; then
    export PATH="$prefix/usr/bin:$PATH" LD_LIBRARY_PATH="$prefix/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
for c in niri sway foot; do
    command -v $c >/dev/null || { echo "SKIP: $c is needed"; exit 0; }
done

cargo build -q -p sliver-desktop --examples --manifest-path "$root/Cargo.toml"
watch=$root/target/debug/examples/desktop-watch
act=$root/target/debug/examples/desktop-act

work=$(mktemp -d /tmp/sliver-niri.XXXX)
chmod 700 "$work"
mkdir -p "$work/cfg/niri"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT

printf 'output HEADLESS-1 resolution 1280x720\n' >"$work/sway.cfg"
echo 'prefer-no-csd' >"$work/cfg/niri/config.kdl"
base=(env -u SWAYSOCK -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u NIRI_SOCKET -u DISPLAY -u XDG_CURRENT_DESKTOP
      XDG_RUNTIME_DIR="$work")
"${base[@]}" WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c "$work/sway.cfg" >"$work/sway.log" 2>&1 & pids+=($!)
for _ in $(seq 1 50); do [ -S "$work/wayland-1" ] && break; sleep 0.1; done
"${base[@]}" WAYLAND_DISPLAY=wayland-1 XDG_CONFIG_HOME="$work/cfg" niri >"$work/niri.log" 2>&1 & pids+=($!)
for _ in $(seq 1 80); do ls "$work"/niri.wayland-2.*.sock >/dev/null 2>&1 && [ -S "$work/wayland-2" ] && break; sleep 0.1; done
sock=$(ls "$work"/niri.wayland-2.*.sock | head -1)
clean=("${base[@]}" NIRI_SOCKET="$sock" WAYLAND_DISPLAY=wayland-2)

"${clean[@]}" "$watch" >"$work/states.jsonl" 2>"$work/watch.err" & pids+=($!)
sleep 1
"${clean[@]}" foot -a sliver-a -T "title a" sh -c 'sleep 90' >/dev/null 2>&1 & pids+=($!)
sleep 1.5
"${clean[@]}" foot -a sliver-b -T "title b" sh -c 'sleep 90' >/dev/null 2>&1 & pids+=($!)

last() { tail -n 1 "$work/states.jsonl"; }
field() { last | python3 -c "import json,sys; s=json.load(sys.stdin); print($1)" 2>/dev/null || true; }
wait_for() {
    for _ in $(seq 1 60); do [ "$(field "$2")" = "True" ] && return 0; sleep 0.2; done
    echo "FAIL: $1"; last | head -c 1500; echo; head -5 "$work/watch.err"; tail -5 "$work/niri.log"; exit 1
}

wait_for "both windows are listed" "sorted(w['class'] for w in s['windows'])==['sliver-a','sliver-b']"
wait_for "the newest window has focus" "s['window'] and s['window']['class']=='sliver-b'"
echo "ok: window list and focus follow two real windows"

addr_a=$(field "[w['address'] for w in s['windows'] if w['class']=='sliver-a'][0]")
addr_b=$(field "[w['address'] for w in s['windows'] if w['class']=='sliver-b'][0]")
"${clean[@]}" "$act" focus "$addr_a" >/dev/null
wait_for "focus moved to the first window" "s['window'] and s['window']['class']=='sliver-a'"
echo "ok: FocusWindow moved focus"

# niri keeps one empty workspace below the used ones: switch to it and back.
wait_for "an empty workspace exists" "len(s['workspaces'])>=2"
cur=$(field "s['workspace']['id']")
other=$(field "[w['id'] for w in s['workspaces'] if not w['active']][0]")
other_name=$(field "[w['name'] for w in s['workspaces'] if not w['active']][0]")
"${clean[@]}" "$act" workspace "$other" "$other_name" >/dev/null
wait_for "the other workspace is current" "s['workspace'] and s['workspace']['id']==$other"
"${clean[@]}" "$act" workspace "$cur" x >/dev/null
wait_for "back on the first workspace" "s['workspace'] and s['workspace']['id']==$cur"
echo "ok: FocusWorkspace switched workspaces and back"

"${clean[@]}" "$act" close "$addr_b" >/dev/null
wait_for "the second window is gone" "[w['class'] for w in s['windows']]==['sliver-a']"
echo "ok: CloseWindow closed it"
