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
    local directory=$1 expected_interval=${2:-} expected_fps=${3:-}
    local expected_misses=${4:-} expected_input_delay=${5:-} expected_growth=${6:-}
    local artifact
    [[ $# -eq 6 && -d "$directory" ]] || return 1
    for artifact in frame-trace input-trace latency-trace measurement-notes; do
        [[ -f "$directory/$artifact" && ! -L "$directory/$artifact" && -s "$directory/$artifact" ]] || return 1
    done
    grep -Fx 'schema=sliver-performance-v1' "$directory/measurement-notes" >/dev/null || return 1
    grep -Fx 'workload=video-2008x60.lua' "$directory/measurement-notes" >/dev/null || return 1
    grep -Fx 'source=real-panel' "$directory/measurement-notes" >/dev/null || return 1
    grep -Fx 'host=Mac14,7' "$directory/measurement-notes" >/dev/null || return 1
    grep -E '^method=.+$' "$directory/measurement-notes" >/dev/null || return 1

    awk -F '\t' \
        -v frame="$directory/frame-trace" \
        -v input="$directory/input-trace" \
        -v latency="$directory/latency-trace" \
        -v expected_interval="$expected_interval" \
        -v expected_fps="$expected_fps" \
        -v expected_misses="$expected_misses" \
        -v expected_input_delay="$expected_input_delay" \
        -v expected_growth="$expected_growth" '
        function decimal(value) {
            return value ~ /^[0-9]+([.][0-9]+)?$/
        }
        function integer(value) {
            return value ~ /^[0-9]+$/
        }
        function same(left, right) {
            return sprintf("%.6f", left) == sprintf("%.6f", right)
        }
        function timestamp_key(value) {
            return sprintf("%.6f", value)
        }
        FILENAME == frame {
            if (FNR == 1) {
                if ($0 != "presentation_s\tframe_index") invalid = 1
                next
            }
            if (NF != 2 || !decimal($1) || !integer($2)) {
                invalid = 1
                next
            }
            if (frame_count == 0) {
                first_frame_time = $1
                previous_frame_time = $1
                previous_frame_index = $2
            } else if ($1 <= previous_frame_time || $2 <= previous_frame_index) {
                invalid = 1
            } else {
                missed += $2 - previous_frame_index - 1
            }
            last_frame_time = $1
            frame_time_key[timestamp_key($1)] = 1
            previous_frame_time = $1
            previous_frame_index = $2
            frame_count++
            next
        }
        FILENAME == input {
            if (FNR == 1) {
                if ($0 != "input_id\tinput_s") invalid = 1
                next
            }
            if (NF != 2 || !integer($1) || !decimal($2) || ($1 in input_time)) {
                invalid = 1
                next
            }
            if (input_count > 0 && $2 <= previous_input_time) invalid = 1
            input_order[++input_count] = $1
            input_position[$1] = input_count
            input_time[$1] = $2
            previous_input_time = $2
            next
        }
        FILENAME == latency {
            if (FNR == 1) {
                if ($0 != "input_id\tpresented_s\tlatency_ms") invalid = 1
                next
            }
            if (NF != 3 || !integer($1) || !decimal($2) || !decimal($3) ||
                !($1 in input_time) || ($1 in latency_seen)) {
                invalid = 1
                next
            }
            if (latency_count + 1 != input_position[$1] ||
                (latency_count > 0 && $2 <= previous_presented_time) ||
                $2 < first_frame_time || $2 > last_frame_time ||
                !(timestamp_key($2) in frame_time_key) ||
                $2 < input_time[$1] ||
                !same($3, ($2 - input_time[$1]) * 1000)) {
                invalid = 1
            }
            latency_seen[$1] = 1
            latency_count++
            latency_sum += $3
            if (latency_count == 1) first_latency = $3
            last_latency = $3
            previous_presented_time = $2
            next
        }
        END {
            for (id in input_time) {
                if (input_time[id] < first_frame_time || input_time[id] > last_frame_time) invalid = 1
            }
            if (frame_count < 2 || input_count < 2 || latency_count != input_count || invalid) exit 1
            interval = last_frame_time - first_frame_time
            fps = (frame_count - 1) / interval
            input_delay = latency_sum / latency_count
            growth = last_latency - first_latency
            if (interval <= 0 || !same(interval, expected_interval) ||
                !same(fps, expected_fps) || missed != expected_misses ||
                !same(input_delay, expected_input_delay) ||
                !same(growth, expected_growth)) exit 1
            exit 0
        }
    ' "$directory/frame-trace" "$directory/input-trace" "$directory/latency-trace"
}
