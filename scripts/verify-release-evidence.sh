#!/usr/bin/env bash

VERIFY_RELEASE_MODIFIER_SIDES=(
    left_ctrl right_ctrl left_alt right_alt
    left_shift right_shift left_super right_super
)

verify_release_modifier_list() {
    local list=$1 side index previous=-1
    [[ "$list" == "-" ]] && return 0
    [[ -n "$list" && "$list" != *, && "$list" != ,* && "$list" != *,,* &&
        "$list" != *[[:space:]]* ]] || return 1

    local -a sides=()
    IFS=',' read -r -a sides <<< "$list"
    ((${#sides[@]} > 0)) || return 1
    for side in "${sides[@]}"; do
        index=-1
        for ((index = 0; index < ${#VERIFY_RELEASE_MODIFIER_SIDES[@]}; index++)); do
            [[ "${VERIFY_RELEASE_MODIFIER_SIDES[index]}" == "$side" ]] && break
        done
        ((index < ${#VERIFY_RELEASE_MODIFIER_SIDES[@]})) || return 1
        ((index > previous)) || return 1
        previous=$index
    done
}

verify_release_modifier_in_list() {
    local list=$1 wanted=$2 side
    [[ "$list" == "-" ]] && return 1
    local -a sides=()
    IFS=',' read -r -a sides <<< "$list"
    for side in "${sides[@]}"; do
        [[ "$side" == "$wanted" ]] && return 0
    done
    return 1
}

# The operator must account for every known physical side exactly once. The
# two lists are deliberately ordered so the evidence is stable and easy to
# audit without interpreting prose.
verify_release_modifier_evidence() {
    local value=$1 tested hardware_na side
    [[ "$value" =~ ^tested=([^[:space:]]+)[[:space:]]hardware_na=([^[:space:]]+)$ ]] || return 1
    tested=${BASH_REMATCH[1]}
    hardware_na=${BASH_REMATCH[2]}
    verify_release_modifier_list "$tested" || return 1
    verify_release_modifier_list "$hardware_na" || return 1

    for side in "${VERIFY_RELEASE_MODIFIER_SIDES[@]}"; do
        if verify_release_modifier_in_list "$tested" "$side"; then
            if verify_release_modifier_in_list "$hardware_na" "$side"; then
                return 1
            fi
        elif verify_release_modifier_in_list "$hardware_na" "$side"; then
            :
        else
            return 1
        fi
    done
}

verify_release_performance_artifacts() {
    local directory=$1 artifact
    [[ -d "$directory" ]] || return 1
    for artifact in frame-trace input-trace latency-trace measurement-notes; do
        [[ -f "$directory/$artifact" && ! -L "$directory/$artifact" && -s "$directory/$artifact" ]] || return 1
    done
    grep -Fx 'workload=video-2008x60.lua' "$directory/measurement-notes" >/dev/null || return 1
    grep -Fx 'source=real-panel' "$directory/measurement-notes" >/dev/null || return 1
    grep -Fx 'host=Mac14,7' "$directory/measurement-notes" >/dev/null || return 1
    grep -E '^method=.+$' "$directory/measurement-notes" >/dev/null || return 1
}
