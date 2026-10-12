#!/usr/bin/env bash
set -euo pipefail
here=$1
client=$2
work=$(mktemp -d /tmp/sliver-gnome.XXXX)
chmod 700 "$work"
log=$work/calls.jsonl
pids=()
cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
    for e in /proc/[0-9]*/environ; do
        if tr '\0' '\n' <"$e" 2>/dev/null | grep -qxF "DBUS_SESSION_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS"; then
            pid=${e#/proc/}; pid=${pid%/environ}
            [ "$pid" != "$$" ] && kill "$pid" 2>/dev/null || true
        fi
    done 2>/dev/null
    wait 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

touch "$log"
python3 "$here/../kwin/mock-desktop1.py" "$log" 2>/dev/null & pids+=($!)

# A private settings store (no dconf), the extension installed and enabled.
export XDG_CONFIG_HOME=$work/config XDG_DATA_HOME=$work/data XDG_RUNTIME_DIR=$work/run
export GSETTINGS_BACKEND=keyfile
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME/gnome-shell/extensions" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
uuid=sliver-focus@sliver.wawow830.github.io
cp -r "$here/$uuid" "$XDG_DATA_HOME/gnome-shell/extensions/"
gsettings set org.gnome.shell enabled-extensions "['$uuid']"
gsettings set org.gnome.shell disable-user-extensions false
gsettings set org.gnome.shell disable-extension-version-validation true
gsettings set org.gnome.mutter dynamic-workspaces false 2>/dev/null || true
gsettings set org.gnome.desktop.wm.preferences num-workspaces 3 2>/dev/null || true

unset WAYLAND_DISPLAY DISPLAY HYPRLAND_INSTANCE_SIGNATURE XDG_CURRENT_DESKTOP
gnome-shell --headless --virtual-monitor 1280x720 --wayland --no-x11 >"$work/shell.log" 2>&1 & pids+=($!)
for _ in $(seq 1 100); do [ -S "$XDG_RUNTIME_DIR/wayland-0" ] && break; sleep 0.2; done
[ -S "$XDG_RUNTIME_DIR/wayland-0" ] || { echo "FAIL: gnome-shell did not start"; tail -20 "$work/shell.log"; exit 1; }

last() { { grep "\"method\": \"$1\"" "$log" || true; } | tail -1; }
wait_for() { # description, method, python expression over the last call's args `a`
    for _ in $(seq 1 100); do
        l=$(last "$2")
        if [ -n "$l" ] && [ "$(echo "$l" | python3 -c "import json,sys; a=json.load(sys.stdin)['arg']; print($3)" 2>/dev/null)" = "True" ]; then return 0; fi
        sleep 0.2
    done
    echo "FAIL: $1"; echo "last $2: $(last "$2" | head -c 800)"; echo "--- shell log"; grep -iE "sliver|error|exception" "$work/shell.log" | head -20; exit 1
}
v() { echo "a[...]"; }

wait_for "the extension reports workspaces" SetWorkspaces "len(a)>=1"
echo "ok: the extension loaded and reported workspaces"

WAYLAND_DISPLAY=wayland-0 "$client" --class sliver-test-app --title "hello sliver" sh -c 'sleep 60' >"$work/c1.log" 2>&1 & pids+=($!)
wait_for "SetWindows lists the window, focused" SetWindows "any(w['app-id']['value']=='sliver-test-app' and w['focused']['value'] for w in a)"
wait_for "SetFocus names it" SetFocus "a.get('app-id',{}).get('value')=='sliver-test-app'"
echo "ok: focus and the window list reach the agent"

WAYLAND_DISPLAY=wayland-0 "$client" --class sliver-test-two --title "second" sh -c 'sleep 60' >"$work/c2.log" 2>&1 & pids+=($!)
wait_for "two windows, the new one focused" SetWindows "len(a)==2 and [w for w in a if w['focused']['value']][0]['app-id']['value']=='sliver-test-two'"
first=$(last SetWindows | python3 -c "
import json,sys
a=json.load(sys.stdin)['arg']
print([w['window']['value'] for w in a if w['app-id']['value']=='sliver-test-app'][0])")
second=${first/#mutter:/}
call() { busctl --user call io.github.wawow830.Sliver.Shell /io/github/wawow830/Sliver/Shell io.github.wawow830.Sliver.Shell1 "$@" >/dev/null; }
call FocusWindow s "${first#mutter:}"
wait_for "FocusWindow moved focus to the first window" SetWindows "[w for w in a if w['focused']['value']][0]['app-id']['value']=='sliver-test-app'"
echo "ok: Shell1.FocusWindow moved focus and the list followed"

call FocusWorkspace i 2
wait_for "workspace 2 is active" SetWorkspaces "[w for w in a if w['active']['value']][0]['id']['value']=='2'"
echo "ok: Shell1.FocusWorkspace switched workspace"

call CloseWindow s "${first#mutter:}"
wait_for "the first window closed" SetWindows "all(w['app-id']['value']!='sliver-test-app' for w in a)"
echo "ok: Shell1.CloseWindow closed the window"
