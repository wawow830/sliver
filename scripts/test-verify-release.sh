#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
script="$root/scripts/verify-release.sh"

fail() {
    printf 'verify-release test failed: %s\n' "$*" >&2
    exit 1
}

bash -n "$script" || fail 'verifier is not valid bash'
help=$($script --help)
grep -F -- '--resume DIR' <<<"$help" >/dev/null || fail 'help omits --resume'
grep -F -- '--rollback DIR' <<<"$help" >/dev/null || fail 'help omits --rollback'
grep -F -- '--manifest' <<<"$help" >/dev/null || fail 'help omits --manifest'

manifest=$($script --manifest)
[[ -n "$manifest" ]] || fail 'manifest is empty'
grep -Fx '/usr/bin/sliver' <<<"$manifest" >/dev/null || fail 'manifest omits public client'
grep -Fx '/usr/libexec/sliver/sliver-lua-worker' <<<"$manifest" >/dev/null || fail 'manifest omits packaged worker'
grep -Fx '/usr/share/doc/sliver/release-commit' <<<"$manifest" >/dev/null || fail 'manifest omits release identity'

if grep -F 'run_optional id sliver' "$script" >/dev/null; then
    fail 'missing fresh-install account is still treated as a blocker'
fi
if grep -F 'run_optional pgrep' "$script" >/dev/null; then
    fail 'missing pre-install process is still treated as a blocker'
fi
if grep -E '^[[:space:]]*sudo systemctl restart sliver-broker.service' "$script" >/dev/null; then
    fail 'service restart bypasses the privileged-step gate'
fi
if grep -F 'if (( TAKEOVER_ACTIVE )); then' "$script" >/dev/null; then
    fail 'rollback is still limited to takeover-active state'
fi
"$root/scripts/test-verify-release-ownership.sh" || fail 'ownership regression tests failed'
grep -F 'PANEL_DRM_NODE' "$script" >/dev/null || fail 'preflight does not retain the exact panel DRM node'
grep -F 'sudo fuser -v "$PANEL_DRM_NODE"' "$script" >/dev/null ||
    fail 'stage 6 does not inspect the exact panel DRM node with privilege'
grep -F 'pre_takeover_drm_owner' "$script" >/dev/null ||
    fail 'stage 6 does not record machine-verifiable DRM ownership'
grep -F 'PANEL_DRM_CONNECTOR' "$script" >/dev/null ||
    fail 'stage 6 does not retain the exact DSI connector identity'
grep -F 'PANEL_DRM_SYSFS_DEVICE' "$script" >/dev/null ||
    fail 'stage 6 does not verify stable DRM device identity'
grep -F 'pre_takeover_drm_identity' "$script" >/dev/null ||
    fail 'stage 6 does not record stable DRM device identity'
grep -F 'takeover_drm_owner' "$script" >/dev/null ||
    fail 'takeover does not revalidate DRM ownership immediately before stopping tiny-dfr'
grep -F 'rollback_drm_owner_verified' "$script" >/dev/null ||
    fail 'rollback does not verify restored DRM ownership'
grep -F 'panel_drm_identity_matches_snapshot || return 1' "$script" >/dev/null ||
    fail 'immediate takeover revalidation skips panel identity'

grep -F 'record_check' "$script" >/dev/null || fail 'checks are not recorded individually'
grep -F 'restore_and_verify' "$script" >/dev/null || fail 'rollback does not verify restoration'
grep -F 'HANDOFF_PENDING' "$script" >/dev/null || fail 'logout handoff is not resumable'
grep -F 'valid.lua' "$script" >/dev/null || fail 'valid fixture is not named'
grep -F 'invalid.lua' "$script" >/dev/null || fail 'invalid fixture is not named'
grep -F 'hung.lua' "$script" >/dev/null || fail 'watchdog fixture is not named'
grep -F 'video-2008x60.lua' "$script" >/dev/null || fail 'video workload is not named'
grep -F 'refuse_if_blocked' "$script" >/dev/null || fail 'failure gate is missing'

echo 'verify-release contract tests passed'
