#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source "$root/scripts/verify-release-session.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
printf 'Gid:\t1000\t1000\t1000\t1000\nGroups:\t10 39 104 1000\n' > "$tmp/stale"
printf 'Gid:\t1000\t1000\t1000\t1000\nGroups:\t10 39 104 976 1000\n' > "$tmp/fresh"
! verify_release_process_has_group 976 "$tmp/stale"
verify_release_process_has_group 976 "$tmp/fresh"
! verify_release_process_has_group 97 "$tmp/fresh"
! verify_release_process_has_group 976 "$tmp/missing"
! verify_release_process_has_group '' "$tmp/fresh"
# Replay a fresh shell with a stale manager. Account lookup alone says yes.
getent() { printf 'sliver-supervisors:x:976:wawow\n'; }
systemctl() { printf '963\n'; }
id() { printf '1000\n'; }
verify_release_process_has_group() {
    [[ "$1" == 976 ]] || return 1
    [[ "$2" == "/proc/$$/status" ]] && return 0
    [[ "$2" == /proc/963/status && "${manager_fresh:-0}" == 1 ]]
}
! verify_release_session_groups_ready
manager_fresh=1
verify_release_session_groups_ready
# The production pre-takeover stage must use this check, not id USER.
grep -F 'if verify_release_session_groups_ready; then' "$root/scripts/verify-release.sh" >/dev/null || {
    echo 'fresh-session stage does not reject a stale user manager' >&2
    exit 1
}
printf 'verify-release session tests passed\n'
