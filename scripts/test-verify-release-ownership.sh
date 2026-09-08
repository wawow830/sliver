#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source "$root/scripts/verify-release-ownership.sh"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/sliver-verify-ownership.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT

cat > "$tmp/drm-info.txt" <<'EOF'
Node: /dev/dri/card1
├───Connectors
│   └───Connector 0
│       ├───Type: DSI
│       ├───Status: connected
Node: /dev/dri/card3
├───Connectors
│   └───Connector 0
│       ├───Type: eDP
│       ├───Status: connected
EOF

[[ "$(panel_drm_node_from_info "$tmp/drm-info.txt")" == /dev/dri/card1 ]] || {
    printf 'ownership test failed: did not identify the connected DSI node\n' >&2
    exit 1
}

mkdir -p "$tmp/sys/card1-DSI-1"
printf 'connected\n' > "$tmp/sys/card1-DSI-1/status"
[[ "$(panel_drm_connected_dsi_connector /dev/dri/card1 "$tmp/sys")" == card1-DSI-1 ]] || {
    printf 'ownership test failed: connected DSI connector identity was not captured\n' >&2
    exit 1
}
panel_drm_node_has_connected_dsi /dev/dri/card1 "$tmp/sys" || {
    printf 'ownership test failed: connected DSI status was rejected\n' >&2
    exit 1
}
printf 'disconnected\n' > "$tmp/sys/card1-DSI-1/status"
if panel_drm_connected_dsi_connector /dev/dri/card1 "$tmp/sys"; then
    printf 'ownership test failed: disconnected DSI connector identity was accepted\n' >&2
    exit 1
fi
if panel_drm_node_has_connected_dsi /dev/dri/card1 "$tmp/sys"; then
    printf 'ownership test failed: disconnected DSI status was accepted\n' >&2
    exit 1
fi

cat > "$tmp/drm-card2.txt" <<'EOF'
DRM evidence node: /dev/dri/card2
DRM evidence connector: card2-DSI-1
DRM evidence status: connected
DRM evidence mode: 60x2008
DRM evidence command: sudo drm_info /dev/dri/card2
Node: /dev/dri/card2
├───Framebuffer size
│   ├───Width: [32, 64]
│   └───Height: [32, 2048]
├───Connectors
│   └───Connector 0
│       ├───Type: DSI
│       ├───Status: connected
│       └───Modes
│           └───60×2008@60.00 preferred driver phsync nvsync
├───CRTCs
│   └───CRTC 0
│       ├───Legacy info
│       │   └───Mode: 60×2008@60.00 preferred driver phsync nvsync
│       └───Properties
│           └───"ACTIVE" (atomic): range [0, 1] = 1
└───Planes
    └───Plane 0
        ├───Legacy info
        │   └───FB ID: 38
        │       └───Size: 64×2048
        └───Properties
            ├───"CRTC_W" (atomic): range [0, INT32_MAX] = 60
            ├───"CRTC_H" (atomic): range [0, INT32_MAX] = 2008
            ├───"SRC_W" (atomic): range [0, UINT32_MAX] = 60
            └───"SRC_H" (atomic): range [0, UINT32_MAX] = 2008
EOF

panel_drm_probe_proves_geometry "$tmp/drm-card2.txt" /dev/dri/card2 card2-DSI-1 || {
    printf 'ownership test failed: exact privileged DRM geometry evidence was rejected\n' >&2
    exit 1
}
sed 's/CRTC_H.*2008/CRTC_H (atomic): = 2007/' "$tmp/drm-card2.txt" > "$tmp/drm-wrong-geometry.txt"
if panel_drm_probe_proves_geometry "$tmp/drm-wrong-geometry.txt" /dev/dri/card2 card2-DSI-1; then
    printf 'ownership test failed: wrong plane geometry was accepted\n' >&2
    exit 1
fi
sed 's/DRM evidence command: sudo drm_info/DRM evidence command: drm_info/' \
    "$tmp/drm-card2.txt" > "$tmp/drm-unprivileged.txt"
if panel_drm_probe_proves_geometry "$tmp/drm-unprivileged.txt" /dev/dri/card2 card2-DSI-1; then
    printf 'ownership test failed: unprivileged DRM evidence was accepted\n' >&2
    exit 1
fi

cat > "$tmp/fuser-valid.txt" <<'EOF'
                     USER        PID ACCESS COMMAND
/dev/dri/card1:      root       1024 F.... tiny-dfr
EOF

cat > "$tmp/fuser-wrong-node.txt" <<'EOF'
                     USER        PID ACCESS COMMAND
/dev/dri/card3:      root       1024 F.... tiny-dfr
EOF

cat > "$tmp/fuser-wrong-process.txt" <<'EOF'
                     USER        PID ACCESS COMMAND
/dev/dri/card1:      root       1024 F.... other-daemon
EOF

cat > "$tmp/fuser-competing-owner.txt" <<'EOF'
                     USER        PID ACCESS COMMAND
/dev/dri/card1:      root       1024 F.... tiny-dfr
                     user       2048 F.... competing-daemon
EOF

owner_matches /dev/dri/card1 1024 "$tmp/fuser-valid.txt" || {
    printf 'ownership test failed: valid exact-node owner evidence was rejected\n' >&2
    exit 1
}
if owner_matches /dev/dri/card1 1024 "$tmp/fuser-wrong-node.txt"; then
    printf 'ownership test failed: evidence for another DRM node was accepted\n' >&2
    exit 1
fi
if owner_matches /dev/dri/card1 1024 "$tmp/fuser-wrong-process.txt"; then
    printf 'ownership test failed: evidence for another process was accepted\n' >&2
    exit 1
fi
if owner_matches /dev/dri/card1 1024 "$tmp/fuser-competing-owner.txt"; then
    printf 'ownership test failed: competing DRM owner was accepted\n' >&2
    exit 1
fi

# Stop the real preflight at tool selection. No hardware probe or transaction
# can run in these subprocesses. modetest cannot feed the drm_info parser.
source <(awk '
    /^preflight_stage\(\) \{/ { copying = 1 }
    copying { print }
    copying && /^}$/ { copying = 0 }
' "$root/scripts/verify-release.sh")
probe_tool_selection() (
    stage() { :; }
    say() { :; }
    command() { [[ "$*" != '-v drm_info' || "$drm_info_available" == yes ]]; }
    pass_check() { [[ "$1" != drm_tool ]] || exit 0; }
    fail_check() { exit 42; }
    preflight_stage
    exit 99
)
# Use a separate variable because command() receives its own positional args.
drm_info_available=no
set +e
probe_tool_selection
status=$?
set -e
[[ "$status" == 42 ]] || {
    printf 'ownership test failed: preflight accepted modetest without a supported parser\n' >&2
    exit 1
}
drm_info_available=yes
probe_tool_selection || {
    printf 'ownership test failed: preflight rejected drm_info\n' >&2
    exit 1
}

printf 'verify-release ownership tests passed\n'
