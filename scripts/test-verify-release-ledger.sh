#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
script="$root/scripts/verify-release.sh"
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

fail() {
    printf 'verify-release ledger test failed: %s\n' "$*" >&2
    exit 1
}

# Exercise the actual ledger requirements and journal checks, without running
# the interactive transaction or restarting either host service.
source <(awk '
    /^REQUIRED_CHECKS=\(/ { array = 1 }
    array { print }
    array && /^\)$/ { array = 0 }
    /^(record_check|pass_check|fail_check|all_required_checks_pass|assert_restart_journal)\(\) \{/ { copying = 1 }
    copying { print }
    copying && /^}$/ { copying = 0 }
' "$script")
warn() { printf '%s\n' "$*" >&2; }
save_state() { :; }
EVIDENCE_FILE="$tmp/evidence.tsv"
: > "$EVIDENCE_FILE"
for id in "${REQUIRED_CHECKS[@]}"; do
    [[ "$id" == restart_journals* ]] || pass_check "$id" 'test fixture'
done
if all_required_checks_pass 2>/dev/null; then
    fail 'acceptance succeeded without restart journal evidence'
fi
printf 'sliver-supervisor.service: Deactivated successfully.\n' > "$tmp/user-journal"
printf 'sliver-broker.service: Deactivated successfully.\n' > "$tmp/broker-journal"
assert_restart_journal restart_journals_user "$tmp/user-journal" sliver-supervisor.service
if all_required_checks_pass 2>/dev/null; then
    fail 'acceptance succeeded without broker restart journal evidence'
fi
assert_restart_journal restart_journals_broker "$tmp/broker-journal" sliver-broker.service
all_required_checks_pass || fail 'successful restart journal checks cannot satisfy final acceptance'
printf "sliver-broker.service: Failed with result 'timeout'.\n" >> "$tmp/broker-journal"
if assert_restart_journal restart_journals_broker "$tmp/broker-journal" sliver-broker.service; then
    fail 'a failed restart journal passed'
fi
if all_required_checks_pass 2>/dev/null; then
    fail 'acceptance ignored a later failed journal check'
fi
printf 'verify-release ledger tests passed\n'
