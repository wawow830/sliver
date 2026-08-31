#!/usr/bin/env bash
set -Eeuo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
MANIFEST="$ROOT/packaging/fedora/release-manifest.txt"
# shellcheck source=verify-release-ownership.sh
source "$ROOT/scripts/verify-release-ownership.sh"
STATE_VERSION=3
TOTAL_STAGES=12

BOLD=""; DIM=""; RESET=""; BLUE=""; GREEN=""; YELLOW=""; RED=""
if [[ -t 1 ]] && command -v tput >/dev/null 2>&1 &&
    [[ "$(tput colors 2>/dev/null || echo 0)" -ge 8 ]]; then
    BOLD=$(tput bold); DIM=$(tput dim); RESET=$(tput sgr0)
    BLUE=$(tput setaf 4); GREEN=$(tput setaf 2); YELLOW=$(tput setaf 3); RED=$(tput setaf 1)
fi

usage() {
    cat <<'EOF'
Usage:
  scripts/verify-release.sh [--build-log FILE] RPM
  scripts/verify-release.sh --resume DIR
  scripts/verify-release.sh --rollback DIR
  scripts/verify-release.sh --service-only DIR
  scripts/verify-release.sh --manifest

The first form starts a transaction. Account setup ends with a logout/login
handoff. Continue it from a new local graphical session with --resume. A full
rollback restores the captured package, account, linger, udev, service, and
selected-path state. --service-only stops the takeover and restores tiny-dfr,
but intentionally leaves the package and setup changes installed.
EOF
}

if [[ "${1:-}" == "--manifest" ]]; then
    cat "$MANIFEST"
    exit 0
fi

MODE=run
RPM_PATH=""
BUILD_LOG=""
VERIFY_DIR=""
while (($#)); do
    case "$1" in
        --resume|--rollback|--service-only)
            [[ $# -ge 2 ]] || { usage >&2; exit 2; }
            case "$1" in
                --resume) MODE=resume; VERIFY_DIR=$2 ;;
                --rollback) MODE=rollback; VERIFY_DIR=$2 ;;
                --service-only) MODE=service-only; VERIFY_DIR=$2 ;;
            esac
            shift 2
            ;;
        --build-log)
            [[ $# -ge 2 ]] || { usage >&2; exit 2; }
            BUILD_LOG=$2
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        --)
            shift
            [[ $# -eq 1 && "$MODE" == run ]] || { usage >&2; exit 2; }
            RPM_PATH=$1
            shift
            ;;
        -*)
            usage >&2
            exit 2
            ;;
        *)
            [[ "$MODE" == run && -z "$RPM_PATH" ]] || { usage >&2; exit 2; }
            RPM_PATH=$1
            shift
            ;;
    esac
done

if [[ "$MODE" == run && -z "$RPM_PATH" ]]; then
    usage >&2
    exit 2
fi
if [[ "$MODE" != run && -z "$VERIFY_DIR" ]]; then
    usage >&2
    exit 2
fi

# State is deliberately plain shell assignment, written with printf %q and
# kept in a mode-0700 directory. It is the recovery record, not a user config.
CURRENT_STAGE=0
HANDOFF_PENDING=0
BLOCKED=0
ROLLBACK_IN_PROGRESS=0
ROLLBACK_FAILED=0
ROLLBACK_DONE=0
SNAPSHOT_READY=0
PACKAGE_INSTALLED_BY_RUN=0
USER_GROUP_CHANGED=0
LINGER_CHANGED=0
UDEV_CHANGED=0
TAKEOVER_ATTEMPTED=0
TAKEOVER_ACTIVE=0
TINY_STOP_ATTEMPTED=0
BROKER_CHANGED=0
GLOBAL_SUPERVISOR_CHANGED=0
USER_SUPERVISOR_CHANGED=0
SELECTED_PATH_CHANGED=0
INITIAL_USER=""
INITIAL_SESSION_ID=""
SOURCE_COMMIT=""
PACKAGE_VERSION=""
PACKAGE_RELEASE=""
PACKAGE_ARCH=""
ORIGINAL_PACKAGE_PRESENT=0
ORIGINAL_PACKAGE_NEVRA=""
ORIGINAL_UDEV_RULE_PRESENT=0
ORIGINAL_TINY_ACTIVE=""
ORIGINAL_TINY_ENABLED=""
ORIGINAL_BROKER_ACTIVE=""
ORIGINAL_BROKER_ENABLED=""
ORIGINAL_BROKER_LOAD=""
ORIGINAL_GLOBAL_SUPERVISOR_ENABLED=""
ORIGINAL_USER_SUPERVISOR_ACTIVE=""
ORIGINAL_USER_SUPERVISOR_ENABLED=""
ORIGINAL_USER_SUPERVISOR_LOAD=""
ORIGINAL_LINGER=""
ORIGINAL_SLIVER_USER_PRESENT=0
ORIGINAL_GROUP_SUPERVISORS_PRESENT=0
ORIGINAL_GROUP_DRM_PRESENT=0
ORIGINAL_GROUP_INPUT_PRESENT=0
ORIGINAL_GROUP_BACKLIGHT_PRESENT=0
ORIGINAL_USER_IN_SUPERVISORS=0
STATE_HOME=""
SELECTED_PATH=""
SELECTED_PATH_KIND="missing"
SELECTED_PATH_MODE=""
SELECTED_PATH_LINK=""
PANEL_DRM_NODE=""
PANEL_DRM_CONNECTOR=""
PANEL_DRM_SYSFS_DEVICE=""
PANEL_DRM_DEV_MAJOR_MINOR=""
DRM_TOOL=""
SNAPSHOT_DIR=""
CONFIG_DIR=""
LOG_FILE=""
EVIDENCE_FILE=""
STATE_FILE=""
INTERACTIVE_TTY=0

if [[ "$MODE" == run ]]; then
    if [[ ! -f "$RPM_PATH" ]]; then
        printf 'RPM does not exist: %s\n' "$RPM_PATH" >&2
        exit 1
    fi
    RPM_PATH=$(readlink -f "$RPM_PATH")
    VERIFY_DIR=${SLIVER_VERIFY_DIR:-"$HOME/sliver-release-verification/$(date +%Y%m%d-%H%M%S)"}
    INITIAL_USER=${USER:-$(id -un)}
    INITIAL_SESSION_ID=${XDG_SESSION_ID:-}
    SOURCE_COMMIT=$(git -C "$ROOT" rev-parse --verify HEAD)
    SNAPSHOT_DIR="$VERIFY_DIR/snapshot"
    CONFIG_DIR="$VERIFY_DIR/fixtures"
    LOG_FILE="$VERIFY_DIR/run.log"
    EVIDENCE_FILE="$VERIFY_DIR/evidence.tsv"
    STATE_FILE="$VERIFY_DIR/state.env"
    mkdir -p "$SNAPSHOT_DIR" "$CONFIG_DIR"
    chmod 700 "$VERIFY_DIR" "$SNAPSHOT_DIR" "$CONFIG_DIR"
    INTERACTIVE_TTY=0
    [[ -t 0 && -t 1 ]] && INTERACTIVE_TTY=1
    : > "$EVIDENCE_FILE"
    chmod 600 "$EVIDENCE_FILE"
else
    STATE_FILE="$VERIFY_DIR/state.env"
    [[ -f "$STATE_FILE" ]] || { printf 'No verifier state: %s\n' "$STATE_FILE" >&2; exit 1; }
    expected_state_version=$STATE_VERSION
    # The state file was created by this script in a private directory.
    # shellcheck disable=SC1090
    source "$STATE_FILE"
    [[ "${STATE_VERSION:-}" == "$expected_state_version" ]] || {
        printf 'Unsupported verifier state version\n' >&2
        exit 1
    }
    [[ -n "${LOG_FILE:-}" && -n "${EVIDENCE_FILE:-}" ]] || {
        printf 'Verifier state is incomplete\n' >&2
        exit 1
    }
    BUILD_LOG="${BUILD_LOG:-}"
    chmod 700 "$VERIFY_DIR" 2>/dev/null || true
    chmod 600 "$STATE_FILE" "$EVIDENCE_FILE" 2>/dev/null || true
fi

umask 077
[[ -t 0 && -t 1 ]] && INTERACTIVE_TTY=1
exec > >(tee -a "$LOG_FILE") 2>&1

say()  { printf '  %s\n' "$*"; }
note() { printf '  %s%s%s\n' "$DIM" "$*" "$RESET"; }
warn() { printf '  %s%s%s\n' "$YELLOW" "$*" "$RESET" >&2; }
stage() {
    printf '\n%s%sStage %s/%s: %s%s\n' "$BOLD" "$BLUE" "$1" "$TOTAL_STAGES" "$2" "$RESET"
}
pause() {
    printf '  %s%s%s ' "$DIM" "${1:-Press Enter to continue}" "$RESET"
    read -r _ || true
}
confirm() {
    local reply=""
    printf '  %s? %s [y/N] ' "$YELLOW" "$1"
    read -r reply || true
    [[ "$reply" =~ ^[Yy]$ ]]
}

save_state() {
    local tmp
    tmp=$(mktemp "$VERIFY_DIR/state.XXXXXX")
    chmod 600 "$tmp"
    {
        printf 'STATE_VERSION=%q\n' "$STATE_VERSION"
        printf 'CURRENT_STAGE=%q\n' "$CURRENT_STAGE"
        printf 'HANDOFF_PENDING=%q\n' "$HANDOFF_PENDING"
        printf 'BLOCKED=%q\n' "$BLOCKED"
        printf 'ROLLBACK_DONE=%q\n' "$ROLLBACK_DONE"
        printf 'SNAPSHOT_READY=%q\n' "$SNAPSHOT_READY"
        printf 'PACKAGE_INSTALLED_BY_RUN=%q\n' "$PACKAGE_INSTALLED_BY_RUN"
        printf 'USER_GROUP_CHANGED=%q\n' "$USER_GROUP_CHANGED"
        printf 'LINGER_CHANGED=%q\n' "$LINGER_CHANGED"
        printf 'UDEV_CHANGED=%q\n' "$UDEV_CHANGED"
        printf 'TAKEOVER_ATTEMPTED=%q\n' "$TAKEOVER_ATTEMPTED"
        printf 'TAKEOVER_ACTIVE=%q\n' "$TAKEOVER_ACTIVE"
        printf 'TINY_STOP_ATTEMPTED=%q\n' "$TINY_STOP_ATTEMPTED"
        printf 'BROKER_CHANGED=%q\n' "$BROKER_CHANGED"
        printf 'GLOBAL_SUPERVISOR_CHANGED=%q\n' "$GLOBAL_SUPERVISOR_CHANGED"
        printf 'USER_SUPERVISOR_CHANGED=%q\n' "$USER_SUPERVISOR_CHANGED"
        printf 'SELECTED_PATH_CHANGED=%q\n' "$SELECTED_PATH_CHANGED"
        printf 'INITIAL_USER=%q\n' "$INITIAL_USER"
        printf 'INITIAL_SESSION_ID=%q\n' "$INITIAL_SESSION_ID"
        printf 'SOURCE_COMMIT=%q\n' "$SOURCE_COMMIT"
        printf 'RPM_PATH=%q\n' "$RPM_PATH"
        printf 'BUILD_LOG=%q\n' "$BUILD_LOG"
        printf 'PACKAGE_VERSION=%q\n' "$PACKAGE_VERSION"
        printf 'PACKAGE_RELEASE=%q\n' "$PACKAGE_RELEASE"
        printf 'PACKAGE_ARCH=%q\n' "$PACKAGE_ARCH"
        printf 'ORIGINAL_PACKAGE_PRESENT=%q\n' "$ORIGINAL_PACKAGE_PRESENT"
        printf 'ORIGINAL_PACKAGE_NEVRA=%q\n' "$ORIGINAL_PACKAGE_NEVRA"
        printf 'ORIGINAL_UDEV_RULE_PRESENT=%q\n' "$ORIGINAL_UDEV_RULE_PRESENT"
        printf 'ORIGINAL_TINY_ACTIVE=%q\n' "$ORIGINAL_TINY_ACTIVE"
        printf 'ORIGINAL_TINY_ENABLED=%q\n' "$ORIGINAL_TINY_ENABLED"
        printf 'ORIGINAL_BROKER_ACTIVE=%q\n' "$ORIGINAL_BROKER_ACTIVE"
        printf 'ORIGINAL_BROKER_ENABLED=%q\n' "$ORIGINAL_BROKER_ENABLED"
        printf 'ORIGINAL_BROKER_LOAD=%q\n' "$ORIGINAL_BROKER_LOAD"
        printf 'ORIGINAL_GLOBAL_SUPERVISOR_ENABLED=%q\n' "$ORIGINAL_GLOBAL_SUPERVISOR_ENABLED"
        printf 'ORIGINAL_USER_SUPERVISOR_ACTIVE=%q\n' "$ORIGINAL_USER_SUPERVISOR_ACTIVE"
        printf 'ORIGINAL_USER_SUPERVISOR_ENABLED=%q\n' "$ORIGINAL_USER_SUPERVISOR_ENABLED"
        printf 'ORIGINAL_USER_SUPERVISOR_LOAD=%q\n' "$ORIGINAL_USER_SUPERVISOR_LOAD"
        printf 'ORIGINAL_LINGER=%q\n' "$ORIGINAL_LINGER"
        printf 'ORIGINAL_SLIVER_USER_PRESENT=%q\n' "$ORIGINAL_SLIVER_USER_PRESENT"
        printf 'ORIGINAL_GROUP_SUPERVISORS_PRESENT=%q\n' "$ORIGINAL_GROUP_SUPERVISORS_PRESENT"
        printf 'ORIGINAL_GROUP_DRM_PRESENT=%q\n' "$ORIGINAL_GROUP_DRM_PRESENT"
        printf 'ORIGINAL_GROUP_INPUT_PRESENT=%q\n' "$ORIGINAL_GROUP_INPUT_PRESENT"
        printf 'ORIGINAL_GROUP_BACKLIGHT_PRESENT=%q\n' "$ORIGINAL_GROUP_BACKLIGHT_PRESENT"
        printf 'ORIGINAL_USER_IN_SUPERVISORS=%q\n' "$ORIGINAL_USER_IN_SUPERVISORS"
        printf 'STATE_HOME=%q\n' "$STATE_HOME"
        printf 'SELECTED_PATH=%q\n' "$SELECTED_PATH"
        printf 'SELECTED_PATH_KIND=%q\n' "$SELECTED_PATH_KIND"
        printf 'SELECTED_PATH_MODE=%q\n' "$SELECTED_PATH_MODE"
        printf 'SELECTED_PATH_LINK=%q\n' "$SELECTED_PATH_LINK"
        printf 'PANEL_DRM_NODE=%q\n' "$PANEL_DRM_NODE"
        printf 'PANEL_DRM_CONNECTOR=%q\n' "$PANEL_DRM_CONNECTOR"
        printf 'PANEL_DRM_SYSFS_DEVICE=%q\n' "$PANEL_DRM_SYSFS_DEVICE"
        printf 'PANEL_DRM_DEV_MAJOR_MINOR=%q\n' "$PANEL_DRM_DEV_MAJOR_MINOR"
        printf 'SNAPSHOT_DIR=%q\n' "$SNAPSHOT_DIR"
        printf 'CONFIG_DIR=%q\n' "$CONFIG_DIR"
        printf 'LOG_FILE=%q\n' "$LOG_FILE"
        printf 'EVIDENCE_FILE=%q\n' "$EVIDENCE_FILE"
        printf 'STATE_FILE=%q\n' "$STATE_FILE"
    } > "$tmp"
    mv -f "$tmp" "$STATE_FILE"
}

record_check() {
    local id=$1 status=$2 detail=${3:-}
    detail=${detail//$'\r'/ }
    detail=${detail//$'\n'/ }
    detail=${detail//$'\t'/ }
    printf '%s\t%s\t%s\t%s\n' "$(date --iso-8601=seconds)" "$id" "$status" "$detail" >> "$EVIDENCE_FILE"
}
pass_check() {
    record_check "$1" pass "${2:-verified}"
}
fail_check() {
    record_check "$1" fail "${2:-failed}"
    BLOCKED=1
    save_state
}
blocker() {
    warn "BLOCKED: $*"
    BLOCKED=1
    record_check "blocker" fail "$*"
    save_state
}

refuse_if_blocked() {
    if (( BLOCKED )); then
        warn "A required check failed. No later operation will run."
        exit 1
    fi
}

# Every normal privileged operation comes through this function. A failed
# operation exits immediately; the EXIT trap then starts the transaction
# rollback without attempting another forward change.
privileged_step() {
    local label=$1
    shift
    refuse_if_blocked
    say "Administrator step: $label"
    if "$@"; then
        record_check "privileged_$label" pass "completed"
    else
        blocker "administrator step failed: $label"
        exit 1
    fi
}
user_step() {
    local label=$1
    shift
    refuse_if_blocked
    say "User-manager step: $label"
    if "$@"; then
        record_check "user_$label" pass "completed"
    else
        blocker "user-manager step failed: $label"
        exit 1
    fi
}
logged_step() {
    local id=$1 output=$2
    shift 2
    refuse_if_blocked
    if "$@" > "$output" 2>&1; then
        pass_check "$id" "output saved at $output"
    else
        fail_check "$id" "command failed; output saved at $output"
        exit 1
    fi
}

capture_drm_preflight() {
    local status mode driver
    [[ -n "$PANEL_DRM_NODE" && -n "$PANEL_DRM_CONNECTOR" ]] || return 1
    status=$(<"/sys/class/drm/$PANEL_DRM_CONNECTOR/status")
    mode=$(tr -d '[:space:]' < "/sys/class/drm/$PANEL_DRM_CONNECTOR/modes")
    [[ "$status" == connected && "$mode" == 60x2008 ]] || return 1
    printf 'DRM evidence node: %s\n' "$PANEL_DRM_NODE"
    printf 'DRM evidence connector: %s\n' "$PANEL_DRM_CONNECTOR"
    printf 'DRM evidence status: %s\n' "$status"
    printf 'DRM evidence mode: %s\n' "$mode"
    printf 'DRM evidence transform: logical 2008x60 -> scanout 60x2008 (quarter-turn)\n'
    if [[ "$DRM_TOOL" == drm_info ]]; then
        printf 'DRM evidence command: sudo drm_info %s\n' "$PANEL_DRM_NODE"
        sudo drm_info "$PANEL_DRM_NODE"
    else
        driver=$(basename "$(readlink -f "/sys/class/drm/${PANEL_DRM_NODE##*/}/device/driver")")
        [[ "$driver" =~ ^[[:alnum:]_.-]+$ ]] || return 1
        printf 'DRM evidence command: sudo modetest -M %s -c -p\n' "$driver"
        sudo modetest -M "$driver" -c -p
    fi
}

unit_active() {
    local unit=$1 value
    value=$(systemctl is-active "$unit" 2>/dev/null || true)
    printf '%s' "${value:-unknown}"
}
unit_enabled() {
    local unit=$1 value
    value=$(systemctl is-enabled "$unit" 2>/dev/null || true)
    printf '%s' "${value:-unknown}"
}
unit_load() {
    local unit=$1 value
    value=$(systemctl show "$unit" -p LoadState --value 2>/dev/null || true)
    printf '%s' "${value:-unknown}"
}
user_unit_active() {
    local unit=$1 value
    value=$(systemctl --user is-active "$unit" 2>/dev/null || true)
    printf '%s' "${value:-unknown}"
}
user_unit_enabled() {
    local unit=$1 value
    value=$(systemctl --user is-enabled "$unit" 2>/dev/null || true)
    printf '%s' "${value:-unknown}"
}
user_unit_load() {
    local unit=$1 value
    value=$(systemctl --user show "$unit" -p LoadState --value 2>/dev/null || true)
    printf '%s' "${value:-unknown}"
}

snapshot_selected_path() {
    STATE_HOME=${XDG_STATE_HOME:-"$HOME/.local/state"}
    SELECTED_PATH="$STATE_HOME/sliver/config-path"
    local snapshot="$SNAPSHOT_DIR/config-path"
    mkdir -p "$SNAPSHOT_DIR"
    if [[ -L "$SELECTED_PATH" ]]; then
        SELECTED_PATH_KIND=symlink
        SELECTED_PATH_LINK=$(readlink "$SELECTED_PATH")
        SELECTED_PATH_MODE=""
    elif [[ -f "$SELECTED_PATH" ]]; then
        SELECTED_PATH_KIND=file
        SELECTED_PATH_MODE=$(stat -c '%a' "$SELECTED_PATH")
        cp --preserve=mode,timestamps "$SELECTED_PATH" "$snapshot"
    elif [[ -e "$SELECTED_PATH" ]]; then
        SELECTED_PATH_KIND=other
        fail_check selected_path_shape "config-path exists but is neither a regular file nor symlink"
        return
    else
        SELECTED_PATH_KIND=missing
    fi
    pass_check selected_path_snapshot "captured $SELECTED_PATH_KIND state without following a final symlink"
}
restore_selected_path() {
    local parent tmp="$SNAPSHOT_DIR/config-path.restore"
    parent=$(dirname "$SELECTED_PATH")
    mkdir -p "$parent"
    rm -f "$tmp"
    case "$SELECTED_PATH_KIND" in
        missing)
            rm -f "$SELECTED_PATH"
            ;;
        symlink)
            ln -s "$SELECTED_PATH_LINK" "$tmp"
            rm -f "$SELECTED_PATH"
            mv -f "$tmp" "$SELECTED_PATH"
            ;;
        file)
            cp --preserve=mode,timestamps "$SNAPSHOT_DIR/config-path" "$tmp"
            chmod "$SELECTED_PATH_MODE" "$tmp"
            rm -f "$SELECTED_PATH"
            mv -f "$tmp" "$SELECTED_PATH"
            ;;
        *)
            return 1
            ;;
    esac
}
selected_path_matches_snapshot() {
    case "$SELECTED_PATH_KIND" in
        missing) [[ ! -e "$SELECTED_PATH" && ! -L "$SELECTED_PATH" ]] ;;
        symlink) [[ -L "$SELECTED_PATH" && "$(readlink "$SELECTED_PATH")" == "$SELECTED_PATH_LINK" ]] ;;
        file) [[ -f "$SELECTED_PATH" ]] && cmp -s "$SELECTED_PATH" "$SNAPSHOT_DIR/config-path" &&
            [[ "$(stat -c '%a' "$SELECTED_PATH")" == "$SELECTED_PATH_MODE" ]] ;;
        *) return 1 ;;
    esac
}

capture_snapshot() {
    ORIGINAL_TINY_ACTIVE=$(unit_active tiny-dfr.service)
    ORIGINAL_TINY_ENABLED=$(unit_enabled tiny-dfr.service)
    ORIGINAL_BROKER_ACTIVE=$(unit_active sliver-broker.service)
    ORIGINAL_BROKER_ENABLED=$(unit_enabled sliver-broker.service)
    ORIGINAL_BROKER_LOAD=$(unit_load sliver-broker.service)
    ORIGINAL_GLOBAL_SUPERVISOR_ENABLED=$(unit_enabled sliver-supervisor.service)
    ORIGINAL_USER_SUPERVISOR_ACTIVE=$(user_unit_active sliver-supervisor.service)
    ORIGINAL_USER_SUPERVISOR_ENABLED=$(user_unit_enabled sliver-supervisor.service)
    ORIGINAL_USER_SUPERVISOR_LOAD=$(user_unit_load sliver-supervisor.service)
    ORIGINAL_LINGER=$(loginctl show-user sliver -p Linger --value 2>/dev/null || true)
    [[ -n "$ORIGINAL_LINGER" ]] || ORIGINAL_LINGER=unknown

    if rpm -q sliver >/dev/null 2>&1; then
        ORIGINAL_PACKAGE_PRESENT=1
        ORIGINAL_PACKAGE_NEVRA=$(rpm -q --qf '%{NAME}-%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}' sliver)
    fi
    if [[ -e /usr/lib/udev/rules.d/70-sliver.rules ]]; then ORIGINAL_UDEV_RULE_PRESENT=1; fi
    if getent passwd sliver >/dev/null 2>&1; then ORIGINAL_SLIVER_USER_PRESENT=1; fi
    if getent group sliver-supervisors >/dev/null 2>&1; then ORIGINAL_GROUP_SUPERVISORS_PRESENT=1; fi
    if getent group sliver-drm >/dev/null 2>&1; then ORIGINAL_GROUP_DRM_PRESENT=1; fi
    if getent group sliver-input >/dev/null 2>&1; then ORIGINAL_GROUP_INPUT_PRESENT=1; fi
    if getent group sliver-backlight >/dev/null 2>&1; then ORIGINAL_GROUP_BACKLIGHT_PRESENT=1; fi
    if id -nG "$INITIAL_USER" 2>/dev/null | tr ' ' '\n' | grep -Fx sliver-supervisors >/dev/null; then
        ORIGINAL_USER_IN_SUPERVISORS=1
    fi
    snapshot_selected_path
    {
        printf 'tiny active=%s enabled=%s\n' "$ORIGINAL_TINY_ACTIVE" "$ORIGINAL_TINY_ENABLED"
        printf 'broker active=%s enabled=%s load=%s\n' "$ORIGINAL_BROKER_ACTIVE" "$ORIGINAL_BROKER_ENABLED" "$ORIGINAL_BROKER_LOAD"
        printf 'global supervisor enabled=%s\n' "$ORIGINAL_GLOBAL_SUPERVISOR_ENABLED"
        printf 'user supervisor active=%s enabled=%s load=%s\n' \
            "$ORIGINAL_USER_SUPERVISOR_ACTIVE" "$ORIGINAL_USER_SUPERVISOR_ENABLED" "$ORIGINAL_USER_SUPERVISOR_LOAD"
        printf 'sliver account present=%s linger=%s\n' "$ORIGINAL_SLIVER_USER_PRESENT" "$ORIGINAL_LINGER"
        printf 'groups supervisors=%s drm=%s input=%s backlight=%s\n' \
            "$ORIGINAL_GROUP_SUPERVISORS_PRESENT" "$ORIGINAL_GROUP_DRM_PRESENT" \
            "$ORIGINAL_GROUP_INPUT_PRESENT" "$ORIGINAL_GROUP_BACKLIGHT_PRESENT"
        printf 'invoking user in sliver-supervisors=%s\n' "$ORIGINAL_USER_IN_SUPERVISORS"
    } > "$SNAPSHOT_DIR/service-and-account-state.txt"
    chmod 600 "$SNAPSHOT_DIR/service-and-account-state.txt"
    SNAPSHOT_READY=1
    save_state
}

write_fixtures() {
    cat > "$CONFIG_DIR/valid.lua" <<'LUA'
local sliver = require("sliver.v1")
return {
    api_version = 1,
    render = function(canvas)
        canvas:rectangle(0, 0, 2008, 60, "#000000")
        canvas:text(20, 36, "release verifier", 24, "#ffffff")
    end,
}
LUA
    cat > "$CONFIG_DIR/invalid.lua" <<'LUA'
local sliver = require("sliver.v1")
return {
    api_version = 1,
    render = function()
        error("release verifier invalid fixture")
    end,
}
LUA
    cat > "$CONFIG_DIR/hung.lua" <<'LUA'
local sliver = require("sliver.v1")
local timer
return {
    api_version = 1,
    start = function()
        timer = sliver.timer.after(0.1, function()
            sliver.input.key.down(sliver.input.keys.keyboard.escape)
            os.execute("sleep 30 >/dev/null 2>&1 &")
            while true do end
        end)
    end,
    render = function(canvas)
        canvas:rectangle(0, 0, 2008, 60, "#000000")
    end,
}
LUA
    cat > "$CONFIG_DIR/video-2008x60.lua" <<'LUA'
local sliver = require("sliver.v1")
local frame = string.rep(string.char(0, 24, 80, 255), 2008 * 60)
local timer
return {
    api_version = 1,
    start = function()
        timer = sliver.timer.every(1 / 60, sliver.redraw)
    end,
    stop = function()
        if timer then timer:cancel() end
    end,
    render = function(canvas, time)
        canvas:raw_pixels(frame, "rgba8", 2008, 60, 2008 * 4,
            { x = 0, y = 0, width = 2008, height = 60 },
            { x = 0, y = 0, width = 2008, height = 60 }, "nearest")
        canvas:text(20, 36, string.format("%0.3f", time), 24, "#ffffff")
    end,
}
LUA
    chmod 600 "$CONFIG_DIR"/*.lua
    pass_check named_fixtures "valid.lua invalid.lua hung.lua video-2008x60.lua in $CONFIG_DIR"
}

manual_check() {
    local id=$1 question=$2 detail=$3
    refuse_if_blocked
    say "$detail"
    if confirm "$question"; then
        pass_check "$id" "operator observed: $detail"
    else
        fail_check "$id" "operator did not confirm: $detail"
        exit 1
    fi
}
record_metric() {
    local id=$1 prompt_text=$2 value="" interval fps misses input_delay growth
    local -a fields=()
    refuse_if_blocked
    printf '  %s ' "$prompt_text"
    read -r value || true
    if [[ ! "$value" =~ ^interval_s=[0-9]+([.][0-9]+)?\ fps=[0-9]+([.][0-9]+)?\ misses=[0-9]+\ input_to_frame_ms=[0-9]+([.][0-9]+)?\ latency_growth_ms=[0-9]+([.][0-9]+)?$ ]]; then
        fail_check "$id" "measurement must use the named numeric fields"
        exit 1
    fi
    read -r -a fields <<< "$value"
    interval=${fields[0]#interval_s=}
    fps=${fields[1]#fps=}
    misses=${fields[2]#misses=}
    input_delay=${fields[3]#input_to_frame_ms=}
    growth=${fields[4]#latency_growth_ms=}
    if ! awk -v interval="$interval" -v fps="$fps" -v misses="$misses" \
        -v input_delay="$input_delay" -v growth="$growth" \
        'BEGIN { exit !(interval >= 30 && fps >= 59.5 && misses == 0 && input_delay >= 0 && growth == 0) }'; then
        fail_check "$id" "measurement did not meet the native 60 FPS and bounded-latency threshold"
        exit 1
    fi
    pass_check "$id" "operator measurement: $value"
}

# Rollback helpers intentionally do not call refuse_if_blocked. They are the
# only permitted operations after a failed forward check, and still route every
# administrator operation through one function so its result is recorded.
rollback_privileged() {
    local label=$1
    shift
    say "Rollback administrator step: $label"
    if "$@"; then
        record_check "rollback_$label" pass "completed"
    else
        record_check "rollback_$label" fail "command failed"
        ROLLBACK_FAILED=1
    fi
}
rollback_user() {
    local label=$1
    shift
    say "Rollback user-manager step: $label"
    if "$@"; then
        record_check "rollback_user_$label" pass "completed"
    else
        record_check "rollback_user_$label" fail "command failed"
        ROLLBACK_FAILED=1
    fi
}
restore_unit() {
    local kind=$1 unit=$2 original_active=$3 original_enabled=$4
    if [[ "$kind" == system ]]; then
        if [[ "$original_active" == active ]]; then
            rollback_privileged "restore_${unit}_active" sudo systemctl start "$unit"
        else
            rollback_privileged "restore_${unit}_inactive" sudo systemctl stop "$unit"
        fi
        if [[ "$original_enabled" == enabled ]]; then
            rollback_privileged "restore_${unit}_enabled" sudo systemctl enable "$unit"
        else
            rollback_privileged "restore_${unit}_disabled" sudo systemctl disable "$unit"
        fi
    else
        if [[ "$original_active" == active ]]; then
            rollback_user "restore_${unit}_active" systemctl --user start "$unit"
        else
            rollback_user "restore_${unit}_inactive" systemctl --user stop "$unit"
        fi
        if [[ "$original_enabled" == enabled ]]; then
            rollback_user "restore_${unit}_enabled" systemctl --user enable "$unit"
        else
            rollback_user "restore_${unit}_disabled" systemctl --user disable "$unit"
        fi
    fi
}

verify_service_restore() {
    local ok=0 broker_active broker_enabled global_enabled user_active user_enabled
    broker_active=$(unit_active sliver-broker.service)
    broker_enabled=$(unit_enabled sliver-broker.service)
    global_enabled=$(unit_enabled sliver-supervisor.service)
    user_active=$(user_unit_active sliver-supervisor.service)
    user_enabled=$(user_unit_enabled sliver-supervisor.service)
    if [[ "$ORIGINAL_BROKER_LOAD" == not-found ]]; then
        [[ "$broker_active" != active && "$broker_enabled" != enabled ]] || ok=1
    else
        [[ "$broker_active" == "$ORIGINAL_BROKER_ACTIVE" ]] || ok=1
        [[ "$broker_enabled" == "$ORIGINAL_BROKER_ENABLED" ]] || ok=1
    fi
    if [[ "$ORIGINAL_GLOBAL_SUPERVISOR_ENABLED" == not-found ]]; then
        [[ "$global_enabled" != enabled ]] || ok=1
    else
        [[ "$global_enabled" == "$ORIGINAL_GLOBAL_SUPERVISOR_ENABLED" ]] || ok=1
    fi
    if [[ "$ORIGINAL_USER_SUPERVISOR_LOAD" == not-found ]]; then
        [[ "$user_active" != active && "$user_enabled" != enabled ]] || ok=1
    else
        [[ "$user_active" == "$ORIGINAL_USER_SUPERVISOR_ACTIVE" ]] || ok=1
        [[ "$user_enabled" == "$ORIGINAL_USER_SUPERVISOR_ENABLED" ]] || ok=1
    fi
    return "$ok"
}

restore_and_verify() {
    local mode=$1 rollback_owner_file rollback_tiny_pid
    ROLLBACK_IN_PROGRESS=1
    ROLLBACK_FAILED=0
    save_state

    if (( USER_SUPERVISOR_CHANGED || TAKEOVER_ATTEMPTED )); then
        restore_unit user sliver-supervisor.service \
            "$ORIGINAL_USER_SUPERVISOR_ACTIVE" "$ORIGINAL_USER_SUPERVISOR_ENABLED"
    fi
    if (( GLOBAL_SUPERVISOR_CHANGED || TAKEOVER_ATTEMPTED )); then
        if [[ "$ORIGINAL_GLOBAL_SUPERVISOR_ENABLED" == enabled ]]; then
            rollback_privileged restore_global_supervisor_enabled sudo systemctl --global enable sliver-supervisor.service
        else
            rollback_privileged restore_global_supervisor_disabled sudo systemctl --global disable sliver-supervisor.service
        fi
    fi
    if (( BROKER_CHANGED || TAKEOVER_ATTEMPTED )); then
        restore_unit system sliver-broker.service "$ORIGINAL_BROKER_ACTIVE" "$ORIGINAL_BROKER_ENABLED"
    fi
    if (( TINY_STOP_ATTEMPTED )) && [[ "$ORIGINAL_TINY_ACTIVE" == active ]]; then
        rollback_privileged restore_tiny_dfr_active sudo systemctl start tiny-dfr.service
    fi

    if [[ "$ORIGINAL_TINY_ACTIVE" == active && -n "$PANEL_DRM_NODE" &&
          -n "$PANEL_DRM_CONNECTOR" && -n "$PANEL_DRM_SYSFS_DEVICE" &&
          -n "$PANEL_DRM_DEV_MAJOR_MINOR" ]]; then
        rollback_owner_file="$VERIFY_DIR/drm-owner-after-rollback.txt"
        rollback_privileged capture_drm_owner_after_rollback capture_drm_owner_to_file "$rollback_owner_file"
        rollback_tiny_pid=$(systemctl show tiny-dfr.service -p MainPID --value 2>/dev/null || true)
        if panel_drm_identity_matches_snapshot &&
           owner_matches "$PANEL_DRM_NODE" "$rollback_tiny_pid" "$rollback_owner_file"; then
            record_check rollback_drm_owner_verified pass "tiny-dfr reacquired the exact preflight panel node"
        else
            record_check rollback_drm_owner_verified fail "tiny-dfr did not reacquire the exact preflight panel node"
            ROLLBACK_FAILED=1
        fi
    elif [[ "$ORIGINAL_TINY_ACTIVE" == active && -z "$PANEL_DRM_NODE" ]]; then
        record_check rollback_drm_owner_verified pass "not applicable because failure preceded panel-node capture"
    elif [[ "$ORIGINAL_TINY_ACTIVE" == active ]]; then
        record_check rollback_drm_owner_verified fail "panel DRM identity capture was incomplete"
        ROLLBACK_FAILED=1
    else
        record_check rollback_drm_owner_verified pass "tiny-dfr was not active in the captured state"
    fi

    # Do not remove the package while a service restoration failed. Leaving the
    # install in place is safer than deleting binaries from a running daemon.
    if ! verify_service_restore; then
        record_check rollback_services_verified fail "service state differs from the captured state"
        ROLLBACK_FAILED=1
    else
        record_check rollback_services_verified pass "tiny-dfr and Sliver service states match the snapshot"
    fi

    if ! { [[ "$(unit_active tiny-dfr.service)" == "$ORIGINAL_TINY_ACTIVE" ]] &&
           [[ "$(unit_enabled tiny-dfr.service)" == "$ORIGINAL_TINY_ENABLED" ]]; }; then
        record_check rollback_tiny_dfr_verified fail "tiny-dfr state differs from the captured state"
        ROLLBACK_FAILED=1
    else
        record_check rollback_tiny_dfr_verified pass "tiny-dfr state matches the snapshot"
    fi

    if [[ "$mode" == service ]]; then
        record_check rollback_verified "$([[ $ROLLBACK_FAILED -eq 0 ]] && echo pass || echo fail)" \
            "service-only rollback; package, account, udev, and selected path were intentionally retained"
        (( ROLLBACK_FAILED == 0 )) || return 1
        ROLLBACK_DONE=1
        save_state
        return 0
    fi

    if (( ROLLBACK_FAILED == 0 )); then
        if (( USER_GROUP_CHANGED )) && id -nG "$INITIAL_USER" 2>/dev/null | tr ' ' '\n' | grep -Fx sliver-supervisors >/dev/null; then
            rollback_privileged remove_user_from_supervisors sudo gpasswd --delete "$INITIAL_USER" sliver-supervisors
        fi
        if (( LINGER_CHANGED )); then
            rollback_privileged disable_sliver_linger sudo loginctl disable-linger sliver
            rollback_privileged terminate_sliver_manager sudo loginctl terminate-user sliver
        fi
    fi

    if (( ROLLBACK_FAILED == 0 && PACKAGE_INSTALLED_BY_RUN )); then
        rollback_privileged remove_package sudo dnf remove -y --no-autoremove sliver
        if rpm -q sliver >/dev/null 2>&1; then
            record_check rollback_package_verified fail "sliver remains installed"
            ROLLBACK_FAILED=1
        else
            record_check rollback_package_verified pass "sliver is absent as it was before the run"
        fi
    elif (( PACKAGE_INSTALLED_BY_RUN == 0 )); then
        record_check rollback_package_verified pass "package was not changed by this run"
    fi
    if (( ROLLBACK_FAILED == 0 && PACKAGE_INSTALLED_BY_RUN )); then
        if [[ "$(unit_load sliver-broker.service)" == not-found &&
              "$(unit_load sliver-supervisor.service)" == not-found &&
              "$(user_unit_load sliver-supervisor.service)" == not-found ]]; then
            record_check rollback_units_verified pass "package-created service units are absent"
        else
            record_check rollback_units_verified fail "a package-created service unit remains"
            ROLLBACK_FAILED=1
        fi
    fi

    if (( ROLLBACK_FAILED == 0 && UDEV_CHANGED )); then
        rollback_privileged reload_udev_rules sudo udevadm control --reload-rules
        rollback_privileged retrigger_drm sudo udevadm trigger --subsystem-match=drm
        rollback_privileged retrigger_input sudo udevadm trigger --subsystem-match=input
        rollback_privileged retrigger_misc sudo udevadm trigger --subsystem-match=misc
        rollback_privileged retrigger_backlight sudo udevadm trigger --subsystem-match=backlight
    fi

    if (( ROLLBACK_FAILED == 0 && ORIGINAL_SLIVER_USER_PRESENT == 0 )) && getent passwd sliver >/dev/null 2>&1; then
        rollback_privileged remove_sliver_account sudo userdel --remove sliver
    fi
    if (( ROLLBACK_FAILED == 0 )); then
        for group in sliver-supervisors sliver-drm sliver-input sliver-backlight; do
            local_present=0
            case "$group" in
                sliver-supervisors) local_present=$ORIGINAL_GROUP_SUPERVISORS_PRESENT ;;
                sliver-drm) local_present=$ORIGINAL_GROUP_DRM_PRESENT ;;
                sliver-input) local_present=$ORIGINAL_GROUP_INPUT_PRESENT ;;
                sliver-backlight) local_present=$ORIGINAL_GROUP_BACKLIGHT_PRESENT ;;
            esac
            if (( local_present == 0 )) && getent group "$group" >/dev/null 2>&1; then
                rollback_privileged "remove_${group}_group" sudo groupdel "$group"
            fi
        done
    fi

    if (( SELECTED_PATH_CHANGED )); then
        if restore_selected_path && selected_path_matches_snapshot; then
            record_check rollback_selected_path_verified pass "selected path bytes, mode, and final symlink identity restored"
        else
            record_check rollback_selected_path_verified fail "selected path restoration did not match the snapshot"
            ROLLBACK_FAILED=1
        fi
    else
        record_check rollback_selected_path_verified pass "selected path was not changed by this run"
    fi

    if (( ORIGINAL_SLIVER_USER_PRESENT == 0 )); then
        if getent passwd sliver >/dev/null 2>&1 || getent group sliver-supervisors >/dev/null 2>&1 ||
           getent group sliver-drm >/dev/null 2>&1 || getent group sliver-input >/dev/null 2>&1 ||
           getent group sliver-backlight >/dev/null 2>&1; then
            record_check rollback_account_verified fail "package-created account or group remains"
            ROLLBACK_FAILED=1
        else
            record_check rollback_account_verified pass "package-created account and groups are absent"
        fi
    fi

    if (( ROLLBACK_FAILED == 0 )); then
        record_check rollback_verified pass "full transaction restored the captured host state"
        ROLLBACK_DONE=1
        TAKEOVER_ACTIVE=0
        save_state
        return 0
    fi
    record_check rollback_verified fail "full restoration was not verified; inspect $VERIFY_DIR"
    save_state
    return 1
}

on_exit() {
    local status=$?
    trap - EXIT
    if (( status != 0 && ! ROLLBACK_IN_PROGRESS && SNAPSHOT_READY && !ROLLBACK_DONE )); then
        warn "Verification stopped. Starting automatic full rollback."
        if ! restore_and_verify full; then
            warn "Rollback was not verified. Use: $0 --rollback $VERIFY_DIR"
            status=1
        fi
    fi
    if (( status != 0 )); then
        warn "Verification stopped with status $status. Evidence: $VERIFY_DIR"
    fi
    exit "$status"
}
trap on_exit EXIT
trap 'exit 130' INT TERM HUP

REQUIRED_CHECKS=(
    host_arch host_model local_tty local_active_session local_nonremote_session
    tiny_dfr_preflight drm_preflight drm_panel_node drm_panel_identity input_identity input_capabilities
    drm_native_mode preexisting_sliver_absent repository_suite release_suite package_nevra package_source_commit package_manifest
    package_build_checks package_preinstall_disabled package_preinstall_broker_inactive
    package_preinstall_supervisor_inactive package_not_started package_not_started_global
    package_not_started_user install_preserves_tiny
    broker_identity account_udev fresh_graphical_session fresh_group_membership
    pre_takeover_owner pre_takeover_drm_owner pre_takeover_drm_identity pre_takeover_sliver_absent takeover_drm_owner takeover_services cli_help cli_version cli_valid_apply
    cli_invalid_retains cli_default_reset selected_path_default_reset
    lifecycle_second_session lifecycle_authorization lifecycle_valid_live_apply
    lifecycle_invalid_retention lifecycle_touch_mapping lifecycle_multitouch_cancel
    lifecycle_fn_recovery lifecycle_modifier_uinput lifecycle_logout_handoff
    lifecycle_watchdog_child_key_cleanup service_restart dirty_framebuffer
    backlight_restore suspend_resume fake_video_workload real_video_workload
    performance_measurement
)
all_required_checks_pass() {
    local id status
    for id in "${REQUIRED_CHECKS[@]}"; do
        status=$(awk -F '\t' -v wanted="$id" '$2 == wanted { result=$3 } END { print result }' "$EVIDENCE_FILE")
        if [[ "$status" != pass ]]; then
            warn "Missing affirmative evidence: $id (latest status: ${status:-none})"
            return 1
        fi
    done
}

preflight_stage() {
    stage 1 "Host, session, and pre-install state"
    say "This stage is read-only. It does not install, enable, stop, or trigger anything."
    for command_name in awk cargo cut dnf fuser getent id journalctl loginctl pgrep rpm rpm2cpio cpio sudo systemctl udevadm; do
        if command -v "$command_name" >/dev/null 2>&1; then
            pass_check "tool_$command_name" "available"
        else
            fail_check "tool_$command_name" "required command is missing"
        fi
    done
    if command -v drm_info >/dev/null 2>&1; then
        DRM_TOOL=drm_info
        pass_check drm_tool "drm_info available"
    elif command -v modetest >/dev/null 2>&1; then
        DRM_TOOL=modetest
        pass_check drm_tool "modetest available"
    else
        fail_check drm_tool "drm_info or modetest is required"
    fi
    [[ "$INTERACTIVE_TTY" == 1 ]] && pass_check local_tty "stdin is a terminal and stdout is captured from a terminal" ||
        fail_check local_tty "run from a local terminal, not a pipe or managed non-TTY shell"

    local arch model product type remote seat active session_id
    arch=$(uname -m)
    [[ "$arch" == aarch64 ]] && pass_check host_arch "aarch64" || fail_check host_arch "expected aarch64, found $arch"
    if [[ -r /sys/firmware/devicetree/base/model ]]; then
        model=$(tr -d '\0' < /sys/firmware/devicetree/base/model)
    else
        model=""
    fi
    if [[ -r /sys/devices/virtual/dmi/id/product_name ]]; then
        product=$(tr -d '\0' < /sys/devices/virtual/dmi/id/product_name)
    else
        product=""
    fi
    printf '  device-tree model: %s\n  DMI product: %s\n  architecture: %s\n' \
        "${model:-unavailable}" "${product:-unavailable}" "$arch"
    [[ "$product" == Mac14,7 || "$model" == Mac14,7 ]] &&
        pass_check host_model "Mac14,7 (DMI or device-tree identity)" ||
        fail_check host_model "expected an explicit Mac14,7 identity"

    session_id=${XDG_SESSION_ID:-}
    if [[ -n "$session_id" ]]; then
        type=$(loginctl show-session "$session_id" -p Type --value 2>/dev/null || true)
        remote=$(loginctl show-session "$session_id" -p Remote --value 2>/dev/null || true)
        seat=$(loginctl show-session "$session_id" -p Seat --value 2>/dev/null || true)
        active=$(loginctl show-session "$session_id" -p Active --value 2>/dev/null || true)
        [[ "$type" == wayland || "$type" == x11 ]] && pass_check local_active_session "session $session_id type=$type" || fail_check local_active_session "no active graphical session"
        [[ "$remote" == no && -z "${SSH_CONNECTION:-}" ]] && pass_check local_nonremote_session "seat=$seat remote=$remote" || fail_check local_nonremote_session "remote sessions are not accepted"
        [[ "$seat" == seat0 && "$active" == yes ]] && pass_check session_seat "seat0 active" || fail_check session_seat "active seat0 session required"
    else
        fail_check local_active_session "XDG_SESSION_ID is missing"
        fail_check local_nonremote_session "cannot prove a non-remote session"
    fi

    capture_snapshot
    if (( ORIGINAL_PACKAGE_PRESENT == 0 )); then
        pass_check preexisting_sliver_absent "rpm reports no installed sliver package"
    else
        fail_check preexisting_sliver_absent "an existing Sliver installation cannot be transactionally replaced"
    fi
    if (( ORIGINAL_SLIVER_USER_PRESENT == 0 )); then
        pass_check preexisting_sliver_account_absent "missing sliver account is the expected fresh-install state"
    else
        fail_check preexisting_sliver_account_absent "an existing sliver account is outside this fresh-install transaction"
    fi
    [[ "$ORIGINAL_BROKER_LOAD" == not-found ]] && pass_check preexisting_broker_absent "broker unit is absent" ||
        fail_check preexisting_broker_absent "broker unit already exists"
    [[ "$ORIGINAL_USER_SUPERVISOR_LOAD" == not-found ]] &&
        pass_check preexisting_user_supervisor_absent "user supervisor unit is absent" ||
        fail_check preexisting_user_supervisor_absent "user supervisor unit already exists"
    (( ORIGINAL_UDEV_RULE_PRESENT == 0 )) && pass_check preexisting_udev_rule_absent "Sliver udev rule is absent" ||
        fail_check preexisting_udev_rule_absent "Sliver udev rule already exists"
    [[ "$ORIGINAL_GLOBAL_SUPERVISOR_ENABLED" == not-found || "$ORIGINAL_GLOBAL_SUPERVISOR_ENABLED" == disabled ]] &&
        pass_check package_preinstall_disabled "global supervisor is not enabled before installation" ||
        fail_check package_preinstall_disabled "global supervisor is already enabled"
    [[ "$ORIGINAL_BROKER_ACTIVE" != active ]] &&
        pass_check package_preinstall_broker_inactive "broker is not active before installation" ||
        fail_check package_preinstall_broker_inactive "broker is already active"
    [[ "$ORIGINAL_USER_SUPERVISOR_ACTIVE" != active ]] &&
        pass_check package_preinstall_supervisor_inactive "current user supervisor is not active before installation" ||
        fail_check package_preinstall_supervisor_inactive "current user supervisor is already active"
    [[ "$ORIGINAL_TINY_ACTIVE" == active ]] && pass_check tiny_dfr_preflight "tiny-dfr is active before installation" ||
        fail_check tiny_dfr_preflight "tiny-dfr must own the panel before installation"

    if pgrep -a -f 'sliver-broker|sliver-supervisor' > "$VERIFY_DIR/processes-before.txt" 2>&1; then
        fail_check preexisting_sliver_process "Sliver process exists before installation"
    else
        pass_check preexisting_sliver_process "no Sliver process exists before installation"
    fi
    if PANEL_DRM_NODE=$(panel_drm_node_from_sysfs); then
        pass_check drm_panel_node "sysfs identifies the connected DSI panel node as $PANEL_DRM_NODE"
    else
        fail_check drm_panel_node "sysfs did not identify exactly one connected DSI DRM node during preflight"
        exit 1
    fi
    local panel_sysfs_link="/sys/class/drm/${PANEL_DRM_NODE##*/}/device"
    PANEL_DRM_CONNECTOR=$(panel_drm_connected_dsi_connector "$PANEL_DRM_NODE" 2>/dev/null || true)
    PANEL_DRM_SYSFS_DEVICE=$(readlink -f "$panel_sysfs_link" 2>/dev/null || true)
    PANEL_DRM_DEV_MAJOR_MINOR=$(stat -c '%t:%T' "$PANEL_DRM_NODE" 2>/dev/null || true)
    if [[ -n "$PANEL_DRM_CONNECTOR" && -n "$PANEL_DRM_SYSFS_DEVICE" &&
          -n "$PANEL_DRM_DEV_MAJOR_MINOR" ]]; then
        pass_check drm_panel_identity "saved connected DSI connector $PANEL_DRM_CONNECTOR on $PANEL_DRM_NODE with $PANEL_DRM_SYSFS_DEVICE and device $PANEL_DRM_DEV_MAJOR_MINOR"
    else
        fail_check drm_panel_identity "could not save a connected DSI identity for $PANEL_DRM_NODE"
        exit 1
    fi
    logged_step drm_preflight "$VERIFY_DIR/drm-before.txt" capture_drm_preflight
    if panel_drm_probe_proves_geometry "$VERIFY_DIR/drm-before.txt" \
        "$PANEL_DRM_NODE" "$PANEL_DRM_CONNECTOR"; then
        pass_check drm_native_mode "privileged DRM evidence proves connected native 60x2008@60 and the quarter-turn scanout geometry"
    else
        fail_check drm_native_mode "privileged DRM evidence does not prove the exact connected native mode and scanout geometry"
        exit 1
    fi
    local input_count=0 device
    : > "$VERIFY_DIR/input-before.txt"
    for device in /dev/input/event*; do
        [[ -e "$device" ]] || continue
        input_count=$((input_count + 1))
        if ! udevadm info --query=property --name="$device" >> "$VERIFY_DIR/input-before.txt" 2>&1; then
            warn "udev inspection failed for $device"
        fi
    done
    if (( input_count > 0 )); then
        pass_check input_devices_present "saved udev properties for $input_count event device(s)"
    else
        fail_check input_identity "no event devices were found"
    fi
    if [[ -r /proc/bus/input/devices ]]; then
        cat /proc/bus/input/devices > "$VERIFY_DIR/input-capabilities-before.txt"
        pass_check input_capabilities "saved full evdev capability bitmaps and handlers"
    else
        fail_check input_capabilities "input capability data is unavailable"
    fi
    manual_check drm_native_mode \
        "Does drm-before.txt prove the connected native 60 by 2008 DSI panel and its rotation?" \
        "Review $VERIFY_DIR/drm-before.txt. Confirm the exact connector, native mode, and rotation from its output."
    manual_check input_identity \
        "Do the saved udev and capability files identify the Touch Bar and keyboard on seat0 with the required capabilities?" \
        "Review $VERIFY_DIR/input-before.txt and input-capabilities-before.txt. Do not infer identity from a transport name."
    refuse_if_blocked
    CURRENT_STAGE=1
    save_state
}

package_stage() {
    stage 2 "Repository checks, package identity, and exact manifest"
    refuse_if_blocked
    logged_step repository_suite "$VERIFY_DIR/repository-suite.log" "$ROOT/scripts/check-release-surface.sh"
    logged_step release_suite "$VERIFY_DIR/release-suite.log" bash -c \
        "cd '$ROOT' && cargo build --release --workspace && SLIVER_LUA_WORKER='$ROOT/target/release/sliver-lua-worker' cargo test --release --workspace"

    [[ -n "$BUILD_LOG" && -f "$BUILD_LOG" ]] && pass_check package_build_log_present "build log is $BUILD_LOG" ||
        fail_check package_build_checks "pass the complete rpmbuild log with --build-log"
    if [[ -n "$BUILD_LOG" && -f "$BUILD_LOG" ]]; then
        grep -F 'sliver package check: packaged worker and pure-Lua/C-module tests passed' "$BUILD_LOG" >/dev/null &&
            pass_check package_build_checks "packaged worker and Lua import checks passed" ||
            fail_check package_build_checks "build log lacks the packaged worker test marker"
        grep -F 'sliver package check: exact install manifest passed' "$BUILD_LOG" >/dev/null &&
            pass_check package_build_manifest "build log contains the exact install manifest marker" ||
            fail_check package_build_manifest "build log lacks the package manifest marker"
        if grep -F 'Skipping user-manager integration tests' "$BUILD_LOG" >/dev/null; then
            fail_check package_build_user_manager "package build skipped user-manager checks"
        else
            pass_check package_build_user_manager "package build did not skip user-manager checks"
        fi
    fi

    local identity expected_version expected_release package_commit actual_manifest
    identity=$(rpm -qp --qf '%{NAME}\n%{VERSION}\n%{RELEASE}\n%{ARCH}\n' "$RPM_PATH")
    PACKAGE_VERSION=$(sed -n '2p' <<< "$identity")
    PACKAGE_RELEASE=$(sed -n '3p' <<< "$identity")
    PACKAGE_ARCH=$(sed -n '4p' <<< "$identity")
    expected_version=$(awk '$1 == "Version:" { print $2 }' "$ROOT/packaging/fedora/sliver.spec")
    expected_release=$(awk '$1 == "Release:" { print $2 }' "$ROOT/packaging/fedora/sliver.spec" | sed 's/%{.*//')
    [[ "$(sed -n '1p' <<< "$identity")" == sliver && "$PACKAGE_VERSION" == "$expected_version" &&
       ( "$PACKAGE_RELEASE" == "$expected_release" || "$PACKAGE_RELEASE" == "$expected_release."* ) &&
       "$PACKAGE_ARCH" == aarch64 ]] && pass_check package_nevra "sliver $PACKAGE_VERSION-$PACKAGE_RELEASE.$PACKAGE_ARCH" ||
        fail_check package_nevra "RPM identity is $identity, expected sliver $expected_version-$expected_release.aarch64"

    actual_manifest="$VERIFY_DIR/package-manifest.txt"
    rpm -qpl "$RPM_PATH" | LC_ALL=C sort > "$actual_manifest"
    local static_manifest="$VERIFY_DIR/package-manifest-static.txt"
    grep -v '^/usr/lib/\.build-id\(/\|$\)' "$actual_manifest" > "$static_manifest"
    if diff -u "$MANIFEST" "$static_manifest" > "$VERIFY_DIR/package-manifest.diff"; then
        pass_check package_manifest "RPM static file list exactly matches $MANIFEST"
    else
        fail_check package_manifest "RPM file list differs; see $VERIFY_DIR/package-manifest.diff"
    fi
    local build_id_path build_id_count=0 malformed_build_ids=0
    while IFS= read -r build_id_path; do
        [[ -z "$build_id_path" ]] && continue
        if [[ "$build_id_path" =~ ^/usr/lib/\.build-id/[0-9a-f]{2}/[0-9a-f]{38}$ ]]; then
            build_id_count=$((build_id_count + 1))
        elif [[ "$build_id_path" != /usr/lib/.build-id &&
                ! "$build_id_path" =~ ^/usr/lib/\.build-id/[0-9a-f]{2}$ ]]; then
            malformed_build_ids=1
        fi
    done < <(grep '^/usr/lib/.build-id' "$actual_manifest" || true)
    if (( build_id_count == 4 && malformed_build_ids == 0 )); then
        pass_check package_build_ids "four valid generated RPM build-id entries are present"
    else
        fail_check package_build_ids "RPM build-id entries are missing or malformed"
    fi

    local extract_dir
    extract_dir=$(mktemp -d "$VERIFY_DIR/rpm-content.XXXXXX")
    if (cd "$extract_dir" && rpm2cpio "$RPM_PATH" | cpio -idm --quiet --no-absolute-filenames) >/dev/null 2>&1; then
        if [[ -f "$extract_dir/usr/share/doc/sliver/release-commit" ]]; then
            package_commit=$(tr -d '[:space:]' < "$extract_dir/usr/share/doc/sliver/release-commit")
            [[ "$package_commit" == "$SOURCE_COMMIT" ]] && pass_check package_source_commit "package identity is $SOURCE_COMMIT" ||
                fail_check package_source_commit "RPM source commit is $package_commit, expected $SOURCE_COMMIT"
        else
            fail_check package_source_commit "RPM omitted /usr/share/doc/sliver/release-commit"
        fi
    else
        fail_check package_source_commit "could not extract the RPM with rpm2cpio and cpio"
    fi
    git -C "$ROOT" diff --quiet && git -C "$ROOT" diff --cached --quiet &&
        pass_check tested_tree_clean "source tree is clean at $SOURCE_COMMIT" ||
        fail_check tested_tree_clean "build and verify from a clean exact commit"
    refuse_if_blocked
    CURRENT_STAGE=2
    save_state
}

install_stage() {
    stage 3 "Install without ownership"
    refuse_if_blocked
    [[ "$(unit_enabled sliver-broker.service)" == "$ORIGINAL_BROKER_ENABLED" &&
       "$(unit_active sliver-broker.service)" == "$ORIGINAL_BROKER_ACTIVE" ]] &&
        pass_check package_preinstall_broker_disabled "broker active=$ORIGINAL_BROKER_ACTIVE enabled=$ORIGINAL_BROKER_ENABLED" ||
        fail_check package_preinstall_broker_disabled "broker changed before installation"
    if ! confirm "Install the exact RPM with dnf now? Package hooks must not enable or start Sliver."; then
        fail_check package_install_confirmed "installation was declined"
        exit 1
    fi
    PACKAGE_INSTALLED_BY_RUN=1
    save_state
    privileged_step install_rpm sudo dnf install -y "$RPM_PATH"
    rpm -q sliver >/dev/null 2>&1 && pass_check package_installed "sliver is installed" || {
        fail_check package_installed "dnf returned but sliver is not installed"
        exit 1
    }
    user_step reload_user_units systemctl --user daemon-reload
    [[ "$(unit_enabled sliver-broker.service)" == disabled && "$(unit_active sliver-broker.service)" != active ]] &&
        pass_check package_not_started "broker is disabled and inactive after installation" ||
        fail_check package_not_started "broker was enabled or started by installation"
    [[ "$(unit_enabled sliver-supervisor.service)" != enabled && "$(unit_active sliver-supervisor.service)" != active ]] &&
        pass_check package_not_started_global "global supervisor is not enabled and is inactive after installation" ||
        fail_check package_not_started_global "global supervisor was enabled or started by installation"
    [[ "$(user_unit_enabled sliver-supervisor.service)" == disabled && "$(user_unit_active sliver-supervisor.service)" != active ]] &&
        pass_check package_not_started_user "current user supervisor is disabled and inactive after installation" ||
        fail_check package_not_started_user "current user supervisor was enabled or started by installation"
    [[ "$(unit_active tiny-dfr.service)" == active ]] && pass_check install_preserves_tiny "tiny-dfr remains active" ||
        fail_check install_preserves_tiny "tiny-dfr lost ownership during installation"
    refuse_if_blocked
    CURRENT_STAGE=3
    save_state
}

account_stage() {
    stage 4 "Account, linger, and udev setup"
    refuse_if_blocked
    say "This changes group membership, the sliver user manager's linger flag, and loaded device rules."
    say "It requires a fresh local graphical login before the next stage."
    if ! confirm "Apply the account, linger, and udev changes now, then log out and back in locally?"; then
        fail_check account_setup_confirmed "setup was declined"
        exit 1
    fi
    if (( ORIGINAL_USER_IN_SUPERVISORS == 0 )); then
        USER_GROUP_CHANGED=1
        save_state
        privileged_step add_supervisor_group sudo usermod --append --groups sliver-supervisors "$INITIAL_USER"
    fi
    if (( ORIGINAL_USER_IN_SUPERVISORS == 1 )); then
        pass_check account_group_change "invoking user already belongs to sliver-supervisors"
    fi
    if [[ "$ORIGINAL_LINGER" != yes ]]; then
        LINGER_CHANGED=1
        save_state
        privileged_step enable_sliver_linger sudo loginctl enable-linger sliver
    else
        pass_check sliver_linger_change "sliver linger was already enabled"
    fi
    UDEV_CHANGED=1
    save_state
    privileged_step reload_udev_rules sudo udevadm control --reload-rules
    privileged_step trigger_drm sudo udevadm trigger --subsystem-match=drm
    privileged_step trigger_input sudo udevadm trigger --subsystem-match=input
    privileged_step trigger_misc sudo udevadm trigger --subsystem-match=misc
    privileged_step trigger_backlight sudo udevadm trigger --subsystem-match=backlight
    getent group sliver-supervisors >/dev/null 2>&1 && pass_check account_udev "packaged groups are present and udev rules were reloaded" ||
        fail_check account_udev "sliver-supervisors group is missing after installation"
    HANDOFF_PENDING=1
    CURRENT_STAGE=4
    save_state
    printf '\n'
    say "Setup is complete. Do not continue in this session. Log out locally, log back in, open a terminal, and run:"
    say "$0 --resume $VERIFY_DIR"
    exit 0
}

fresh_session_stage() {
    stage 5 "Verify the fresh local graphical session"
    refuse_if_blocked
    local current_session=${XDG_SESSION_ID:-} type remote seat active groups manager_state
    [[ -n "$current_session" && "$current_session" != "$INITIAL_SESSION_ID" ]] && pass_check fresh_graphical_session "new session $current_session replaced $INITIAL_SESSION_ID" ||
        fail_check fresh_graphical_session "resume from a new local login session"
    type=$(loginctl show-session "$current_session" -p Type --value 2>/dev/null || true)
    remote=$(loginctl show-session "$current_session" -p Remote --value 2>/dev/null || true)
    seat=$(loginctl show-session "$current_session" -p Seat --value 2>/dev/null || true)
    active=$(loginctl show-session "$current_session" -p Active --value 2>/dev/null || true)
    [[ "$type" == wayland || "$type" == x11 ]] && [[ "$remote" == no ]] && [[ "$seat" == seat0 ]] && [[ "$active" == yes ]] ||
        fail_check fresh_graphical_session "new session is not an active local graphical seat0 session"
    groups=$(id -nG "$INITIAL_USER")
    grep -qw sliver-supervisors <<< "$groups" && pass_check fresh_group_membership "new process sees sliver-supervisors membership" ||
        fail_check fresh_group_membership "new session did not acquire sliver-supervisors membership"
    manager_state=$(systemctl --user is-system-running 2>/dev/null || true)
    [[ "$manager_state" == running || "$manager_state" == degraded || "$manager_state" == starting ]] &&
        pass_check fresh_user_manager "user manager is $manager_state" || fail_check fresh_user_manager "user manager is not available"
    HANDOFF_PENDING=0
    CURRENT_STAGE=5
    save_state
}

owner_stage() {
    stage 6 "Capture the pre-takeover owner"
    refuse_if_blocked
    logged_step pre_takeover_owner "$VERIFY_DIR/tiny-dfr-before-takeover.txt" \
        systemctl --no-pager --full status tiny-dfr.service
    if pgrep -a -f 'tiny-dfr|sliver-broker|sliver-supervisor' > "$VERIFY_DIR/processes-before-takeover.txt" 2>&1; then
        pass_check pre_takeover_processes "saved current daemon processes"
    else
        fail_check pre_takeover_processes "could not inspect current daemon processes"
        exit 1
    fi
    : > "$VERIFY_DIR/device-owners-before-takeover.txt"
    local device tiny_pid current_panel_sysfs_device current_panel_dev_major_minor
    for device in /dev/dri/card* /dev/input/event* /dev/uinput; do
        [[ -e "$device" ]] && ls -l "$device" >> "$VERIFY_DIR/device-owners-before-takeover.txt"
    done
    printf 'preflight panel DRM node: %s\n' "$PANEL_DRM_NODE" >> "$VERIFY_DIR/device-owners-before-takeover.txt"
    pass_check pre_takeover_device_owner "saved device permission evidence and exact panel node"
    if [[ -z "$PANEL_DRM_NODE" || ! -c "$PANEL_DRM_NODE" ]]; then
        fail_check pre_takeover_drm_owner "preflight did not retain a valid exact panel DRM node"
        exit 1
    fi
    if panel_drm_identity_matches_snapshot; then
        pass_check pre_takeover_drm_identity "exact connected DSI panel node identity still matches preflight"
    else
        fail_check pre_takeover_drm_identity "exact panel node identity or connected DSI status changed since preflight"
        exit 1
    fi
    if verify_tiny_dfr_drm_owner "$VERIFY_DIR/drm-owner-before-takeover.txt"; then
        tiny_pid=$(systemctl show tiny-dfr.service -p MainPID --value 2>/dev/null || true)
        pass_check pre_takeover_drm_owner "privileged evidence shows $PANEL_DRM_NODE is open only by tiny-dfr PID $tiny_pid"
    else
        fail_check pre_takeover_drm_owner "privileged evidence does not show tiny-dfr as the sole opener of $PANEL_DRM_NODE"
        exit 1
    fi
    if grep -E '(^|[[:space:]/])sliver-(broker|supervisor)([[:space:]]|$)' \
        "$VERIFY_DIR/processes-before-takeover.txt" >/dev/null; then
        fail_check pre_takeover_sliver_absent "saved process evidence contains a Sliver process"
        exit 1
    fi
    pass_check pre_takeover_sliver_absent "saved process evidence contains no Sliver broker or supervisor"
    pass_check pre_takeover_owner "machine-verified: tiny-dfr exclusively opened the exact preflight panel node and Sliver was absent"
    CURRENT_STAGE=6
    save_state
}

panel_drm_identity_matches_snapshot() {
    local current_panel_connector current_panel_sysfs_device current_panel_dev_major_minor
    [[ -n "$PANEL_DRM_NODE" && -n "$PANEL_DRM_CONNECTOR" &&
       -n "$PANEL_DRM_SYSFS_DEVICE" && -n "$PANEL_DRM_DEV_MAJOR_MINOR" ]] || return 1
    current_panel_connector=$(panel_drm_connected_dsi_connector "$PANEL_DRM_NODE" 2>/dev/null || true)
    current_panel_sysfs_device=$(readlink -f "/sys/class/drm/${PANEL_DRM_NODE##*/}/device" 2>/dev/null || true)
    current_panel_dev_major_minor=$(stat -c '%t:%T' "$PANEL_DRM_NODE" 2>/dev/null || true)
    [[ "$current_panel_connector" == "$PANEL_DRM_CONNECTOR" &&
       "$current_panel_sysfs_device" == "$PANEL_DRM_SYSFS_DEVICE" &&
       "$current_panel_dev_major_minor" == "$PANEL_DRM_DEV_MAJOR_MINOR" ]]
}

capture_pre_takeover_drm_owner() {
    printf 'preflight panel DRM node: %s\n' "$PANEL_DRM_NODE"
    printf 'tiny-dfr MainPID: %s\n' "$(systemctl show tiny-dfr.service -p MainPID --value 2>/dev/null || true)"
    sudo fuser -v "$PANEL_DRM_NODE"
}
capture_drm_owner_to_file() {
    local output=$1
    capture_pre_takeover_drm_owner > "$output" 2>&1
}
verify_tiny_dfr_drm_owner() {
    local evidence_file=$1 tiny_pid
    panel_drm_identity_matches_snapshot || return 1
    capture_drm_owner_to_file "$evidence_file" || return 1
    tiny_pid=$(systemctl show tiny-dfr.service -p MainPID --value 2>/dev/null || true)
    owner_matches "$PANEL_DRM_NODE" "$tiny_pid" "$evidence_file"
}

takeover_stage() {
    stage 7 "Take over the real panel"
    refuse_if_blocked
    if ! confirm "Stop tiny-dfr and explicitly enable/start Sliver on the real panel?"; then
        fail_check takeover_confirmed "real-panel takeover was declined"
        exit 1
    fi
    TAKEOVER_ATTEMPTED=1
    TAKEOVER_ACTIVE=1
    save_state
    if [[ "$ORIGINAL_TINY_ACTIVE" == active ]]; then
        if verify_tiny_dfr_drm_owner "$VERIFY_DIR/drm-owner-immediately-before-takeover.txt"; then
            pass_check takeover_drm_owner "revalidated tiny-dfr on the exact panel node immediately before takeover"
        else
            fail_check takeover_drm_owner "tiny-dfr ownership changed before takeover"
            exit 1
        fi
    else
        fail_check takeover_drm_owner "tiny-dfr was not active before takeover"
        exit 1
    fi
    TINY_STOP_ATTEMPTED=1
    save_state
    privileged_step stop_tiny_dfr sudo systemctl stop tiny-dfr.service
    BROKER_CHANGED=1
    save_state
    privileged_step enable_start_broker sudo systemctl enable --now sliver-broker.service
    GLOBAL_SUPERVISOR_CHANGED=1
    save_state
    privileged_step enable_global_supervisor sudo systemctl --global enable sliver-supervisor.service
    USER_SUPERVISOR_CHANGED=1
    save_state
    user_step enable_start_user_supervisor systemctl --user enable --now sliver-supervisor.service
    [[ "$(unit_active tiny-dfr.service)" != active && "$(unit_active sliver-broker.service)" == active &&
       "$(unit_enabled sliver-broker.service)" == enabled && "$(user_unit_active sliver-supervisor.service)" == active ]] &&
        pass_check takeover_services "tiny-dfr stopped; broker and current user supervisor are active" || {
            fail_check takeover_services "service takeover state is not exact"
            exit 1
        }
    if systemctl show sliver-broker.service -p User -p Group -p SupplementaryGroups > "$VERIFY_DIR/broker-identity.txt" 2>&1; then
        grep -F 'User=sliver' "$VERIFY_DIR/broker-identity.txt" >/dev/null &&
            grep -F 'Group=sliver-supervisors' "$VERIFY_DIR/broker-identity.txt" >/dev/null &&
            pass_check broker_identity "broker identity saved at $VERIFY_DIR/broker-identity.txt" ||
            fail_check broker_identity "broker identity does not match the package contract"
    else
        fail_check broker_identity "could not inspect broker identity"
    fi
    manual_check takeover_services \
        "Did ownership visibly move from tiny-dfr to Sliver without a blank or competing owner?" \
        "Observe the real panel and review broker-identity.txt before confirming."
    CURRENT_STAGE=7
    save_state
}

cli_stage() {
    stage 8 "Run named public CLI fixtures"
    refuse_if_blocked
    write_fixtures
    local cli=${SLIVER_CLI:-/usr/bin/sliver} help_output version_output invalid_status
    [[ -x "$cli" ]] || { fail_check cli_binary "installed /usr/bin/sliver is missing"; exit 1; }
    help_output=$("$cli" --help)
    [[ "$help_output" == 'usage: sliver [FILE]' ]] && pass_check cli_help "exact public help output" || fail_check cli_help "unexpected help output"
    version_output=$("$cli" --version)
    [[ "$version_output" == "sliver $PACKAGE_VERSION" ]] && pass_check cli_version "$version_output" || fail_check cli_version "unexpected version output: $version_output"
    SELECTED_PATH_CHANGED=1
    save_state
    if "$cli" "$CONFIG_DIR/valid.lua" > "$VERIFY_DIR/valid.stdout" 2> "$VERIFY_DIR/valid.stderr" &&
       [[ ! -s "$VERIFY_DIR/valid.stdout" && ! -s "$VERIFY_DIR/valid.stderr" ]]; then
        pass_check cli_valid_apply "valid.lua applied silently"
    else
        fail_check cli_valid_apply "valid.lua did not apply silently"
        exit 1
    fi
    set +e
    "$cli" "$CONFIG_DIR/invalid.lua" > "$VERIFY_DIR/invalid.stdout" 2> "$VERIFY_DIR/invalid.stderr"
    invalid_status=$?
    set -e
    if (( invalid_status != 0 )) && [[ -s "$VERIFY_DIR/invalid.stderr" ]]; then
        pass_check cli_invalid_retains "invalid.lua failed with a diagnostic; output saved at $VERIFY_DIR/invalid.stderr"
    else
        fail_check cli_invalid_retains "invalid.lua did not fail with a diagnostic"
        exit 1
    fi
    if "$cli" > "$VERIFY_DIR/default.stdout" 2> "$VERIFY_DIR/default.stderr" &&
       [[ ! -s "$VERIFY_DIR/default.stdout" && ! -s "$VERIFY_DIR/default.stderr" ]]; then
        pass_check cli_default_reset "default selection and reset were silent"
    else
        fail_check cli_default_reset "default reset was not silent"
        exit 1
    fi
    if [[ ! -e "$SELECTED_PATH" && ! -L "$SELECTED_PATH" ]]; then
        pass_check selected_path_default_reset "default reset cleared the selected path"
    else
        fail_check selected_path_default_reset "default reset left a selected path"
    fi
    CURRENT_STAGE=8
    save_state
}

lifecycle_stage() {
    stage 9 "Observe lifecycle, authorization, recovery, and cleanup"
    refuse_if_blocked
    manual_check lifecycle_second_session \
        "Did a second local graphical session switch to its own Sliver state and back without a blank handoff?" \
        "Use local seat0 switching only. Record the visible frame and handoff result in the run log."
    manual_check lifecycle_authorization \
        "Were inactive, SSH, and root config applications rejected while the active local session remained authoritative?" \
        "Use the named valid.lua and invalid.lua fixtures where appropriate. Do not treat a same-UID remote shell as local."
    manual_check lifecycle_valid_live_apply \
        "Did applying valid.lua from the active local session commit a new visible frame?" \
        "Run sliver $CONFIG_DIR/valid.lua and wait for its first committed frame."
    manual_check lifecycle_invalid_retention \
        "Did applying invalid.lua retain the previously committed frame and report path, stage, and traceback text?" \
        "Run sliver $CONFIG_DIR/invalid.lua and inspect invalid.stderr without guessing."
    manual_check lifecycle_touch_mapping \
        "Did rotation and all four Touch Bar touch edges map to the expected logical 2008x60 coordinates?" \
        "Test the real panel edges and orientation, not a synthetic event source."
    manual_check lifecycle_multitouch_cancel \
        "Did independent multitouch contacts, cancel events, and replacement cancellation behave correctly?" \
        "Use real fingers and an ownership or config replacement to exercise cancellation."
    manual_check lifecycle_fn_recovery \
        "Did a continuous three-second Fn hold show the fixed F1-F12 row and deliver Fn-up recovery correctly?" \
        "Hold Fn for the full deadline, then test recovery key activation and release."
    manual_check lifecycle_modifier_uinput \
        "Did left/right modifier bridging and generic uinput key delivery work without stuck keys?" \
        "Test physical modifiers with a recovery and Lua key action, then release every key."
    manual_check lifecycle_logout_handoff \
        "Did logout restore the pre-login default and login retain it until the new worker committed, without a blank interval?" \
        "Use a real local logout/login and review both service journals."
    manual_check lifecycle_watchdog_child_key_cleanup \
        "Did hung.lua hit the two-second watchdog, kill child processes, and release every synthetic key?" \
        "Apply $CONFIG_DIR/hung.lua only as the named watchdog fixture, then verify no child or held key remains."
    CURRENT_STAGE=9
    save_state
}

restart_stage() {
    stage 10 "Restart infrastructure and inspect journals"
    refuse_if_blocked
    if ! confirm "Restart the Sliver broker and current user supervisor now? This interrupts panel ownership briefly."; then
        fail_check service_restart_confirmed "restart was declined"
        exit 1
    fi
    privileged_step restart_broker sudo systemctl restart sliver-broker.service
    user_step restart_user_supervisor systemctl --user restart sliver-supervisor.service
    [[ "$(unit_active sliver-broker.service)" == active && "$(user_unit_active sliver-supervisor.service)" == active ]] &&
        pass_check service_restart "broker and supervisor recovered active after restart" || {
            fail_check service_restart "service restart did not recover both units"
            exit 1
        }
    logged_step user_journal_restart "$VERIFY_DIR/user-journal-restart.txt" \
        journalctl --user -u sliver-supervisor.service -n 100 --no-pager
    privileged_step capture_broker_journal sudo journalctl -u sliver-broker.service -n 100 --no-pager
    manual_check dirty_framebuffer \
        "Did a command-mode pixel/control update flush a dirty framebuffer after restart?" \
        "Change a visible value and confirm the real panel updates, then review the restart journal files."
    manual_check service_restart \
        "Did the broker and supervisor restart once without a restart loop, lost state, or competing DRM owner?" \
        "Review both restart journals and the visible panel before confirming."
    CURRENT_STAGE=10
    save_state
}

suspend_performance_stage() {
    stage 11 "Suspend, resume, and named native workload"
    refuse_if_blocked
    local worker=${SLIVER_LUA_WORKER:-"$ROOT/target/release/sliver-lua-worker"}
    if [[ -x "$worker" ]]; then
        logged_step fake_video_workload "$VERIFY_DIR/fake-video-workload.txt" \
            env SLIVER_LUA_WORKER="$worker" cargo test --release --package sliverd --lib \
            lua_raw_decoded_frames_hold_native_rate_under_broker_contention -- --nocapture
    else
        fail_check fake_video_workload "release worker is missing"
        exit 1
    fi
    say "The reproducible real-panel workload is $CONFIG_DIR/video-2008x60.lua."
    say "It redraws a complete native 2008x60 RGBA frame every 1/60 second."
    manual_check real_video_workload \
        "Did video-2008x60.lua run on the real panel for the agreed observation interval?" \
        "Apply sliver $CONFIG_DIR/video-2008x60.lua and keep the named workload and panel visible while measuring."
    if ! confirm "Is it safe to suspend the whole machine now?"; then
        fail_check suspend_confirmed "suspend was declined"
        exit 1
    fi
    privileged_step suspend_machine sudo systemctl suspend
    manual_check suspend_resume \
        "After resume, did touch cancel, backlight restore, devices reacquire, Lua state survive, and one fresh frame appear?" \
        "Observe the real panel and input after resume. Do not confirm from journal text alone."
    manual_check backlight_restore \
        "Did setting and restoring the Touch Bar backlight survive worker restart and suspend?" \
        "Record the before, changed, and restored normalized levels in the run log."
    record_metric performance_measurement \
        "Enter metrics as interval_s=30 fps=59.8 misses=0 input_to_frame_ms=18 latency_growth_ms=0 (requires >=30s, >=59.5 FPS, zero misses, and numeric bounded latency):"
    CURRENT_STAGE=11
    save_state
}

final_stage() {
    stage 12 "Review evidence and perform full rollback"
    refuse_if_blocked
    logged_step user_journal_final "$VERIFY_DIR/user-journal-final.txt" \
        journalctl --user -u sliver-supervisor.service -b --no-pager
    privileged_step capture_broker_journal_final sudo journalctl -u sliver-broker.service -b --no-pager
    if ! all_required_checks_pass; then
        fail_check complete_acceptance_evidence "one or more required checks lack affirmative evidence"
        exit 1
    fi
    say "The evidence ledger has one required entry per acceptance check."
    say "A full rollback removes this fresh-install RPM and restores the captured account, linger, udev, services, and selected path."
    if ! confirm "Perform the full transactional rollback now and verify every restored state?"; then
        fail_check rollback_confirmed "full rollback was declined"
        exit 1
    fi
    if ! restore_and_verify full; then
        exit 1
    fi
    if [[ "$(awk -F '\t' '$2 == "rollback_verified" { result=$3 } END { print result }' "$EVIDENCE_FILE")" != pass ]]; then
        blocker "full rollback was not recorded as verified"
        exit 1
    fi
    CURRENT_STAGE=12
    save_state
    printf '\n%s%sVerification passed and the host was restored.%s\n' "$BOLD" "$GREEN" "$RESET"
    say "Evidence: $VERIFY_DIR"
}

if [[ "$MODE" == rollback || "$MODE" == service-only ]]; then
    if ! confirm "Run the $MODE cleanup recorded in $VERIFY_DIR now?"; then
        printf 'Cleanup declined. No service or package state was changed.\n' >&2
        exit 1
    fi
    if [[ "$MODE" == service-only ]]; then
        restore_and_verify service
    else
        restore_and_verify full
    fi
    exit $?
fi

if [[ "$CURRENT_STAGE" -lt 1 ]]; then preflight_stage; fi
if [[ "$CURRENT_STAGE" -lt 2 ]]; then package_stage; fi
if [[ "$CURRENT_STAGE" -lt 3 ]]; then install_stage; fi
if [[ "$CURRENT_STAGE" -lt 4 ]]; then account_stage; fi
if [[ "$CURRENT_STAGE" -lt 5 ]]; then fresh_session_stage; fi
if [[ "$CURRENT_STAGE" -lt 6 ]]; then owner_stage; fi
if [[ "$CURRENT_STAGE" -lt 7 ]]; then takeover_stage; fi
if [[ "$CURRENT_STAGE" -lt 8 ]]; then cli_stage; fi
if [[ "$CURRENT_STAGE" -lt 9 ]]; then lifecycle_stage; fi
if [[ "$CURRENT_STAGE" -lt 10 ]]; then restart_stage; fi
if [[ "$CURRENT_STAGE" -lt 11 ]]; then suspend_performance_stage; fi
if [[ "$CURRENT_STAGE" -lt 12 ]]; then final_stage; fi
