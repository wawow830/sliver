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
| `gnome-shell-extension-sliver` | the GNOME Shell extension in `/usr/share/gnome-shell/extensions/sliver-focus@…` (noarch). Installed with GNOME (`Supplements:`), never enabled |
| `sliver-kwin` | the KWin script in `/usr/share/kwin/scripts/sliver-focus` (noarch). Installed with KWin (`Supplements:`), never enabled |
| `sliver-browser` | `/usr/libexec/sliver-browser-host`, native-messaging manifests for Firefox (`%{_libdir}/mozilla`), Chromium and Chrome (`/etc`), extension folders in `/usr/share/sliver/browser`. Optional, not pulled in by `sliver`. Needs python3-gobject-base |

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

## Upgrades (seen in T94)
- Upgrading restarts sliverd (`%systemd_postun_with_restart`). If sliverd was start-limited at the
  time, the upgrade still brings it up: restarting the failed `sliverd.socket` lets a waiting agent
  connect, which socket-activates sliverd once the 60 s start-limit window has passed (journal,
  T94). A fresh install starts nothing (no socket, no agent yet).
- **udev `change` events on the digitizers start sliverd.** systemd acts on their
  `SYSTEMD_WANTS=sliverd.service` for `change` too, despite what systemd.device(5) suggests
  (measured: tiny-dfr in charge, one `udevadm trigger --action=change` on the digitizer, sliverd
  took over). So `%post` sends none there (checked by `scripts/check`). Before this was fixed, the T96
  digitizer trigger made every install and upgrade take the bar over. Anything else that triggers
  them (a broad `udevadm trigger`, replugging) also brings sliverd back. "Going back to tiny-dfr"
  with `systemctl start tiny-dfr` only lasts until the next such event or boot. Since the agent spec fix,
  running agents are restarted too (`%systemd_user_postun_with_restart`, in every user manager).
  `%postun` runs from the *old* package, so this works for upgrades away from .263 (737ff05) on.
  The .261 → .263 upgrade still kept the old agent, which reconnected fine.
  **Verified on .263 → .290 (T115):** the running agent restarted by itself (new PID), sliverd
  had 0 restarts, and there were no AVCs. Idle cost with the display off (lid closed): sliverd 0
  wakeups/s, agent 1.45/s (within `scripts/idle-wakeups`' 3/s budget).
  Before that fix, the old agent kept running against the new sliverd until the next login.
- The RPM's own sliverd is the only real-hardware code the swarm may restart without the harness.
  If it fails, the fallback hands the bar to tiny-dfr. That happened on the .256 upgrade: the
  cgroup gate didn't know about the unit's private cgroup namespace (fixed in 59a1eb4).

## Testing the packaging
- `scripts/check` (always): unit/rule syntax, and the lines that must not regress (no
  `Conflicts=tiny-dfr`, the handover commands, no udev trigger on the digitizers).
- `packaging/itest-units.sh` (~10 s, a user systemd manager): the units' handover and fallback
  logic with stand-in services: crash loop → tiny-dfr, preemption both ways, the refused-start hole,
  both started at once, no tiny-dfr installed.
- `packaging/itest-rpm.sh [RPM_DIR]` (~1 min, podman; T131): the real RPMs in a Fedora container
  with systemd as PID 1. Install starts nothing and leaves tiny-dfr alone, the agent's user preset
  enables it, an upgrade doesn't take the bar from tiny-dfr but does restart a running sliverd,
  uninstalling a running sliverd hands the bar to tiny-dfr and removes our drop-in and rule, and all
  of it works without tiny-dfr. Only sliverd's `[Service]` is a stand-in (no bar in a container).
  Run it after changing the spec's scriptlets or the units. It found that sliverd couldn't start
  where tiny-dfr isn't installed (the handover's `systemctl stop` failed; fixed).

## SELinux and polkit (T76, checked on the installed Mac14,7, enforcing)
- **SELinux: clean, no policy module needed.** sliverd runs as `unconfined_service_t` (a `bin_t`
  binary started by systemd, the same domain tiny-dfr gets). The agent and apps run as the user's
  `unconfined_t`, and the socket is `var_run_t`. `ausearch -m AVC -ts boot` showed no denials for any
  sliver process across install, start, fallback, harness runs and normal use. Confinement on top
  (a `sliverd_t` module) would add risk for little gain, since systemd's sandbox (DynamicUser, no
  capabilities, a device allowlist, a syscall filter) already does the confining. Revisit if Fedora
  ever confines `unconfined_service_t`.
- **polkit: nothing to ship.** No sliver component defines or checks a polkit action. sliverd
  authorises agents itself (seat0's active uid). Screen and keyboard-backlight sliders use logind's
  `SetBrightness`, which logind allows for the active session without polkit. Everything the
  agent launches runs as the user.

## COPR
The spec is COPR-ready (SRPM upload, or the "SCM" source type running `scripts/rpm-build`).
Creating the COPR project is an `external` ask for the human (H46, 2026-10-12: not now; local RPMs
via `scripts/rpm-build --binary`). Chroots: `fedora-*-aarch64`
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
