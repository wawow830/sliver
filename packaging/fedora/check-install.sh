#!/bin/sh
set -eu

root=${1:?usage: check-install.sh BUILDROOT}

required='
x:/usr/bin/sliver
x:/usr/libexec/sliver/sliver-broker
x:/usr/libexec/sliver/sliver-supervisor
x:/usr/libexec/sliver/sliver-lua-worker
f:/usr/share/doc/sliver/README.md
f:/usr/share/doc/sliver/INSTALL.md
f:/usr/share/doc/sliver/lua.md
f:/usr/share/doc/sliver/architecture.md
f:/usr/share/doc/sliver/troubleshooting.md
f:/usr/share/doc/sliver/release-commit
f:/usr/share/licenses/sliver/cargo-vendor.txt
f:/usr/lib/systemd/system/sliver-broker.service
f:/usr/lib/systemd/user/sliver-supervisor.service
f:/usr/lib/systemd/user/sliver-lua-worker-.service.d/50-defaults.conf
f:/usr/lib/udev/rules.d/99-z-sliver.rules
f:/usr/lib/sysusers.d/sliver.conf
'
for entry in $required; do
    kind=${entry%%:*}
    path=${entry#*:}
    case "$kind" in
        f) test -f "$root$path" || {
            printf 'missing packaged file: %s\n' "$path" >&2
            exit 1
        };;
        x) test -x "$root$path" || {
            printf 'packaged file is not executable: %s\n' "$path" >&2
            exit 1
        };;
        *)
            printf 'invalid package manifest entry: %s\n' "$entry" >&2
            exit 1
            ;;
    esac
done

manifest=$(dirname "$0")/release-manifest.txt
expected=$(mktemp)
actual=$(mktemp)
trap 'rm -f "$expected" "$actual"' EXIT
sed \
    -e '\#^/usr/libexec/sliver$#d' \
    -e '\#^/usr/lib/systemd/user/sliver-lua-worker-.service.d$#d' \
    -e '\#^/usr/share/doc/sliver$#d' \
    -e '\#^/usr/share/doc/sliver/#d' \
    "$manifest" > "$expected"
LC_ALL=C sort -o "$expected" "$expected"
find "$root" -type f -printf '/%P\n' |
    sed -e '\#^/usr/lib/debug/#d' -e '\#^/usr/src/debug/#d' \
        -e '\#^/usr/share/doc/sliver$#d' -e '\#^/usr/share/doc/sliver/#d' \
        -e '\#^/usr/share/licenses/sliver$#d' -e '\#^/usr/share/licenses/sliver/#d' |
    LC_ALL=C sort > "$actual"
diff -u "$expected" "$actual"

for path in \
    /usr/bin/sliverd \
    /usr/bin/sliver-edit \
    /usr/bin/sliver-broker \
    /usr/bin/sliver-supervisor \
    /usr/bin/sliver-lua-worker \
    /usr/libexec/sliver/sliver-calibrate \
    /usr/share/sliver/default.lua; do
    test ! -e "$root$path" || {
        printf 'unexpected public or editable file: %s\n' "$path" >&2
        exit 1
    }
done

grep -F 'FinalKillSignal=SIGKILL' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'FinalKillSignal=SIGKILL' \
    "$root/usr/lib/systemd/user/sliver-supervisor.service" >/dev/null
grep -F 'Type=notify' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'NotifyAccess=main' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'User=sliver' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'Group=sliver-supervisors' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'RuntimeDirectoryMode=0750' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'SupplementaryGroups=sliver-drm sliver-input sliver-backlight' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'g sliver-drm -' "$root/usr/lib/sysusers.d/sliver.conf" >/dev/null
grep -F 'g sliver-input -' "$root/usr/lib/sysusers.d/sliver.conf" >/dev/null
grep -F 'g sliver-backlight -' "$root/usr/lib/sysusers.d/sliver.conf" >/dev/null
grep -F 'ExecStart=/usr/libexec/sliver/sliver-broker' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'Conflicts=tiny-dfr.service' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'Before=tiny-dfr.service' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'ExecStart=/usr/libexec/sliver/sliver-supervisor' \
    "$root/usr/lib/systemd/user/sliver-supervisor.service" >/dev/null
grep -F 'ConditionGroup=sliver-supervisors' \
    "$root/usr/lib/systemd/user/sliver-supervisor.service" >/dev/null
grep -F 'PrivateDevices=yes' \
    "$root/usr/lib/systemd/user/sliver-lua-worker-.service.d/50-defaults.conf" >/dev/null
udev_rule=$root/usr/lib/udev/rules.d/99-z-sliver.rules
test "$(basename "$udev_rule")" = 99-z-sliver.rules || {
    printf 'Sliver udev rules must run after the Touch Bar seat rules\n' >&2
    exit 1
}
grep -F 'MODE="0660"' "$udev_rule" >/dev/null
grep -F 'ID_SEAT}=="seat-touchbar"' "$udev_rule" >/dev/null
grep -F 'ID_INPUT_TOUCHSCREEN}=="1"' "$udev_rule" >/dev/null
grep -F 'ID_INPUT_KEYBOARD}=="1"' "$udev_rule" >/dev/null
if grep -F 'ATTRS{name}' "$udev_rule" >/dev/null; then
    printf 'udev rules must not depend on transport-specific input names\n' >&2
    exit 1
fi

echo 'Fedora package file layout is valid'
