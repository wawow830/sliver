#!/bin/sh
set -eu

root=${1:?usage: check-install.sh BUILDROOT}

required='
/usr/bin/sliver
/usr/libexec/sliver/sliver-broker
/usr/libexec/sliver/sliver-supervisor
/usr/libexec/sliver/sliver-lua-worker
/usr/lib/systemd/system/sliver-broker.service
/usr/lib/systemd/user/sliver-supervisor.service
/usr/lib/systemd/user/sliver-lua-worker-.service.d/50-defaults.conf
/usr/lib/udev/rules.d/70-sliver.rules
/usr/lib/sysusers.d/sliver.conf'

for path in $required; do
    test -e "$root$path" || {
        printf 'missing packaged file: %s\n' "$path" >&2
        exit 1
    }
done

test -x "$root/usr/bin/sliver"
test -x "$root/usr/libexec/sliver/sliver-broker"
test -x "$root/usr/libexec/sliver/sliver-supervisor"
test -x "$root/usr/libexec/sliver/sliver-lua-worker"

for path in \
    /usr/bin/sliverd \
    /usr/bin/sliver-edit \
    /usr/bin/sliver-broker \
    /usr/bin/sliver-supervisor \
    /usr/bin/sliver-lua-worker \
    /usr/share/sliver/default.lua \
    /usr/share/sliver/sliver.toml; do
    test ! -e "$root$path" || {
        printf 'unexpected public or editable file: %s\n' "$path" >&2
        exit 1
    }
done

grep -F 'User=sliver' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'Group=sliver-supervisors' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'RuntimeDirectoryMode=0750' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'SupplementaryGroups=video input' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'ExecStart=/usr/libexec/sliver/sliver-broker' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'Conflicts=tiny-dfr.service' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'Before=tiny-dfr.service' \
    "$root/usr/lib/systemd/system/sliver-broker.service" >/dev/null
grep -F 'ExecStart=/usr/libexec/sliver/sliver-supervisor' \
    "$root/usr/lib/systemd/user/sliver-supervisor.service" >/dev/null
grep -F 'PrivateDevices=yes' \
    "$root/usr/lib/systemd/user/sliver-lua-worker-.service.d/50-defaults.conf" >/dev/null
grep -F 'MODE="0660"' "$root/usr/lib/udev/rules.d/70-sliver.rules" >/dev/null

echo 'Fedora package file layout is valid'
