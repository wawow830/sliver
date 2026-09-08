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

panel_drm_connected_dsi_connector() {
    local node=$1 sysfs_root=${2:-/sys/class/drm} status connector
    local -a connectors=()
    [[ "$node" =~ ^/dev/dri/card[0-9]+$ ]] || return 1
    for status in "$sysfs_root/${node##*/}"-DSI-*/status; do
        [[ -r "$status" ]] && [[ "$(<"$status")" == connected ]] || continue
        connector=${status%/status}
        connector=${connector##*/}
        connectors+=("$connector")
    done
    (( ${#connectors[@]} == 1 )) || return 1
    printf '%s\n' "${connectors[0]}"
}

panel_drm_node_has_connected_dsi() {
    local node=$1 sysfs_root=${2:-/sys/class/drm}
    panel_drm_connected_dsi_connector "$node" "$sysfs_root" >/dev/null
}

# drm_info does not expose the connector's kernel name, so the caller records
# the exact sysfs identity next to the privileged probe. The probe itself must
# then prove the DSI connector, preferred native mode, active CRTC, and the
# portrait scanout geometry used by the 90-degree logical display transform.
panel_drm_probe_proves_geometry() {
    local info_file=$1 expected_node=$2 expected_connector=$3
    [[ -r "$info_file" ]] || return 1
    [[ "$expected_node" =~ ^/dev/dri/card[0-9]+$ ]] || return 1
    [[ "$expected_connector" =~ ^card[0-9]+-DSI-[0-9]+$ ]] || return 1
    grep -Fx "DRM evidence node: $expected_node" "$info_file" >/dev/null || return 1
    grep -Fx "DRM evidence connector: $expected_connector" "$info_file" >/dev/null || return 1
    grep -Fx 'DRM evidence status: connected' "$info_file" >/dev/null || return 1
    grep -Fx 'DRM evidence mode: 60x2008' "$info_file" >/dev/null || return 1
    grep -E '^DRM evidence command: sudo drm_info([[:space:]]|$)' \
        "$info_file" >/dev/null || return 1

    awk -v expected_node="$expected_node" '
        function mode_line(line) {
            return line ~ /60(×|x)2008@60(\.0+)?[[:space:]]+preferred[[:space:]]+driver([[:space:]]|$)/
        }
        function finish_connector() {
            if (in_connector && connector_dsi && connector_connected && connector_mode)
                valid_connectors++
            in_connector = connector_dsi = connector_connected = connector_mode = 0
        }
        function finish_plane() {
            if (in_plane && plane_fb && plane_crtc_w && plane_crtc_h &&
                plane_src_w && plane_src_h)
                valid_planes++
            in_plane = plane_fb = plane_crtc_w = plane_crtc_h = 0
            plane_src_w = plane_src_h = 0
        }
        BEGIN {
            node_count = other_nodes = 0
            section = ""
            in_connector = 0
            in_plane = 0
            valid_connectors = valid_planes = 0
            crtc_mode = active_crtc = 0
        }
        /^Node: \/dev\/dri\/card[0-9]+$/ {
            finish_connector()
            finish_plane()
            if ($0 == "Node: " expected_node) {
                node_count++
                in_node = 1
            } else {
                other_nodes++
                in_node = 0
            }
            section = ""
            next
        }
        !in_node { next }
        /├───Connectors$/ {
            finish_connector()
            section = "connectors"
            next
        }
        /├───CRTCs$/ {
            finish_connector()
            section = "crtcs"
            next
        }
        /└───Planes$/ {
            finish_connector()
            finish_plane()
            section = "planes"
            next
        }
        section == "connectors" && /Connector [0-9]+[[:space:]]*$/ {
            finish_connector()
            in_connector = 1
            next
        }
        section == "connectors" && in_connector && /Type: DSI[[:space:]]*$/ {
            connector_dsi = 1
            next
        }
        section == "connectors" && in_connector && /Status: connected[[:space:]]*$/ {
            connector_connected = 1
            next
        }
        section == "connectors" && in_connector && mode_line($0) {
            connector_mode = 1
            next
        }
        section == "crtcs" && /Mode: / && mode_line($0) {
            crtc_mode = 1
            next
        }
        section == "crtcs" && /"ACTIVE"/ && /=[[:space:]]*1[[:space:]]*$/ {
            active_crtc = 1
            next
        }
        section == "planes" && /Plane [0-9]+[[:space:]]*$/ {
            finish_plane()
            in_plane = 1
            next
        }
        section == "planes" && in_plane && /Size: 64(×|x)2048[[:space:]]*$/ {
            plane_fb = 1
            next
        }
        section == "planes" && in_plane && /"CRTC_W"/ && /=[[:space:]]*60[[:space:]]*$/ {
            plane_crtc_w = 1
            next
        }
        section == "planes" && in_plane && /"CRTC_H"/ && /=[[:space:]]*2008[[:space:]]*$/ {
            plane_crtc_h = 1
            next
        }
        section == "planes" && in_plane && /"SRC_W"/ && /=[[:space:]]*60[[:space:]]*$/ {
            plane_src_w = 1
            next
        }
        section == "planes" && in_plane && /"SRC_H"/ && /=[[:space:]]*2008[[:space:]]*$/ {
            plane_src_h = 1
            next
        }
        END {
            finish_connector()
            finish_plane()
            exit !(node_count == 1 && other_nodes == 0 && valid_connectors == 1 &&
                crtc_mode && active_crtc && valid_planes == 1)
        }
    ' "$info_file"
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
