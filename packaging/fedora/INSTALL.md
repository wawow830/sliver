# Fedora Asahi package

The RPM installs one command in `PATH`, `/usr/bin/sliver`. The broker,
supervisor, and disposable worker live in `/usr/libexec/sliver`; they are only
started by systemd. `default.lua` and `sliver.toml` are not installed. The
default is embedded in the worker binary.

The RPM creates the `sliver` system account and the `sliver-supervisors` group.
Add each user who should apply a configuration to that group, then start a
user manager for the broker account so its pre-login worker can run:

```sh
sudo usermod --append --groups sliver-supervisors "$USER"
sudo loginctl enable-linger sliver
```

Installation does not enable either unit. Reload the udev rules after an
installation or upgrade, log out and back in after changing group membership,
then perform the takeover explicitly:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=drm
sudo udevadm trigger --subsystem-match=input
sudo udevadm trigger --subsystem-match=misc
sudo udevadm trigger --subsystem-match=backlight
```

Enable takeover with one administrator operation:

```sh
sudo systemctl enable --now sliver-broker.service && \
  sudo systemctl --global enable sliver-supervisor.service
```

The broker conflicts with `tiny-dfr.service` and starts before it. Stopping the
broker does not start `tiny-dfr` again. Start `tiny-dfr` manually when handing
back ownership.

The udev rules grant the broker device-specific access through dedicated
`sliver-drm`, `sliver-input`, and `sliver-backlight` groups: the Asahi DRM
card, the Mac14,7 Touch Bar, the Apple MTP keyboard, uinput, and the DSI
backlight. Lua workers run in transient
user units with private devices, closed device policy, no new privileges, a
512 MiB memory limit, and a 64-task limit. Their output goes to the user
journal. Broker output goes to the system journal.

## Package checks

Build and inspect the RPM with Fedora's normal tools:

```sh
mkdir -p ~/rpmbuild/SOURCES
version=$(awk '$1 == "Version:" { print $2 }' packaging/fedora/sliver.spec)
git archive --format=tar.gz --prefix="sliver-${version}/" \
  -o "$HOME/rpmbuild/SOURCES/sliver-${version}.tar.gz" HEAD
cp packaging/fedora/sliver.sysusers ~/rpmbuild/SOURCES/
rpmbuild -ba packaging/fedora/sliver.spec
rpm -qlp ~/rpmbuild/RPMS/$(uname -m)/sliver-*.rpm
```

The file list should contain `/usr/bin/sliver`, the three files under
`/usr/libexec/sliver`, both systemd service definitions, the worker drop-in,
the sysusers file, and the udev rule. It must not contain a second
`default.lua`, `sliver.toml`, `sliverd`, or `sliver-edit` executable.

On a tested Mac14,7, verify a package install with these checks:

1. Before enablement, `systemctl is-enabled sliver-broker.service` reports
   `disabled` or `not-found`, and `tiny-dfr` still owns the panel.
2. Enable Sliver and confirm `tiny-dfr` stops, the broker runs as `sliver`, and
   the broker and supervisor journals are separate.
3. Check login, logout, user switching, broker restart, shutdown, DRM release,
   and device rediscovery. The panel must retain a frame during handoff and
   must not enter a service restart loop.
4. Stop Sliver and confirm that `tiny-dfr` remains stopped until an
   administrator starts it.
