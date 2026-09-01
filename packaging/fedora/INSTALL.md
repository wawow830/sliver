# Fedora Asahi package

The RPM installs one command in `PATH`, `/usr/bin/sliver`. The broker,
supervisor, and disposable worker live in `/usr/libexec/sliver`; systemd
starts them only after explicit enablement. The embedded default is stored in
the worker binary and is not installed as an editable file.

The package creates the `sliver` system account and the
`sliver-supervisors` group. Add users who should apply a configuration to that
group, then give the broker account a user manager for its pre-login worker:

```sh
sudo usermod --append --groups sliver-supervisors "$USER"
sudo loginctl enable-linger sliver
```

Installation does not enable either unit. Reload udev rules after an install or
upgrade, log out and back in after changing group membership, then perform the
takeover explicitly:

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

## Package checks

Build from the exact clean commit that will be tested. The source archive
expands `release-commit` to that commit, and the verifier rejects a package
with a different identity.

```sh
mkdir -p ~/rpmbuild/SOURCES
version=$(awk '$1 == "Version:" { print $2 }' packaging/fedora/sliver.spec)
git archive --format=tar.gz --prefix="sliver-${version}/" \
  -o "$HOME/rpmbuild/SOURCES/sliver-${version}.tar.gz" HEAD
cp packaging/fedora/sliver.sysusers ~/rpmbuild/SOURCES/
rpmbuild -ba packaging/fedora/sliver.spec 2>&1 | tee "$HOME/sliver-rpmbuild.log"
rpm=$(find ~/rpmbuild/RPMS -name 'sliver-*.rpm' -type f | sort | tail -n1)
rpm -qlp "$rpm"
```

The build must run the complete `%check`, including the packaged worker's
pure-Lua and compiled Lua 5.4 module checks, and must end with the exact
manifest checks. Do not use a build log that says the user-manager tests were
skipped. `packaging/fedora/check-install.sh` is the buildroot-side manifest
check; `packaging/fedora/release-manifest.txt` is its expected file list.

## Physical release verification

Run the verifier from a persistent local terminal on the tested Fedora Asahi
Mac14,7. It requires a TTY and an active, non-remote `seat0` graphical
session. It captures state before any change and records each acceptance item
separately. It asks before installation, account and udev setup, takeover,
service restart, suspend, and rollback.

```sh
scripts/verify-release.sh --build-log "$HOME/sliver-rpmbuild.log" "$rpm"
```

The account and udev stage stops before logout. After logging out and back in
locally, continue from a new terminal using the printed command:

```sh
scripts/verify-release.sh --resume "$HOME/sliver-release-verification/YYYYMMDD-HHMMSS"
```

The verifier creates named fixtures in the evidence directory:
`valid.lua`, `invalid.lua`, `hung.lua`, and `video-2008x60.lua`. The last one
redraws a complete native 2008 by 60 RGBA frame at 60 Hz. The text in the
named drawing fixtures uses the canvas's top-origin coordinate, not a baseline;
keep text above the bottom edge when adding or editing a fixture. The wizard asks
for the real-panel interval, presented rate, misses, input-to-frame delay, and
latency growth. Enter measured values only.

A failed run automatically attempts a full rollback. It restores only state
captured before this run, removes the fresh RPM and package-created account
and groups, restores linger and udev loading, and restores the selected-path
file including a final symlink. It verifies every restoration. If a command is
interrupted, rerun the printed `--rollback DIR` command from a local terminal.

`--service-only DIR` is different. It stops the Sliver takeover and restores
the captured service state, but deliberately leaves the RPM, account, linger,
udev, and selected-path changes in place. Use it only when retaining that
installation is intentional. It is not a full rollback.

Do not run the verifier over SSH. Do not close the release issues from a
partial log, a skipped check, or an unobserved physical behavior.

## Return to tiny-dfr

Stopping Sliver does not restart another daemon automatically:

```sh
sudo systemctl stop sliver-broker.service
sudo systemctl stop tiny-dfr.service  # only if it was started separately
sudo systemctl start tiny-dfr.service
```

The verifier's full rollback runs the equivalent service changes only when
they differ from its captured pre-run state.
