# Packaging

System integration design: ADR-0005. Files: `packaging/`, `scripts/rpm-build`.

## RPM (`packaging/rpm/sliver.spec`)
| package | contents |
|---|---|
| `sliver` | metapackage: sliverd + sliver-agent + sliverctl, Recommends sliver-settings. Docs, examples, bundled-crate licenses |
| `sliverd` | `/usr/libexec/sliverd`, `sliverd.{service,socket}` (+ alias `sliver.service`), `tiny-dfr.service.d/50-sliver.conf` (handover, ADR-0005), `99-z-sliverd.rules`. Recommends `tiny-dfr` (fallback) |
| `sliver-agent` | `/usr/bin/sliver-agent`, user unit `sliver-agent.service` (`Type=dbus`), D-Bus activation file for `io.github.wawow830.Sliver`, user preset (enabled by default) |
| `sliverctl` | `/usr/bin/sliverctl` |
| `sliver-settings` | `/usr/bin/sliver-settings`, `.desktop`, AppStream metainfo |
| `gnome-shell-extension-sliver` | the GNOME Shell extension in `/usr/share/gnome-shell/extensions/sliver-focus@…` (noarch). Installed with GNOME (`Supplements:`), never enabled |
| `sliver-kwin` | the KWin script in `/usr/share/kwin/scripts/sliver-focus` (noarch). Installed with KWin (`Supplements:`), never enabled |
| `sliver-browser` | `/usr/libexec/sliver-browser-host`, native-messaging manifests for Firefox (`%{_libdir}/mozilla`), Chromium and Chrome (`/etc`), extension folders in `/usr/share/sliver/browser`. Optional, not pulled in by `sliver`. Needs python3-gobject-base |

Built offline from a working-tree snapshot plus a `cargo vendor` tarball, using Fedora's cargo
macros (`%cargo_prep -v vendor`, `%cargo_build`, `%cargo_license`, `%cargo_vendor_manifest`).
`%check` validates the desktop/metainfo files and runs the hardware-free test suites (`--without
check` skips them). Man pages live in `man/` and are installed by the spec;
`crates/sliverctl/tests/man_pages.rs` checks them against the CLI and config keys.
`sliver-hwprobe` (hardware debugging) is not packaged.

```
scripts/rpm-build                    # SRPM → target/rpmbuild/SRPMS (~30 s)
scripts/rpm-build --binary           # + RPMs → target/rpmbuild/RPMS (release build; the vendored
                                     # crates stay compiled in target/rpm-cache, so later builds
                                     # only recompile sliver's own)
```
`scripts/check` runs `rpmspec -P` on the spec when rpm is installed.

### Install behaviour
- **Rules applied at install:** `%post` reloads udev and sends a `change` event to the bar's DRM card
  (`/dev/sliver_display` alias, needed by `BindsTo=`) and the bar backlight (group-writable). Without
  this, a `systemctl start sliverd` before the next reboot times out waiting for the device and falls
  back to tiny-dfr. If the backlight still isn't writable, sliverd exits so systemd falls back to
  tiny-dfr (working dim/off, no OLED burn-in). Only without tiny-dfr installed does it run at a
  fixed brightness, logging a warning.
- **No takeover at install.** A fresh install starts nothing. sliverd's udev rule applies from the
  next boot (or the next digitizer `add` event). Switch right away with `sudo systemctl start
  sliverd`: its `ExecStartPre=` stops tiny-dfr. Back with `sudo systemctl start tiny-dfr` (the
  drop-in stops sliverd and its socket).
- **Uninstall gives the bar back:** `%preun` stops sliverd, and `%postun` starts tiny-dfr (if it's
  installed) so Esc/F-keys don't wait for a reboot.
- The agent's user unit is enabled for all users by the preset. It starts with
  `graphical-session.target`, and only if sliverd is installed.

### Upgrades
- Upgrading restarts sliverd (`%systemd_postun_with_restart`), which costs ~0.5 s of bar. Running
  agents are restarted too (`%systemd_user_postun_with_restart`, in every user manager); `%postun`
  runs from the *old* package, so this only applies to upgrades from a package that already has
  it. An agent that isn't restarted reconnects to the new sliverd on its own.
- If sliverd was start-limited at the time, the upgrade still brings it up: restarting the failed
  `sliverd.socket` lets a waiting agent connect, which socket-activates sliverd once the 60 s
  start-limit window has passed.
- **udev `change` events on the digitizers start sliverd.** systemd acts on their
  `SYSTEMD_WANTS=sliverd.service` for `change` too, despite what systemd.device(5) suggests: with
  tiny-dfr in charge, one `udevadm trigger --action=change` on the digitizer makes sliverd take
  over. So `%post` sends none there (checked by `scripts/check`). Anything else that
  triggers them (a broad `udevadm trigger`, replugging) also brings sliverd back, so "going back to
  tiny-dfr" with `systemctl start tiny-dfr` only lasts until the next such event or boot.
- The RPM's own sliverd is the only real-hardware code that may be restarted without the harness.
  If it fails, the fallback hands the bar to tiny-dfr.

## Testing the packaging
- `scripts/check` (always): unit/rule syntax, and the lines that must not regress (no
  `Conflicts=tiny-dfr`, the handover commands, no udev trigger on the digitizers).
- `packaging/itest-units.sh` (~10 s, a user systemd manager): the units' handover and fallback
  logic with stand-in services: crash loop → tiny-dfr, preemption both ways, the refused-start hole,
  both started at once, no tiny-dfr installed.
- `packaging/itest-rpm.sh [RPM_DIR]` (~1 min, podman): the real RPMs in a Fedora container
  with systemd as PID 1. Install starts nothing and leaves tiny-dfr alone, the agent's user preset
  enables it, an upgrade doesn't take the bar from tiny-dfr but does restart a running sliverd,
  uninstalling a running sliverd hands the bar to tiny-dfr and removes our drop-in and rule, and all
  of it works without tiny-dfr. Only sliverd's `[Service]` is a stand-in (no bar in a container).
  Run it after changing the spec's scriptlets or the units.

## SELinux and polkit
- **SELinux: no policy module needed** (enforcing). sliverd runs as `unconfined_service_t` (a `bin_t`
  binary started by systemd, the same domain tiny-dfr gets). The agent and apps run as the user's
  `unconfined_t`, and the socket is `var_run_t`. No sliver process causes AVC denials; check with
  `ausearch -m AVC -ts boot`. Confinement
  on top (a `sliverd_t` module covering DRM, input, uinput, sysfs backlight and the `/run/sliver`
  socket) would add risk for little gain, since systemd's sandbox (DynamicUser, no capabilities, a
  device allowlist, a syscall filter) already does the confining. Revisit if Fedora ever confines
  `unconfined_service_t`.
- **polkit: nothing to ship.** No sliver component defines or checks a polkit action. sliverd
  authorises agents itself (seat0's active uid). Screen and keyboard-backlight sliders use logind's
  `SetBrightness`, which logind allows for the active session without polkit. Everything the
  agent launches runs as the user. (The safe-swap harness policy is a dev tool and isn't packaged.)

## COPR
The spec builds in COPR from an SRPM upload, or with the "SCM" source type running
`scripts/rpm-build`. The chroots are `fedora-*-aarch64` (Asahi) and `fedora-*-x86_64` (t2linux).
Publishing there is a public release, so it needs the maintainer's OK.

## Bundled-crate licenses
The spec's `bundled_licenses` is written by hand. Refresh it when dependencies change:
```
RUSTC_BOOTSTRAP=1 cargo tree -Zavoid-dev-deps --workspace --edges=no-build,no-dev,no-proc-macro \
  --target=all --prefix=none --format '# {l}' | sort -u
```
(`cargo2rpm license-summary` needs the vendored tree.) Sliver itself is MIT OR Apache-2.0.
