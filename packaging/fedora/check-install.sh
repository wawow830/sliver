#!/bin/sh
set -eu

root=${1:?usage: check-install.sh BUILDROOT}

required='
x:/usr/bin/sliver
x:/usr/libexec/sliver/sliver-broker
x:/usr/libexec/sliver/sliver-supervisor
x:/usr/libexec/sliver/sliver-lua-worker
f:/usr/lib/systemd/system/sliver-broker.service
f:/usr/lib/systemd/user/sliver-supervisor.service
f:/usr/lib/systemd/user/sliver-lua-worker-.service.d/50-defaults.conf
f:/usr/lib/udev/rules.d/70-sliver.rules
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
find "$root" -type f -printf '/%P\n' | sort > "$actual"
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
grep -F 'MODE="0660"' "$root/usr/lib/udev/rules.d/70-sliver.rules" >/dev/null
grep -F 'ID_SEAT}=="seat-touchbar"' "$root/usr/lib/udev/rules.d/70-sliver.rules" >/dev/null
grep -F 'ID_INPUT_TOUCHSCREEN}=="1"' "$root/usr/lib/udev/rules.d/70-sliver.rules" >/dev/null
grep -F 'ID_INPUT_KEYBOARD}=="1"' "$root/usr/lib/udev/rules.d/70-sliver.rules" >/dev/null
if grep -F 'ATTRS{name}' "$root/usr/lib/udev/rules.d/70-sliver.rules" >/dev/null; then
    printf 'udev rules must not depend on transport-specific input names\n' >&2
    exit 1
fi

echo 'Fedora package file layout is valid'
