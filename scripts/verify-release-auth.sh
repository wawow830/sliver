#!/usr/bin/env bash

# sudo uses this status when authentication prevented the command from
# running. It is retryable, unlike an error from the command being protected.
VERIFY_RELEASE_AUTH_REQUIRED=75

verify_release_authentication_error() {
    local output=$1
    [[ -f "$output" ]] || return 1
    grep -E -q '^sudo: (timed out reading password|a password is required|no password was provided|[0-9]+ incorrect password attempts?)$' "$output"
}

verify_release_capture_privileged() {
    local output=$1
    shift
    if "$@" > "$output" 2>&1; then
        return 0
    fi
    if verify_release_authentication_error "$output"; then
        return "$VERIFY_RELEASE_AUTH_REQUIRED"
    fi
    return 1
}

verify_release_should_auto_rollback() {
    local status=$1 auth_required=$2 rollback_attempted=$3 rollback_done=$4
    local snapshot_ready=$5 rollback_in_progress=$6
    (( status != 0 && auth_required == 0 && rollback_attempted == 0 &&
       rollback_done == 0 && snapshot_ready == 1 && rollback_in_progress == 0 ))
}
