#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
fail() { printf 'DRM rebinding test failed: %s\n' "$*" >&2; exit 1; }

# Load real functions only; replace the two hardware path roots throughout
# their definitions (including validation patterns). Character-device checks
# still run for real, against private symlinks to /dev/null and /dev/zero.
load_functions() {
    local definitions
    definitions=$(<"$root/scripts/verify-release-ownership.sh")
    definitions+=$'\n'
    definitions+=$(awk '
        /^(owner_stage|takeover_stage|panel_drm_identity_matches_snapshot|rebind_panel_drm_at_boundary|capture_pre_takeover_drm_owner|capture_drm_owner_to_file|verify_tiny_dfr_drm_owner|restore_and_verify|rollback_privileged|record_check|pass_check|fail_check|refuse_if_blocked)\(\) \{/ { copying = 1 }
        copying { print }
        copying && /^}$/ { copying = 0 }
    ' "$root/scripts/verify-release.sh")
    definitions=${definitions//\/sys\/class\/drm/$tmp/sys}
    definitions=${definitions//\/dev\/dri/$tmp/dri}
    local escaped_root=${tmp//\//\\/} pattern='\/dev\/dri'
    local replacement="$escaped_root\\/dri"
    definitions=${definitions//"$pattern"/"$replacement"}
    definitions=${definitions//\/dev\/input/$tmp/input}
    definitions=${definitions//\/dev\/uinput/$tmp/uinput}
    source /dev/stdin <<< "$definitions"
}
load_functions

# All service/privilege effects are allowlisted doubles. Any unexpected call
# fails the test, never falls through to the host.
sudo() {
    case "$*" in
        "fuser -v $tmp/dri/card1") printf '%s: user 2000 F.... GPU-client\n' "$tmp/dri/card1" ;;
        "fuser -v $tmp/dri/card2")
            printf '%s: root %s F.... tiny-dfr\n' "$tmp/dri/card2" "${owner_pid:-1017}"
            if [[ ${competing_owner:-no} == yes ]]; then printf 'user 2048 F.... competitor\n'; fi ;;
        'dnf remove -y --no-autoremove sliver') printf 'remove-package\n' >> "$VERIFY_DIR/actions"; package_present=0 ;;
        *) fail "unexpected sudo: $*" ;;
    esac
}
systemctl() {
    [[ "$*" == 'show tiny-dfr.service -p MainPID --value' ]] || fail "unexpected systemctl: $*"
    printf '1017\n'
}
# Private symlinks stand in for real device nodes; stat observes their targets.
stat() { command stat -L "$@"; }
pgrep() { printf '1017 tiny-dfr\n'; }
verify_release_require_authentication() { :; }
verify_release_capture_privileged() { local output=$1; shift; "$@" > "$output" 2>&1; }
logged_step() { :; }
stage() { :; }
say() { :; }
warn() { :; }
confirm() { return 0; }
privileged_step() { fail 'takeover reached a mutation'; }
restore_unit() { fail 'unexpected service restoration'; }
verify_service_restore() { return 0; }
unit_active() { printf 'active\n'; }
unit_enabled() { printf 'enabled\n'; }
unit_load() { printf 'not-found\n'; }
user_unit_load() { printf 'not-found\n'; }
rpm() { (( package_present )); }
getent() { return 2; }
restore_failure_state() { :; }
failure_state_matches_snapshot() { :; }
save_state() {
    printf 'PANEL_DRM_NODE=%q\nPANEL_DRM_CONNECTOR=%q\nPANEL_DRM_SYSFS_DEVICE=%q\nPANEL_DRM_DEV_MAJOR_MINOR=%q\n' \
        "$PANEL_DRM_NODE" "$PANEL_DRM_CONNECTOR" "$PANEL_DRM_SYSFS_DEVICE" "$PANEL_DRM_DEV_MAJOR_MINOR" > "$STATE_FILE"
}
setup_case() {
    rm -rf "$tmp/sys" "$tmp/dri"
    mkdir -p "$tmp/sys/card1" "$tmp/sys/card2" "$tmp/sys/card2-DSI-1" "$tmp/dri" "$tmp/persistent/panel" "$tmp/persistent/gpu"
    ln -s "$tmp/persistent/gpu" "$tmp/sys/card1/device"
    ln -s "$tmp/persistent/panel" "$tmp/sys/card2/device"
    ln -s /dev/null "$tmp/dri/card1"
    ln -s /dev/zero "$tmp/dri/card2"
    printf 'connected\n' > "$tmp/sys/card2-DSI-1/status"
    VERIFY_DIR="$tmp/$1"; mkdir -p "$VERIFY_DIR"
    STATE_FILE="$VERIFY_DIR/state.env"; EVIDENCE_FILE="$VERIFY_DIR/evidence.tsv"
    PANEL_DRM_NODE="$tmp/dri/card1"; PANEL_DRM_CONNECTOR=card1-DSI-1
    PANEL_DRM_SYSFS_DEVICE="$tmp/persistent/panel"; PANEL_DRM_DEV_MAJOR_MINOR=1:3
    BLOCKED=0; CURRENT_STAGE=5; VERIFY_RELEASE_AUTH_REQUIRED=77
    owner_pid=1017; competing_owner=no
    ORIGINAL_TINY_ACTIVE=active; ORIGINAL_TINY_ENABLED=enabled
    USER_SUPERVISOR_CHANGED=0; GLOBAL_SUPERVISOR_CHANGED=0; BROKER_CHANGED=0
    TAKEOVER_ATTEMPTED=0; TINY_STOP_ATTEMPTED=0; USER_GROUP_CHANGED=0; LINGER_CHANGED=0
    PACKAGE_INSTALLED_BY_RUN=1; package_present=1; UDEV_CHANGED=0; SELECTED_PATH_CHANGED=0
    ORIGINAL_SLIVER_USER_PRESENT=0; ORIGINAL_GROUP_SUPERVISORS_PRESENT=0
    ORIGINAL_GROUP_DRM_PRESENT=0; ORIGINAL_GROUP_INPUT_PRESENT=0; ORIGINAL_GROUP_BACKLIGHT_PRESENT=0
    save_state
    cp "$STATE_FILE" "$VERIFY_DIR/original-state"
    printf 'original privileged preflight geometry\n' > "$VERIFY_DIR/drm-before.txt"
    printf 'before\tpre_takeover_drm_identity\tfail\toriginal run failure\n' > "$EVIDENCE_FILE"
}

assert_preserved_evidence() {
    cmp "$VERIFY_DIR/original-state" "$VERIFY_DIR/drm-preflight-state.env" || fail 'original saved binding was not preserved'
    [[ "$(<"$VERIFY_DIR/drm-before.txt")" == 'original privileged preflight geometry' ]] || fail 'preflight artifact changed'
    grep -F 'original run failure' "$EVIDENCE_FILE" >/dev/null || fail 'rebinding erased forward failure'
    awk -F '\t' -v old="$tmp/dri/card1" -v new="$tmp/dri/card2" -v device="$tmp/persistent/panel" '
        $3 == device && $4 == old && $5 == "card1-DSI-1" && $6 == "1:3" &&
        $7 == new && $8 == "card2-DSI-1" && $9 == "1:5" { found=1 }
        END { exit !found }
    ' "$VERIFY_DIR/drm-rebindings.tsv" || fail 'audit omitted old/new identity'
}

if [[ ${1:-all} != rollback ]]; then
    setup_case owner
    if ! (owner_stage); then fail 'real pre-owner boundary rejected the same panel after card1 -> card2 renumbering'; fi
    source "$STATE_FILE"
    [[ "$PANEL_DRM_NODE" == "$tmp/dri/card2" ]] || fail 'pre-owner boundary retained stale GPU node'
    panel_drm_identity_matches_snapshot || fail 'new binding does not pass strict validation'
    assert_preserved_evidence
    # Resume/rollback of an already rebound state must not overwrite the first
    # snapshot or invent another renumbering event.
    restore_and_verify full || fail 'rollback of already rebound state failed'
    assert_preserved_evidence
    [[ $(wc -l < "$VERIFY_DIR/drm-rebindings.tsv") == 1 ]] || fail 'unchanged binding produced another rebind'
fi

setup_case rollback
restore_and_verify full || fail 'real rollback rejected same persistent panel after renumbering'
[[ "$package_present" == 0 ]] || fail 'rollback never passed the package-removal owner gate'
assert_preserved_evidence

for invalid in different ambiguous disconnected non_character wrong_connector extra_connector extra_physical_connector missing_device \
    missing_node missing_connector missing_identity missing_devnum mismatched_connector; do
    setup_case "$invalid"
    case "$invalid" in
        different) ln -sfn "$tmp/persistent/gpu" "$tmp/sys/card2/device" ;;
        ambiguous)
            mkdir -p "$tmp/sys/card3" "$tmp/sys/card3-DSI-1"
            ln -s "$tmp/persistent/panel" "$tmp/sys/card3/device"
            ln -s /dev/null "$tmp/dri/card3"
            printf 'connected\n' > "$tmp/sys/card3-DSI-1/status" ;;
        extra_physical_connector)
            mkdir -p "$tmp/sys/card3" "$tmp/sys/card3-DSI-2"
            ln -s "$tmp/persistent/panel" "$tmp/sys/card3/device"
            ln -s /dev/null "$tmp/dri/card3"
            printf 'connected\n' > "$tmp/sys/card3-DSI-2/status" ;;
        disconnected) printf 'disconnected\n' > "$tmp/sys/card2-DSI-1/status" ;;
        non_character) rm "$tmp/dri/card2"; touch "$tmp/dri/card2" ;;
        wrong_connector) mv "$tmp/sys/card2-DSI-1" "$tmp/sys/card2-DSI-2" ;;
        extra_connector) mkdir "$tmp/sys/card2-DSI-2"; printf 'connected\n' > "$tmp/sys/card2-DSI-2/status" ;;
        missing_device) rm "$tmp/sys/card2/device" ;;
        missing_node) PANEL_DRM_NODE='' ;;
        missing_connector) PANEL_DRM_CONNECTOR='' ;;
        missing_identity) PANEL_DRM_SYSFS_DEVICE='' ;;
        missing_devnum) PANEL_DRM_DEV_MAJOR_MINOR='' ;;
        mismatched_connector) PANEL_DRM_CONNECTOR=card9-DSI-1 ;;
    esac
    save_state; cp "$STATE_FILE" "$VERIFY_DIR/invalid-state"
    if (owner_stage); then fail "pre-owner boundary accepted $invalid"; fi
    cmp "$STATE_FILE" "$VERIFY_DIR/invalid-state" || fail "$invalid changed working binding"
    if restore_and_verify full; then fail "rollback accepted $invalid"; fi
    [[ "$package_present" == 1 && ! -e "$VERIFY_DIR/actions" ]] || fail "$invalid bypassed cleanup gate"
    [[ ! -e "$VERIFY_DIR/drm-rebindings.tsv" ]] || fail "$invalid was recorded as a rebind"
    grep -F $'rollback_drm_owner_verified\tfail\t' "$EVIDENCE_FILE" >/dev/null || fail "$invalid rollback did not record failure"
done

for invalid_owner in wrong_pid competitor; do
    setup_case "$invalid_owner"
    if [[ "$invalid_owner" == wrong_pid ]]; then owner_pid=999; else competing_owner=yes; fi
    if restore_and_verify full; then fail "rollback accepted $invalid_owner after rebinding"; fi
    [[ "$package_present" == 1 && ! -e "$VERIFY_DIR/actions" ]] || fail "$invalid_owner bypassed owner gate"
done

# Strict takeover is deliberately NOT another rediscovery boundary. A later
# node/device-number, persistent-device, or connector change must stop it.
for changed in number identity connector; do
    setup_case "takeover-$changed"
    (owner_stage) || fail 'pre-owner setup failed'
    source "$STATE_FILE"
    case "$changed" in
        number) ln -sfn /dev/null "$tmp/dri/card2" ;;
        identity) ln -sfn "$tmp/persistent/gpu" "$tmp/sys/card2/device" ;;
        connector) mv "$tmp/sys/card2-DSI-1" "$tmp/sys/card2-DSI-2" ;;
    esac
    if (takeover_stage); then fail "takeover accepted a late $changed change"; fi
    grep -F $'takeover_drm_owner\tfail\t' "$EVIDENCE_FILE" >/dev/null || fail 'takeover did not fail at strict owner gate'
    [[ $(wc -l < "$VERIFY_DIR/drm-rebindings.tsv") == 1 ]] || fail 'takeover performed rediscovery'
done

setup_case audit_failure
mkdir "$VERIFY_DIR/drm-rebindings.tsv"
if (owner_stage) 2>/dev/null; then fail 'rebound without recording audit evidence'; fi
cmp "$STATE_FILE" "$VERIFY_DIR/original-state" || fail 'audit failure changed saved identity'

printf 'verify-release DRM rebinding tests passed\n'
