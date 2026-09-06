#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
spec="$root/packaging/fedora/sliver.spec"
prepare="$root/scripts/prepare-fedora-sources.sh"
manifest="$root/packaging/fedora/release-manifest.txt"

fail() {
    printf 'Fedora packaging test failed: %s\n' "$*" >&2
    exit 1
}

bash -n "$prepare" || fail 'source preparation script is not valid bash'
grep -F 'Source1:        sliver-%{version}-vendor.tar.gz' "$spec" >/dev/null ||
    fail 'spec does not declare the reproducible vendor source archive'
grep -F '%cargo_prep -v vendor' "$spec" >/dev/null ||
    fail 'spec does not use cargo-rpm-macros vendored-source mode'
grep -F '%cargo_vendor_manifest > cargo-vendor.txt' "$spec" >/dev/null ||
    fail 'spec does not record the vendored crate manifest'
grep -F '%license cargo-vendor.txt' "$spec" >/dev/null ||
    fail 'spec does not ship the vendored crate manifest as license metadata'
grep -F 'cargo vendor --locked' "$prepare" >/dev/null ||
    fail 'source preparation is not locked to Cargo.lock'
grep -F 'sliver-${version}-vendor.tar.gz' "$prepare" >/dev/null ||
    fail 'source preparation does not create the declared vendor archive'
grep -Fx '/usr/share/licenses/sliver/cargo-vendor.txt' "$manifest" >/dev/null ||
    fail 'release manifest omits cargo vendor metadata'
if grep -F '%cargo_generate_buildrequires' "$spec" >/dev/null; then
    fail 'spec still generates unavailable Fedora crate BuildRequires'
fi
if grep -F -- '--skip-unavailable' "$spec" "$prepare" >/dev/null; then
    fail 'packaging bypasses missing dependencies with --skip-unavailable'
fi

printf 'Fedora packaging contract checks passed\n'
