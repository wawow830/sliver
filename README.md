# sliver

Sliver makes the Touch Bar on Linux Macs part of the desktop: waybar-style modules, layouts and
stylesheets, a D-Bus API for apps (`docs/dbus/`, `examples/`), replacing tiny-dfr. It never trades
away: (1) Esc and the F-keys are always reachable, (2) a hardware experiment never strands the
user, (3) it's cheap on power, (4) all of it is testable without hardware.

Install: `scripts/rpm-build --binary`, `sudo dnf install target/rpmbuild/RPMS/*/*.rpm`, then
`sudo systemctl start sliverd`. `sliverctl doctor` says what's wrong; `man sliver-config` is the
reference. Bar stuck? Double-press Fn and hold: the F-keys. Back to tiny-dfr: remove the packages.

MIT or Apache-2.0; bundled fonts and icons keep their licenses (beside them).
