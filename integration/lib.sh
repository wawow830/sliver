# Sourced by the integration tests and scripts/settings-headless: one private runtime directory,
# every process started through `spawn` stopped on exit, a private D-Bus, and waits on the JSON
# states a backend example prints, one per line, into $work/states.jsonl. The live session is never
# touched: the environment that would reach it is cleared (`$clean`).
#
#   . "$(dirname "$0")/../lib.sh"
#   need sway foot                 # skip (exit 0) unless installed
#   setup sway sliver-desktop      # $root, $work, $clean; that crate's examples built
#   bus                            # a private session bus (nothing can be activated on it)
#   spawn sway -c "$work/config"   # background, under $clean, stopped on exit
#   watch desktop-watch            # an example ($examples) writing states.jsonl
#   wait_for "focus moved" "s['window']['class']=='a'"
#
# Packages not installed? Without root: `dnf download --resolve` the rpms, `rpm2cpio | cpio -idm`
# them into a prefix, and set SLIVER_TEST_ROOT to it.

set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [ -n "${SLIVER_TEST_ROOT:-}" ]; then
    export PATH="$SLIVER_TEST_ROOT/usr/bin:$PATH"
    export LD_LIBRARY_PATH="$SLIVER_TEST_ROOT/usr/lib64${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

need() {
    for c in "$@"; do
        command -v "$c" >/dev/null || { echo "SKIP: $* needed ($c is missing)"; exit 0; }
    done
}

examples=$root/target/debug/examples
pids=()
# On exit: `on_exit` (if defined), then every spawned process group gets SIGTERM, and SIGKILL
# after 5 s (the same contract as scripts/sandbox.py), then $work goes.
stop_all() {
    if declare -F on_exit >/dev/null; then on_exit || true; fi
    if [ ${#pids[@]} -gt 0 ]; then
        # A group, or a lone process started with `& pids+=($!)`.
        local targets=("${pids[@]/#/-}" "${pids[@]}")
        kill -- "${targets[@]}" 2>/dev/null || true
        setsid sh -c 'sleep 5; kill -KILL -- "$@" 2>/dev/null' _ "${targets[@]}" &
        local killer=$!
        wait "${pids[@]}" 2>/dev/null || true
        kill -- "-$killer" 2>/dev/null || true
    fi
    rm -rf "$work"
}

# setup NAME [CRATE...]: the private runtime dir ($work), the cleared environment ($clean), and
# the crates' examples, in $examples.
setup() {
    local crate
    for crate in "${@:2}"; do
        cargo build -q -p "$crate" --examples --manifest-path "$root/Cargo.toml"
    done
    work=$(mktemp -d "/tmp/sliver-$1.XXXX")
    chmod 700 "$work"
    trap stop_all EXIT
    clean=(env -u SWAYSOCK -u WAYLAND_DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE -u NIRI_SOCKET -u DISPLAY
           -u XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR="$work" WLR_BACKENDS=headless WLR_RENDERER=pixman
           WLR_LIBINPUT_NO_DEVICES=1)
}

# bus: a private dbus-daemon (integration/bus.conf) as the session and system bus.
bus() {
    spawn dbus-daemon --config-file="$root/integration/bus.conf" --nofork --print-address=3 3>"$work/bus.addr"
    wait_until "the private bus is up" test -s "$work/bus.addr"
    DBUS_SESSION_BUS_ADDRESS=$(head -1 "$work/bus.addr")
    export DBUS_SESSION_BUS_ADDRESS DBUS_SYSTEM_BUS_ADDRESS=$DBUS_SESSION_BUS_ADDRESS
}

# spawn CMD...: run it in the background under $clean, in its own process group (its output in $work/<program>.log; a
# leading `env VAR=value…` is skipped for the name).
spawn() {
    local name
    for name in "$@"; do [[ $name == env || $name == *=* ]] || break; done
    "${clean[@]}" setsid "$@" >"$work/$(basename "$name").log" 2>&1 &
    pids+=($!)
    last_pid=$!
}

# wait_until DESCRIPTION TEST...: up to 8 s for a shell test (a socket to appear, say).
wait_until() {
    local what=$1; shift
    for _ in $(seq 1 80); do "$@" && return 0; sleep 0.1; done
    echo "FAIL: $what"; exit 1
}

# watch EXAMPLE [ARGS...]: (re)start a backend example writing states into $work/states.jsonl.
# Each run has its own compositor connection, which its actions (`--focus-first-window`) use.
watch() {
    unwatch
    : >"$work/states.jsonl"
    "${clean[@]}" setsid "$examples/$1" "${@:2}" >"$work/states.jsonl" 2>"$work/watch.err" &
    pids+=($!)
    watcher=$!
}
unwatch() {
    if [ -n "${watcher:-}" ]; then
        kill -- "-$watcher" 2>/dev/null || true
        wait "$watcher" 2>/dev/null || true
    fi
    watcher=
}

last() { tail -n 1 "$work/states.jsonl"; }
# field EXPR: a Python expression over the latest state `s`.
field() { last | python3 -c "import json,sys; s=json.load(sys.stdin); print($1)" 2>/dev/null || true; }
# wait_for DESCRIPTION EXPR: up to 12 s for EXPR to hold on the latest state. On failure it
# prints the state, the watcher's errors, and whatever `on_fail` (if defined) adds.
wait_for() {
    for _ in $(seq 1 60); do [ "$(field "$2")" = "True" ] && return 0; sleep 0.2; done
    echo "FAIL: $1"; last | head -c 1500; echo; head -5 "$work/watch.err"
    if declare -F on_fail >/dev/null; then on_fail; fi
    echo "states seen: $(wc -l <"$work/states.jsonl")"
    exit 1
}

# client APP_ID: a foot window that lives 90 s.
client() { spawn foot -a "$1" -T "title ${1#sliver-}" sh -c 'sleep 90'; }
