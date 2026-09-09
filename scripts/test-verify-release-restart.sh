#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
fail() { printf 'restart evidence test failed: %s\n' "$*" >&2; exit 1; }
# Exercise the real stage with fake commands: never touch host services/journals.
source <(awk '
    /^(restart_stage|capture_broker_restart_journal|assert_restart_journal)\(\) \{/ { copying = 1 }
    copying { print }
    copying && /^}$/ { copying = 0 }
' "$root/scripts/verify-release.sh")
stage() { :; }
refuse_if_blocked() { :; }
confirm() { return 0; }
save_state() { :; }
pass_check() { :; }
fail_check() { printf '%s: %s\n' "$1" "$2" >&2; return 1; }
unit_active() { printf 'active\n'; }
user_unit_active() { printf 'active\n'; }
user_step() { :; }
privileged_step() {
    local id=$1
    shift
    case "$id" in
        restart_broker) : ;;
        capture_broker_journal) "$@" ;;
        *) fail "unexpected privileged step: $id" ;;
    esac
}
logged_step() {
    local id=$1 output=$2
    shift 2
    "$@" > "$output"
}
sudo() { "$@"; }
# More than 100 ordinary messages can legitimately follow a clean stop, or
# conceal an earlier failure. Model journalctl's -n limit, not a grep of source.
journalctl() {
    local unit=sliver-supervisor.service count='' previous=''
    for arg in "$@"; do
        [[ "$previous" != -n ]] || count=$arg
        [[ "$arg" != sliver-broker.service ]] || unit=$arg
        previous=$arg
    done
    {
        printf 'Stopped %s\n' "$unit"
        [[ ${BAD_RESTART:-0} == 0 ]] || printf '%s: Failed with result '\''timeout'\''.\n' "$unit"
        for ((i=0; i<150; i++)); do printf 'ordinary message %s\n' "$i"; done
    } > "$tmp/journal-source"
    if [[ -n "$count" ]]; then tail -n "$count" "$tmp/journal-source"; else cat "$tmp/journal-source"; fi
}
systemctl() { printf 'sliver-lua-worker-test.service loaded active running worker\n'; }
sleep() { :; }
manual_check() { :; }
VERIFY_DIR=$tmp
CONFIG_DIR=$tmp/fixtures
SELECTED_PATH=$tmp/config-path
printf '%s/hung.lua' "$CONFIG_DIR" > "$SELECTED_PATH"
restart_stage || fail 'clean stop was lost behind more than 100 messages'
for unit in user broker; do
    [[ $(wc -l < "$tmp/$unit-journal-restart.txt") == 151 ]] || fail "$unit journal was truncated"
done
BAD_RESTART=1
if (restart_stage); then fail 'earlier restart failure was hidden by later messages'; fi
printf 'restart evidence tests passed\n'
