#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT=$root
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
fail() { printf 'input rollback test failed: %s\n' "$*" >&2; exit 1; }

# Exercise the real rollback sequence, not a list of expected commands.
# Hardware roots are private; privilege and daemon commands never reach the host.
definitions=$(<"$root/scripts/verify-release-ownership.sh")
definitions+=$'\n'
definitions+=$(awk '
    /^(restore_and_verify|rollback_privileged|restore_unit|rollback_user|verify_service_restore|unit_active|unit_enabled|unit_load|user_unit_active|user_unit_enabled|user_unit_load|global_unit_enabled|record_check|panel_drm_identity_matches_snapshot|rebind_panel_drm_at_boundary|capture_drm_owner_to_file|capture_pre_takeover_drm_owner|verify_tiny_dfr_drm_owner|capture_rollback_input_nodes|capture_rollback_input_access)\(\) \{/ { copying=1 }
    copying { print }
    copying && /^}$/ { copying=0 }
' "$root/scripts/verify-release.sh")
definitions=${definitions//\/sys\/class\/drm/$tmp/sys}
definitions=${definitions//\/dev\/dri/$tmp/dri}
escaped_root=${tmp//\//\\/}; pattern='\/dev\/dri'
definitions=${definitions//"$pattern"/"$escaped_root\\/dri"}
source /dev/stdin <<< "$definitions"
source "$root/scripts/verify-release-auth.sh"
save_state() { :; }
say() { printf '%s\n' "$*" >> "$VERIFY_DIR/messages"; }
restore_failure_state() { :; }
failure_state_matches_snapshot() { :; }
stat() { command stat -L "$@"; }
systemctl() {
    case "$*" in
        'show tiny-dfr.service -p MainPID --value') printf '%s\n' "$daemon_pid" ;;
        'is-active tiny-dfr.service') printf 'active\n' ;;
        'is-enabled tiny-dfr.service') printf 'enabled\n' ;;
        *is-active*) printf 'inactive\n' ;;
        *is-enabled*) printf 'disabled\n' ;;
        *show*) printf 'not-found\n' ;;
        *) fail "unexpected systemctl: $*" ;;
    esac
}
rpm() { (( package_present )); }
getent() {
    if [[ "$*" == 'group sliver-input' ]] && (( group_present )); then
        printf 'sliver-input:x:977:\n'
    else
        return 2
    fi
}
sudo() {
    case "$*" in
        'systemctl start tiny-dfr.service'|'systemctl restart tiny-dfr.service')
            if [[ "$*" == 'systemctl restart tiny-dfr.service' && "$scenario" == restart_failed ]]; then return 1; fi
            daemon_pid=$((daemon_pid + 1))
            if (( node_gid == 104 )); then inputs_open=1; else inputs_open=0; fi ;;
        'dnf remove -y --no-autoremove sliver') package_present=0 ;;
        'udevadm control --reload-rules') : ;;
        "python3 $root/scripts/verify-release-input.py nodes") printf '/dev/input/event7\n/dev/input/event12\n' ;;
        "python3 $root/scripts/verify-release-input.py check --pid $daemon_pid")
            input_checks=$((input_checks + 1))
            printf '{"gid":%s,"opened":%s,"scenario":"%s"}\n' "$node_gid" "$inputs_open" "$scenario"
            if [[ "$scenario" == missing_keyboard_fd || "$scenario" == missing_touchbar_fd ]]; then return 1; fi
            if [[ "$scenario" == changed_pid ]]; then daemon_pid=$((daemon_pid + 1)); fi
            (( node_gid == 104 && inputs_open == 1 )) ;;
        udevadm\ trigger*)
            # Default change retains udev DB permissions. Add requests rule
            # defaults again, but those changes are asynchronous until settle.
            if [[ " $* " == *' --action=add '* && " $* " == *' --subsystem-match=input '* ]] &&
                (( package_present == 0 )); then pending_gid=104; fi ;;
        udevadm\ settle*)
            [[ "$scenario" != settle_failed ]] || return 1
            if [[ "$scenario" != stale_permissions ]]; then node_gid=$pending_gid; fi ;;
        'groupdel sliver-input') group_present=0 ;;
        "fuser -v $tmp/dri/card2")
            if [[ "$scenario" == early_wrong_owner ]] ||
                { [[ "$scenario" == final_wrong_owner ]] && (( daemon_pid > 1018 )); }; then
                printf '%s: nobody 999 F.... tiny-dfr\n' "$tmp/dri/card2"
            else
                printf '%s: nobody %s F.... tiny-dfr\n' "$tmp/dri/card2" "$daemon_pid"
                if [[ "$scenario" == final_competitor ]] && (( daemon_pid > 1018 )); then
                    printf 'nobody 999 F.... competitor\n'
                fi
            fi ;;
        *) fail "unexpected sudo: $*" ;;
    esac
}

setup_case() {
mkdir -p "$tmp/sys/card2" "$tmp/sys/card2-DSI-1" "$tmp/dri" "$tmp/panel"
ln -sfn "$tmp/panel" "$tmp/sys/card2/device"
ln -sfn /dev/null "$tmp/dri/card2"
printf 'connected\n' > "$tmp/sys/card2-DSI-1/status"
VERIFY_DIR="$tmp/$scenario"; mkdir -p "$VERIFY_DIR"
EVIDENCE_FILE="$VERIFY_DIR/evidence.tsv"; STATE_FILE="$VERIFY_DIR/state.env"
printf 'before\tperformance_measurement\tfail\toriginal hardware failure\n' > "$EVIDENCE_FILE"
PANEL_DRM_NODE="$tmp/dri/card2"; PANEL_DRM_CONNECTOR=card2-DSI-1
PANEL_DRM_SYSFS_DEVICE="$tmp/panel"; PANEL_DRM_DEV_MAJOR_MINOR=1:3
ORIGINAL_TINY_ACTIVE=active; ORIGINAL_TINY_ENABLED=enabled
ORIGINAL_BROKER_LOAD=not-found; ORIGINAL_USER_SUPERVISOR_LOAD=not-found
ORIGINAL_GLOBAL_SUPERVISOR_ENABLED=not-found
USER_SUPERVISOR_CHANGED=0; GLOBAL_SUPERVISOR_CHANGED=0; BROKER_CHANGED=0
TAKEOVER_ATTEMPTED=0; TINY_STOP_ATTEMPTED=1; USER_GROUP_CHANGED=0; LINGER_CHANGED=0
PACKAGE_INSTALLED_BY_RUN=1; UDEV_CHANGED=1; SELECTED_PATH_CHANGED=0
ORIGINAL_SLIVER_USER_PRESENT=0; ORIGINAL_GROUP_SUPERVISORS_PRESENT=0
ORIGINAL_GROUP_DRM_PRESENT=0; ORIGINAL_GROUP_INPUT_PRESENT=0; ORIGINAL_GROUP_BACKLIGHT_PRESENT=0
package_present=1; group_present=1; node_gid=977; pending_gid=977; inputs_open=0; daemon_pid=1017
ROLLBACK_DONE=0; input_checks=0
if [[ "$scenario" == before_forward_trigger ]]; then UDEV_CHANGED=0; fi
}
for scenario in restored before_forward_trigger; do
    setup_case
    restore_and_verify full || fail "$scenario: full rollback did not finish"
    [[ "$node_gid" == 104 && "$inputs_open" == 1 ]] ||
        fail "rollback reported success with input gid=$node_gid opened=$inputs_open (expected input-group access and both devices reopened)"
    [[ "$input_checks" == 1 && "$ROLLBACK_DONE" == 1 ]] || fail 'rollback did not execute the final input inspection'
    for check in rollback_input_access_verified rollback_final_drm_owner_verified; do
        grep -F "$check"$'\tpass\t' "$EVIDENCE_FILE" >/dev/null || fail "missing affirmative $check"
    done
    grep -F $'performance_measurement\tfail\toriginal hardware failure' "$EVIDENCE_FILE" >/dev/null || fail 'forward failure evidence erased'
    grep -F 'Physical check still required:' "$VERIFY_DIR/messages" >/dev/null || fail 'physical Fn check is buried in the ledger'
done
for scenario in stale_permissions missing_keyboard_fd missing_touchbar_fd settle_failed restart_failed final_wrong_owner final_competitor changed_pid early_wrong_owner; do
    setup_case
    if restore_and_verify full; then fail "$scenario: invalid final state reported successful rollback"; fi
    [[ "$ROLLBACK_DONE" == 0 ]] || fail "$scenario: failed restoration marked rollback done"
    grep -F $'rollback_verified\tfail\t' "$EVIDENCE_FILE" >/dev/null || fail "$scenario: failure evidence missing"
    if [[ "$scenario" == early_wrong_owner ]]; then
        [[ "$package_present" == 1 && "$input_checks" == 0 && "$node_gid" == 977 ]] || fail 'early DRM gate allowed removal/input changes'
    fi
    if [[ "$scenario" == missing_keyboard_fd || "$scenario" == missing_touchbar_fd || "$scenario" == stale_permissions ]]; then
        grep -F $'rollback_input_access_verified\tfail\t' "$EVIDENCE_FILE" >/dev/null || fail 'input failure not recorded'
        [[ "$input_checks" == 1 ]] || fail 'input regression missed the final rollback call site'
        compgen -G "$VERIFY_DIR/input-after-rollback.*.jsonl" >/dev/null || fail 'raw failed input evidence missing'
    fi
done
printf 'verify-release input rollback tests passed\n'
