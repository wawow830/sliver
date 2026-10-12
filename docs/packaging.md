# Packaging

Owner: **hazel**. System integration design: ADR-0005. Files: `packaging/`, `scripts/rpm-build`.

## RPM (`packaging/rpm/sliver.spec`)
| package | contents |
|---|---|
| `sliver` | metapackage: sliverd + sliver-agent + sliverctl, Recommends sliver-settings. Docs, examples, bundled-crate licenses |
| `sliverd` | `/usr/libexec/sliverd`, `sliverd.{service,socket}` (+ alias `sliver.service`), `tiny-dfr.service.d/50-sliver.conf` (handover, ADR-0005), `99-z-sliverd.rules`. Recommends `tiny-dfr` (fallback) |
| `sliver-agent` | `/usr/bin/sliver-agent`, user unit `sliver-agent.service` (`Type=dbus`), D-Bus activation file for `io.github.wawow830.Sliver`, user preset (enabled by default) |
| `sliverctl` | `/usr/bin/sliverctl` |
| `sliver-settings` | `/usr/bin/sliver-settings`, `.desktop`, AppStream metainfo |

Built offline from a working-tree snapshot plus a `cargo vendor` tarball, using Fedora's cargo
macros (`%cargo_prep -v vendor`, `%cargo_build`, `%cargo_license`, `%cargo_vendor_manifest`).
`%check` validates the desktop/metainfo files and runs the hardware-free test suites (`--without
check` skips them). `sliver-hwprobe` (hardware debugging) is not packaged.

```
scripts/rpm-build                    # SRPM → target/rpmbuild/SRPMS (~30 s)
scripts/rpm-build --binary           # + RPMs → target/rpmbuild/RPMS (full release build)
```
`scripts/check` runs `rpmspec -P` on the spec when rpm is installed.

### Install behaviour
- **Rules applied at install:** `%post` reloads udev and sends a `change` event to the bar's DRM card
  (`/dev/sliver_display` alias, needed by `BindsTo=`) and the bar backlight (group-writable). Without this,
  the first `systemctl start sliverd` before a reboot timed out on the device and fell back to tiny-dfr
  (seen on the first real install). If the backlight still isn't writable, sliverd exits so systemd falls
  back to tiny-dfr (working dim/off, no OLED burn-in). Only without tiny-dfr installed does it run at a
  fixed brightness, logging a warning.
- **No takeover at install.** sliverd's udev rule applies from the next boot (or the next digitizer
  `add` event). Switch right away with `sudo systemctl start sliverd`: its `ExecStartPre=` stops tiny-dfr. Back with
  `sudo systemctl start tiny-dfr` (the drop-in stops sliverd and its socket).
- **Uninstall gives the bar back:** `%preun` stops sliverd, and `%postun` starts tiny-dfr (if it's
  installed) so Esc/F-keys don't wait for a reboot.
- **Upgrade** restarts sliverd (`%systemd_postun_with_restart`), which costs ~0.5 s of bar. The agent
  reconnects on its own.
- The agent's user unit is enabled for all users by the preset. It starts with
  `graphical-session.target`, and only if sliverd is installed.

## COPR
The spec is COPR-ready (SRPM upload, or the "SCM" source type running `scripts/rpm-build`).
Creating the COPR project is an `external` ask for the human. Chroots: `fedora-*-aarch64`
(Asahi) and `fedora-*-x86_64` (t2linux).

## Still open
- **Bundled-crate licenses:** the spec's `bundled_licenses` is hand-written. Refresh it when
  dependencies change: `RUSTC_BOOTSTRAP=1 cargo tree -Zavoid-dev-deps --workspace --edges=no-build,no-dev,no-proc-macro --target=all --prefix=none --format '# {l}' | sort -u`
  (`cargo2rpm license-summary` needs the vendored tree). Sliver itself: MIT OR Apache-2.0 (H27).
- **SELinux:** sliverd runs `unconfined_service_t` (like tiny-dfr). It needs nothing beyond standard
  device access, and DynamicUser is fine under targeted policy. A real policy module (DRM, input,
  uinput, sysfs backlight, `/run/sliver` socket) is future work. Check the first install with
  `ausearch -m avc -ts recent`.
- **polkit:** nothing needed. sliverd authorises agents by seat0 session ownership, the agent is
  per user, and nothing asks for privileges at runtime. (The safe-swap harness policy is a dev tool
  and isn't packaged.)
- **Man pages** (T42, cedar): `man/`, installed by the spec. `crates/sliverctl/tests/man_pages.rs` checks them against the CLI and config keys.
- **Icon** for the settings app: it uses the theme's `input-keyboard` until we have one.
