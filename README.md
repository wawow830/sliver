# sliver

Sliver makes the Touch Bar on Linux Macs part of the desktop: waybar-style modules (`examples/`),
layouts and stylesheets, driven by `sliverctl` (`docs/dbus/`), replacing tiny-dfr. It never trades
away: (1) Esc and the F-keys are always reachable, (2) a hardware experiment never strands the
user, (3) it's cheap on power, (4) all of it is testable without hardware.

Install: `scripts/rpm-build --binary`, `sudo dnf install target/rpmbuild/RPMS/*/*.rpm`, then
`sudo systemctl start sliverd`. `sliverctl doctor` says what's wrong; `man sliver-config` is the
reference. Bar stuck? Double-press Fn and hold: the F-keys. Back to tiny-dfr: remove the packages.

Hacking: `sliverd` owns the bar and its safe layer; `sliver-agent`, in the session, tells it what
to draw (`crates/`; each crate's `//!` says why it exists). `scripts/check` is the gate
(CI runs it; needs the packages in `.github/workflows/ci.yml`). Slower, by hand: `integration/*/`,
`packaging/itest-*.sh`, `scripts/soak`, `scripts/idle-wakeups`; real hardware only via
`harness/safeswap/sliver-safeswap`.

MIT or Apache-2.0; bundled fonts and icons keep their licenses (beside them).
