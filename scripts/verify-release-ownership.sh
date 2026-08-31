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

panel_drm_node_has_connected_dsi() {
    local node=$1 sysfs_root=${2:-/sys/class/drm} status
    [[ "$node" =~ ^/dev/dri/card[0-9]+$ ]] || return 1
    for status in "$sysfs_root/${node##*/}"-DSI-*/status; do
        [[ -r "$status" ]] && [[ "$(<"$status")" == connected ]] && return 0
    done
    return 1
}

owner_matches() {
    local node=$1 expected_pid=$2 evidence_file=$3
    [[ "$node" =~ ^/dev/dri/card[0-9]+$ ]] || return 1
    [[ "$expected_pid" =~ ^[1-9][0-9]*$ ]] || return 1
    awk -v expected_node="$node" -v expected_pid="$expected_pid" '
        function count_owner(pid, command) {
            owner_lines++
            sub(/^.*\//, "", command)
            if (pid == expected_pid && command == "tiny-dfr") expected_owner++
        }
        {
            if ($1 ~ /^\/dev\/dri\/card[0-9]+:$/) {
                current_node = $1
                sub(/:$/, "", current_node)
                if (current_node == expected_node && $3 ~ /^[0-9]+$/) {
                    count_owner($3, $NF)
                }
                next
            }
            if (current_node == expected_node && NF >= 4 && $2 ~ /^[0-9]+$/) {
                count_owner($2, $NF)
            }
        }
        END {
            exit !(owner_lines == 1 && expected_owner == 1)
        }
    ' "$evidence_file"
}
