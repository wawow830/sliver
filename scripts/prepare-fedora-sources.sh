#!/usr/bin/env bash
set -Eeuo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
out_dir=${1:-"${HOME}/rpmbuild/SOURCES"}
spec=${root}/packaging/fedora/sliver.spec
version=$(awk '$1 == "Version:" { print $2 }' "$spec")
commit=$(git -C "$root" rev-parse --verify HEAD)

if [[ -n $(git -C "$root" status --porcelain) ]]; then
    printf 'source tree must be clean before preparing Fedora sources\n' >&2
    exit 1
fi

mkdir -p "$out_dir"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/sliver-fedora-sources.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

main_archive="$tmp/sliver-${version}.tar.gz"
vendor_archive="$tmp/sliver-${version}-vendor.tar.gz"
vendor_dir="$tmp/vendor"
epoch=$(git -C "$root" show -s --format=%ct "$commit")

git -C "$root" archive \
    --format=tar.gz \
    --prefix="sliver-${version}/" \
    --output="$main_archive" \
    "$commit"

cd "$root"
cargo vendor --locked "$vendor_dir" > "$tmp/cargo-vendor.log"
tar --sort=name --mtime="@${epoch}" --owner=0 --group=0 --numeric-owner \
    --directory="$tmp" --create --file=- vendor | gzip -n > "$vendor_archive"

install -m 0644 "$main_archive" "$out_dir/sliver-${version}.tar.gz"
install -m 0644 "$vendor_archive" "$out_dir/sliver-${version}-vendor.tar.gz"
install -m 0644 "$root/packaging/fedora/sliver.sysusers" "$out_dir/sliver.sysusers"

printf 'commit=%s\nversion=%s\nsource_dir=%s\n' "$commit" "$version" "$out_dir"
printf 'source_sha256=%s\n' "$(sha256sum "$out_dir/sliver-${version}.tar.gz" | awk '{print $1}')"
printf 'vendor_sha256=%s\n' "$(sha256sum "$out_dir/sliver-${version}-vendor.tar.gz" | awk '{print $1}')"
printf 'vendor_crates=%s\n' "$(find "$vendor_dir" -mindepth 1 -maxdepth 1 -type d | wc -l)"
