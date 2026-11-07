#!/usr/bin/env bash
# The niri backend against a real niri 25.11+: niri runs nested (winit backend) inside a headless
# sway, so no display or GPU session is needed. Window list, focus and workspaces follow two real
# clients, and the actions (focus, switch workspace, close) really act. Private runtime dir.
#
# Needs niri, sway, foot. Without root see integration/sway/test.sh (dnf download --resolve,
# rpm2cpio into a prefix, SLIVER_TEST_ROOT). Skips (exit 0) when one is missing.
. "$(dirname "$0")/../lib.sh"
need niri sway foot
setup niri sliver-desktop
on_fail() { tail -5 "$work/niri.log"; }

mkdir -p "$work/cfg/niri"
printf 'output HEADLESS-1 resolution 1280x720\n' >"$work/sway.cfg"
echo 'prefer-no-csd' >"$work/cfg/niri/config.kdl"
spawn sway -c "$work/sway.cfg"
wait_until "sway is up" test -S "$work/wayland-1"
spawn env WAYLAND_DISPLAY=wayland-1 XDG_CONFIG_HOME="$work/cfg" niri
wait_until "niri is up" bash -c 'ls "$1"/niri.wayland-2.*.sock >/dev/null 2>&1 && [ -S "$1/wayland-2" ]' _ "$work"
clean+=(NIRI_SOCKET="$(ls "$work"/niri.wayland-2.*.sock | head -1)" WAYLAND_DISPLAY=wayland-2)
act=$examples/desktop-act

watch desktop-watch
sleep 1
client sliver-a
sleep 1.5
client sliver-b

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
