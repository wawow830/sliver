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
    /^(record_check|pass_check|fail_check|all_required_checks_pass|assert_restart_journal|record_modifier_check)\(\) \{/ { copying = 1 }
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
# A correctable entry-format mistake is not a failed hardware observation.
# Exercise the actual prompt with the exact ordering mistake from the live run.
source "$root/scripts/verify-release-evidence.sh"
say() { printf '%s\n' "$*"; }
refuse_if_blocked() { :; }
canonical='tested=left_ctrl,left_alt,right_alt,left_shift,right_shift,left_super,right_super hardware_na=right_ctrl'
unordered='tested=left_ctrl,left_alt,left_shift,left_super,right_alt,right_shift,right_super hardware_na=right_ctrl'
printf '%s\n%s\n' "$unordered" "$canonical" > "$tmp/modifier-input"
: > "$EVIDENCE_FILE"
if ! (record_modifier_check < "$tmp/modifier-input") > "$tmp/modifier-output" 2>&1; then
    fail 'corrected modifier entry aborted instead of retrying'
fi
[[ $(wc -l < "$EVIDENCE_FILE") == 1 ]] || fail 'format mistake polluted acceptance ledger'
grep -F $'lifecycle_modifier_uinput\tpass\t' "$EVIDENCE_FILE" >/dev/null ||
    fail 'corrected modifier evidence did not pass'
grep -F "$canonical" "$EVIDENCE_FILE" >/dev/null || fail 'corrected entry was not retained'
grep -F 'left_ctrl right_ctrl left_alt right_alt left_shift right_shift left_super right_super' "$tmp/modifier-output" >/dev/null ||
    fail 'prompt does not explain required ordering'

: > "$EVIDENCE_FILE"
if (record_modifier_check <<< "$unordered") > "$tmp/modifier-eof-output" 2>&1; then
    fail 'incomplete correction at EOF passed'
fi
grep -F $'lifecycle_modifier_uinput\tfail\t' "$EVIDENCE_FILE" >/dev/null ||
    fail 'EOF without valid evidence did not fail closed'

: > "$EVIDENCE_FILE"
if (record_modifier_check <<< 'fail') > "$tmp/modifier-fail-output" 2>&1; then
    fail 'explicit hardware failure passed'
fi
grep -F 'operator reported a physical modifier test failure' "$EVIDENCE_FILE" >/dev/null ||
    fail 'physical failure was not recorded distinctly from input formatting'
printf 'verify-release ledger tests passed\n'
