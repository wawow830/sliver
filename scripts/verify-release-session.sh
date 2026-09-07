#!/usr/bin/env bash

# Check kernel credentials, not the account database. A lingering user manager
# can retain its old supplementary groups after a new graphical login.
verify_release_process_has_group() {
    local gid=$1 status_file=$2
    [[ "$gid" =~ ^[0-9]+$ && -r "$status_file" ]] || return 1
    awk -v gid="$gid" '
        $1 == "Groups:" {
            for (i = 2; i <= NF; i++) if ($i == gid) found = 1
        }
        $1 == "Gid:" && $3 == gid { found = 1 }
        END { exit !found }
    ' "$status_file"
}

verify_release_session_is_fresh() {
    local current_id=$1 initial_id=$2 session_started_at=$3 setup_finished_at=$4
    [[ -n "$current_id" && -n "$initial_id" ]] || return 1
    [[ "$current_id" != "$initial_id" ]] && return 0
    [[ -n "$session_started_at" && -n "$setup_finished_at" ]] || return 1
    local session_started_seconds setup_seconds
    session_started_seconds=$(date -d "$session_started_at" +%s) || return 1
    setup_seconds=$(date -d "$setup_finished_at" +%s) || return 1
    (( session_started_seconds > setup_seconds ))
}

verify_release_session_groups_ready() {
    local gid manager_pid
    gid=$(getent group sliver-supervisors | cut -d: -f3)
    [[ "$gid" =~ ^[0-9]+$ ]] || return 1
    verify_release_process_has_group "$gid" "/proc/$$/status" || return 1
    manager_pid=$(systemctl show "user@$(id -u).service" -p MainPID --value) || return 1
    [[ "$manager_pid" =~ ^[1-9][0-9]*$ ]] || return 1
    verify_release_process_has_group "$gid" "/proc/$manager_pid/status"
}
