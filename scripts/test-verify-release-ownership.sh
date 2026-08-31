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

printf 'verify-release ownership tests passed\n'
