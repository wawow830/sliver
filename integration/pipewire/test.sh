#!/usr/bin/env bash
# The capture follower (the `privacy` module's data) against a REAL PipeWire: a private pipewire
# and wireplumber (policy only: no hardware, no devices of the real machine), a null microphone,
# and pw-record as the recording app. Checks that "pw-record" shows up as a microphone user while
# it records and is gone when it stops, and that the bar turning off drops the connection.
#
# Needs pipewire, wireplumber, pw-cli and pw-record (pipewire-utils). Without root, the way
# integration/sway/test.sh does it: extract the rpms into a prefix and set SLIVER_TEST_ROOT.
# Skips (exit 0) when something is missing.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
prefix=${SLIVER_TEST_ROOT:-}
if [ -n "$prefix" ]; then
    export PATH="$prefix/usr/bin:$PATH" LD_LIBRARY_PATH="$prefix/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi
for c in pipewire wireplumber pw-cli pw-record dbus-daemon; do
    command -v $c >/dev/null || { echo "SKIP: $c is needed"; exit 0; }
done
[ -f /usr/share/pipewire/pipewire.conf ] || { echo "SKIP: /usr/share/pipewire/pipewire.conf not found"; exit 0; }

cargo build -q -p sliver-session --example capture-watch --manifest-path "$root/Cargo.toml"
watch=$root/target/debug/examples/capture-watch

work=$(mktemp -d /tmp/sliver-pw.XXXX)
chmod 700 "$work"
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; rm -rf "$work"; }
trap cleanup EXIT

export XDG_RUNTIME_DIR=$work/run XDG_CONFIG_HOME=$work/cfg XDG_STATE_HOME=$work/state XDG_DATA_HOME=$work/data XDG_CACHE_HOME=$work/cache
mkdir -p "$XDG_RUNTIME_DIR" "$work/pwconf" "$work/cfg" "$work/state" "$work/data" "$work/cache"
chmod 700 "$XDG_RUNTIME_DIR"
# Only the stock server config: no drop-ins (Fedora Asahi's microphone filter chain would open the
# real microphone and fight the user's own PipeWire for it).
cp /usr/share/pipewire/pipewire.conf /usr/share/pipewire/client.conf "$work/pwconf/"
export PIPEWIRE_CONFIG_DIR=$work/pwconf

pipewire >"$work/pw.log" 2>&1 & pids+=($!)
for _ in $(seq 1 50); do [ -S "$XDG_RUNTIME_DIR/pipewire-0" ] && break; sleep 0.1; done
# Policy only: it links streams to the null source below and starts them; no ALSA, no bluez.
wireplumber -p policy >"$work/wp.log" 2>&1 & pids+=($!)
sleep 1.5
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=test-mic media.class=Audio/Source object.linger=true audio.position=[FL,FR] }' >/dev/null

dbus-daemon --config-file="$root/integration/kwin/session.conf" --nofork --print-address=3 3>"$work/addr" >/dev/null 2>&1 & pids+=($!)
for _ in $(seq 1 50); do [ -s "$work/addr" ] && break; sleep 0.1; done
export DBUS_SESSION_BUS_ADDRESS=$(cat "$work/addr") DBUS_SYSTEM_BUS_ADDRESS=$(cat "$work/addr")
"$watch" "$XDG_RUNTIME_DIR/pipewire-0" >"$work/out" 2>"$work/err" & watcher=$!; pids+=($watcher)

fail() { echo "FAIL: $1"; echo "--- watcher"; cat "$work/out" "$work/err"; echo "--- wireplumber"; tail -5 "$work/wp.log"; exit 1; }
waitfor() { # description, grep pattern for the LAST line of the watcher's output
    for _ in $(seq 1 60); do tail -n 1 "$work/out" 2>/dev/null | grep -q "$2" && return 0; sleep 0.2; done
    fail "$1"
}

waitfor "the follower reports nothing capturing at first" 'mic: \[\], camera: \[\], screen: \[\]'
pw-record --target test-mic "$work/rec.wav" >/dev/null 2>&1 & rec=$!; pids+=($rec)
waitfor "pw-record shows up as a microphone user" 'mic: \["pw-record"\]'
echo "ok: a running recording stream is the microphone, with the app's name"
kill $rec
waitfor "pw-record is gone after it stops" 'mic: \[\], camera: \[\], screen: \[\]'
echo "ok: it disappears when the app stops"
