#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
script="$root/scripts/verify-release.sh"

fail() {
    printf 'verify-release service test failed: %s\n' "$*" >&2
    exit 1
}

# Load the production query and verification functions without running the
# interactive transaction or touching host services.
source <(awk '
    /^(unit_active|unit_enabled|global_unit_enabled|user_unit_active|user_unit_enabled|verify_service_restore)\(\) \{/ { copying = 1 }
    copying { print }
    copying && /^}$/ { copying = 0 }
' "$script")

systemctl() {
    case "$*" in
        '--global is-enabled sliver-supervisor.service') printf '%s\n' "$mock_global_enabled" ;;
        'is-enabled sliver-supervisor.service') printf 'not-found\n'; return 4 ;;
        'is-active sliver-broker.service'|'--user is-active sliver-supervisor.service') printf 'inactive\n'; return 3 ;;
        'is-enabled sliver-broker.service'|'--user is-enabled sliver-supervisor.service') printf 'disabled\n'; return 1 ;;
        *) fail "unexpected systemctl call: $*" ;;
    esac
}

ORIGINAL_BROKER_LOAD=not-found
ORIGINAL_BROKER_ACTIVE=inactive
ORIGINAL_BROKER_ENABLED=not-found
ORIGINAL_USER_SUPERVISOR_LOAD=not-found
ORIGINAL_USER_SUPERVISOR_ACTIVE=inactive
ORIGINAL_USER_SUPERVISOR_ENABLED=not-found
ORIGINAL_GLOBAL_SUPERVISOR_ENABLED=not-found

mock_global_enabled=enabled
if verify_service_restore; then
    fail 'rollback accepted a globally enabled user supervisor absent from the snapshot'
fi

mock_global_enabled=disabled
verify_service_restore || fail 'rollback rejected a disabled global supervisor'
ORIGINAL_GLOBAL_SUPERVISOR_ENABLED=enabled
mock_global_enabled=enabled
verify_service_restore || fail 'rollback rejected restored global enablement'
mock_global_enabled=disabled
if verify_service_restore; then
    fail 'rollback accepted missing original global enablement'
fi

printf 'verify-release service tests passed\n'
