#!/usr/bin/env bash

panel_drm_node_from_info() {
    local info_file=$1 line node="" connector_dsi=0
    local -a nodes=()
    while IFS= read -r line; do
        if [[ "$line" =~ ^Node:\ (/dev/dri/card[0-9]+)$ ]]; then
            node=${BASH_REMATCH[1]}
            connector_dsi=0
        elif [[ "$line" == *Connector\ * ]]; then
            connector_dsi=0
        elif [[ "$line" == *"Type: DSI"* ]]; then
            connector_dsi=1
        elif [[ "$line" == *"Status: connected"* && "$connector_dsi" == 1 ]]; then
            if [[ ! " ${nodes[*]} " == *" $node "* ]]; then
                nodes+=("$node")
            fi
            connector_dsi=0
        fi
    done < "$info_file"
    (( ${#nodes[@]} == 1 )) || return 1
    printf '%s\n' "${nodes[0]}"
}

panel_drm_node_from_sysfs() {
    local status card node
    local -a nodes=()
    for status in /sys/class/drm/card*-DSI-*/status; do
        [[ -r "$status" ]] && [[ "$(<"$status")" == connected ]] || continue
        card=${status#/sys/class/drm/}
        card=${card%%-*}
        node=/dev/dri/$card
        [[ -c "$node" ]] || continue
        if [[ ! " ${nodes[*]} " == *" $node "* ]]; then
            nodes+=("$node")
        fi
    done
    (( ${#nodes[@]} == 1 )) || return 1
    printf '%s\n' "${nodes[0]}"
}

owner_matches() {
    local node=$1 expected_pid=$2 evidence_file=$3
    [[ "$node" =~ ^/dev/dri/card[0-9]+$ ]] || return 1
    [[ "$expected_pid" =~ ^[1-9][0-9]*$ ]] || return 1
    awk -v expected_node="$node" -v expected_pid="$expected_pid" '
        {
            observed_node = $1
            sub(/:$/, "", observed_node)
            if (observed_node != expected_node) next
            owner_lines++
            pid_found = 0
            for (i = 2; i <= NF; i++) {
                if ($i == expected_pid) pid_found = 1
            }
            command = $NF
            sub(/^.*\//, "", command)
            if (pid_found && command == "tiny-dfr") expected_owner++
        }
        END {
            exit !(owner_lines == 1 && expected_owner == 1)
        }
    ' "$evidence_file"
}
