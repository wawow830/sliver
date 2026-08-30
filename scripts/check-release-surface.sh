#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

fail() {
  printf 'release audit failed: %s\n' "$*" >&2
  exit 1
}

cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings

audit_tmp=$(mktemp -d "${TMPDIR:-/tmp}/sliver-release-audit.XXXXXX")
trap 'rm -rf -- "$audit_tmp"' EXIT

cli=${SLIVER_CLI:-target/debug/sliver}
if [[ ! -x "$cli" ]]; then
  cargo build --package sliverd --bin sliver
fi

help_output=$($cli --help)
[[ "$help_output" == 'usage: sliver [FILE]' ]] || fail "unexpected --help output"
version_output=$($cli --version)
[[ "$version_output" =~ ^sliver\ [0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "unexpected --version output"

set +e
$cli one.lua two.lua > "$audit_tmp/stdout" 2> "$audit_tmp/stderr"
usage_status=$?
set -e
[[ "$usage_status" == 2 ]] || fail "extra arguments returned status $usage_status, expected 2"
[[ ! -s "$audit_tmp/stdout" ]] || fail "usage error wrote to stdout"
grep -F 'usage: sliver [FILE]' "$audit_tmp/stderr" >/dev/null ||
  fail "usage error omitted its usage line"

spec=packaging/fedora/sliver.spec
grep -F '%{_bindir}/sliver' "$spec" >/dev/null || fail "RPM does not install the public client"
if grep -E '^%\{_(bindir|libexecdir)\}/sliver-(edit|preview|probe|calibrate)' "$spec" >/dev/null; then
  fail "RPM exposes a removed executable"
fi
if grep -E '^[[:space:]]*sliver (preview|probe|status|logs|diagnostic|install|service)([[:space:]]|$)' \
  README.md docs packaging systemd >/dev/null 2>&1; then
  fail "documentation exposes a removed public command"
fi

grep -F 'Success is silent' README.md >/dev/null || fail "README omits silent success behavior"
grep -F 'Usage errors exit 2' README.md >/dev/null || fail "README omits usage status"
grep -F 'M1' README.md >/dev/null || fail "README omits the M1 validation boundary"

grep -F 'The package does not take over the Touch Bar during installation.' \
  README.md >/dev/null || fail "README omits package takeover behavior"

grep -F 'scripts/verify-release.sh' README.md >/dev/null ||
  fail "README does not link the privileged release verifier"

grep -F 'packaging/fedora/INSTALL.md' README.md >/dev/null ||
  fail "README does not link package instructions"

if [[ $# -gt 0 ]]; then
  bash packaging/fedora/check-install.sh "$1"
else
  printf 'source, CLI, documentation, and package manifest checks passed\n'
  printf 'RPM buildroot checks skipped: pass a buildroot path to check-install.sh\n'
fi
