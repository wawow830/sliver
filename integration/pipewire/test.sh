#!/usr/bin/env bash
# The capture follower (the `privacy` module's data) against a REAL PipeWire: a private pipewire
# and wireplumber (policy only: no hardware, no devices of the real machine), a null microphone,
# and pw-record as the recording app. Checks that "pw-record" shows up as a microphone user while
# it records and is gone when it stops.
#
# Needs pipewire, wireplumber, pw-cli and pw-record (pipewire-utils). Without root, the way
# integration/sway/test.sh does it: extract the rpms into a prefix and set SLIVER_TEST_ROOT.
# Skips (exit 0) when something is missing.
. "$(dirname "$0")/../lib.sh"
need pipewire wireplumber pw-cli pw-record dbus-daemon
[ -f /usr/share/pipewire/pipewire.conf ] || { echo "SKIP: /usr/share/pipewire/pipewire.conf not found"; exit 0; }
setup pw sliver-session

export XDG_RUNTIME_DIR=$work XDG_CONFIG_HOME=$work/cfg XDG_STATE_HOME=$work/state XDG_DATA_HOME=$work/data XDG_CACHE_HOME=$work/cache
mkdir -p "$work/pwconf" "$work/cfg" "$work/state" "$work/data" "$work/cache"
# Only the stock server config: no drop-ins (Fedora Asahi's microphone filter chain would open the
# real microphone and fight the user's own PipeWire for it).
cp /usr/share/pipewire/pipewire.conf /usr/share/pipewire/client.conf "$work/pwconf/"
export PIPEWIRE_CONFIG_DIR=$work/pwconf

spawn pipewire
wait_until "pipewire is up" test -S "$work/pipewire-0"
# Policy only: it links streams to the null source below and starts them; no ALSA, no bluez.
spawn wireplumber -p policy
sleep 1.5
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=test-mic media.class=Audio/Source object.linger=true audio.position=[FL,FR] }' >/dev/null

bus
"$examples/capture-watch" "$work/pipewire-0" >"$work/out" 2>"$work/err" & pids+=($!)

fail() { echo "FAIL: $1"; echo "--- watcher"; cat "$work/out" "$work/err"; echo "--- wireplumber"; tail -5 "$work/wireplumber.log"; exit 1; }
waitfor() { # description, grep pattern for the LAST line of the watcher's output
    for _ in $(seq 1 60); do tail -n 1 "$work/out" 2>/dev/null | grep -q "$2" && return 0; sleep 0.2; done
    fail "$1"
}

waitfor "the follower reports nothing capturing at first" 'mic: \[\], camera: \[\], screen: \[\]'
spawn pw-record --target test-mic "$work/rec.wav"; rec=$last_pid
waitfor "pw-record shows up as a microphone user" 'mic: \["pw-record"\]'
echo "ok: a running recording stream is the microphone, with the app's name"
kill $rec
waitfor "pw-record is gone after it stops" 'mic: \[\], camera: \[\], screen: \[\]'
echo "ok: it disappears when the app stops"
