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
grep -F 'sudo drm_info "$PANEL_DRM_NODE"' "$script" >/dev/null ||
    fail 'stage 1 does not capture privileged DRM evidence for the exact panel node'
grep -F 'panel_drm_probe_proves_geometry' "$script" >/dev/null ||
    fail 'stage 1 does not objectively verify native mode and scanout geometry'
grep -F 'logged_step drm_preflight "$VERIFY_DIR/drm-before.txt" capture_drm_preflight' "$script" >/dev/null ||
    fail 'stage 1 does not save the privileged exact-node DRM probe'
grep -F 'DRM evidence transform: logical 2008x60 -> scanout 60x2008 (quarter-turn)' "$script" >/dev/null ||
    fail 'stage 1 does not record the required logical-to-scanout transform'
grep -F 'privileged DRM evidence does not prove the exact connected native mode and scanout geometry' "$script" >/dev/null ||
    fail 'stage 1 does not fail closed when DRM geometry evidence is incomplete'
grep -F 'PANEL_DRM_SYSFS_DEVICE' "$script" >/dev/null ||
    fail 'stage 6 does not verify stable DRM device identity'
grep -F 'pre_takeover_drm_identity' "$script" >/dev/null ||
    fail 'stage 6 does not record stable DRM device identity'
grep -F 'takeover_drm_owner' "$script" >/dev/null ||
    fail 'takeover does not revalidate DRM ownership immediately before stopping tiny-dfr'
grep -F 'rollback_drm_owner_verified' "$script" >/dev/null ||
    fail 'rollback does not verify restored DRM ownership'
grep -F 'panel DRM identity capture was incomplete' "$script" >/dev/null ||
    fail 'rollback treats incomplete panel identity as not applicable'
grep -F 'panel_drm_identity_matches_snapshot || return 1' "$script" >/dev/null ||
    fail 'immediate takeover revalidation skips panel identity'

grep -F 'record_check' "$script" >/dev/null || fail 'checks are not recorded individually'
grep -F 'restore_and_verify' "$script" >/dev/null || fail 'rollback does not verify restoration'
grep -F 'HANDOFF_PENDING' "$script" >/dev/null || fail 'logout handoff is not resumable'
grep -F 'valid.lua' "$script" >/dev/null || fail 'valid fixture is not named'
grep -F 'invalid.lua' "$script" >/dev/null || fail 'invalid fixture is not named'
grep -F 'hung.lua' "$script" >/dev/null || fail 'watchdog fixture is not named'
grep -F 'two-second physical Fn hold' "$script" >/dev/null || fail 'Fn recovery prompt does not name its two-second deadline'
grep -F 'two-second Lua callback watchdog' "$script" >/dev/null || fail 'watchdog prompt does not distinguish its deadline'
if grep -F 'three-second Fn hold' "$script" >/dev/null; then
    fail 'verifier still describes a three-second Fn recovery hold'
fi
grep -F 'video-2008x60.lua' "$script" >/dev/null || fail 'video workload is not named'
grep -F 'refuse_if_blocked' "$script" >/dev/null || fail 'failure gate is missing'
grep -F 'source "$ROOT/scripts/verify-release-auth.sh"' "$script" >/dev/null ||
    fail 'verifier does not load authentication handling'
grep -F 'verify_release_capture_privileged "$output" capture_pre_takeover_drm_owner' "$script" >/dev/null ||
    fail 'stage 6 does not classify privileged capture failures'
grep -F 'verify_release_require_authentication "$VERIFY_DIR/sudo-auth-before-takeover.txt"' "$script" >/dev/null ||
    fail 'stage 6 does not acquire administrator authentication deliberately'
grep -F 'owner_status == VERIFY_RELEASE_AUTH_REQUIRED' "$script" >/dev/null ||
    fail 'stage 6 does not keep authentication timeouts resumable'
grep -F 'ROLLBACK_ATTEMPTED' "$script" >/dev/null ||
    fail 'rollback attempt state is not persisted'

# Authentication failures before an objective check must be retryable. A real
# ownership failure must still return an ordinary failure status, and a second
# automatic rollback must not be attempted after the first one was recorded.
# shellcheck disable=SC1091
source "$root/scripts/verify-release-auth.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/sliver-verify-release-auth.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT
cat > "$tmp/auth-timeout" <<'EOF'
sudo: timed out reading password
sudo: a password is required
EOF
cat > "$tmp/ownership-failure" <<'EOF'
fuser: cannot open /dev/dri/card2
EOF
verify_release_authentication_error "$tmp/auth-timeout" || fail 'sudo authentication timeout was not classified as retryable'
if verify_release_authentication_error "$tmp/ownership-failure"; then
    fail 'ordinary ownership command failure was classified as authentication'
fi
set +e
verify_release_capture_privileged "$tmp/capture-timeout" bash -c 'cat "$1"; exit 1' _ "$tmp/auth-timeout"
status=$?
set -e
[[ "$status" == "$VERIFY_RELEASE_AUTH_REQUIRED" ]] || fail "authentication capture returned $status, expected $VERIFY_RELEASE_AUTH_REQUIRED"
set +e
verify_release_capture_privileged "$tmp/capture-failure" bash -c 'cat "$1"; exit 1' _ "$tmp/ownership-failure"
status=$?
set -e
[[ "$status" == 1 ]] || fail "ownership capture returned $status, expected ordinary failure"
set +e
verify_release_capture_privileged "$tmp/capture-status-75" bash -c 'exit 75'
status=$?
set -e
[[ "$status" == 1 ]] || fail "unclassified status-75 capture returned $status, expected ordinary failure"
cat > "$tmp/sudo" <<'EOF'
#!/usr/bin/env bash
printf 'sudo: timed out reading password\n'
printf 'sudo: a password is required\n'
exit 1
EOF
chmod 700 "$tmp/sudo"
set +e
PATH="$tmp:$PATH" verify_release_require_authentication "$tmp/auth-gate-timeout"
status=$?
set -e
[[ "$status" == "$VERIFY_RELEASE_AUTH_REQUIRED" ]] || fail "authentication gate returned $status, expected $VERIFY_RELEASE_AUTH_REQUIRED"
cat > "$tmp/sudo" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod 700 "$tmp/sudo"
PATH="$tmp:$PATH" verify_release_require_authentication "$tmp/auth-gate-success" ||
    fail 'successful sudo authentication was rejected'
if verify_release_should_auto_rollback 1 1 0 0 1 0; then
    fail 'authentication pause still schedules automatic rollback'
fi
if verify_release_should_auto_rollback 1 0 1 0 1 0; then
    fail 'previous rollback attempt still schedules duplicate automatic rollback'
fi
verify_release_should_auto_rollback 1 0 0 0 1 0 || fail 'ordinary failure did not schedule rollback'

echo 'verify-release contract tests passed'
