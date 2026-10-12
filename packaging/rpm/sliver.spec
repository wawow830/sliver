# Sliver: the Touch Bar platform for Linux Macs. Built from a git snapshot plus a cargo vendor
# tarball (offline, so it works in mock/COPR). Local build: scripts/rpm-build. See docs/packaging.md.

# Sliver's own license (H27: MIT OR Apache-2.0, like most Rust projects).
%global sliver_license MIT OR Apache-2.0
# Bundled crates (cargo tree --format '{l}', see docs/packaging.md) plus embedded assets
# (Noto Sans: OFL-1.1, Material icons: Apache-2.0).
%global bundled_licenses (0BSD OR MIT OR Apache-2.0) AND Apache-2.0 AND (Apache-2.0 OR GPL-2.0-only) AND (Apache-2.0 OR MIT) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND BSD-2-Clause AND BSD-3-Clause AND MIT AND (MIT OR Apache-2.0) AND (MIT OR Apache-2.0 OR LGPL-2.1-or-later) AND (MIT OR Zlib OR Apache-2.0) AND (Unlicense OR MIT) AND Unicode-3.0 AND Zlib AND (Zlib OR Apache-2.0 OR MIT) AND OFL-1.1

%bcond check 1

Name:           sliver
Version:        0.1.0
Release:        0.1%{?snapshot:.%{snapshot}}%{?dist}
Summary:        Touch Bar platform and customizer for Linux Macs
URL:            https://github.com/wawow830/sliver
License:        (%{sliver_license}) AND %{bundled_licenses}
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

# Touch Bar Macs: Apple Silicon (Asahi) and Intel T2 (t2linux).
ExclusiveArch:  aarch64 x86_64

BuildRequires:  cargo-rpm-macros >= 26
BuildRequires:  pkgconfig(gtk4) >= 4.22
BuildRequires:  pkgconfig(libadwaita-1) >= 1.8
BuildRequires:  systemd-rpm-macros
BuildRequires:  desktop-file-utils
BuildRequires:  appstream
%if %{with check}
BuildRequires:  dbus-daemon
%endif

Requires:       sliverd = %{version}-%{release}
Requires:       sliver-agent = %{version}-%{release}
Requires:       sliverctl = %{version}-%{release}
Recommends:     sliver-settings = %{version}-%{release}

%description
Sliver turns the Touch Bar on Linux Macs into a first-class part of the Linux desktop: layouts,
per-app contexts, widgets, gestures and appearance, plus a public D-Bus API so every app and
script can put things on the bar. It replaces tiny-dfr, and Escape and the F-keys always stay
reachable. This metapackage installs the daemon, the session agent and the CLI.

%package -n sliverd
Summary:        Sliver Touch Bar system daemon
License:        (%{sliver_license}) AND %{bundled_licenses}
%{?systemd_requires}
Requires:       systemd-udev
# The fallback when sliverd can't run (OnFailure=tiny-dfr.service, ADR-0005). The two coexist:
# sliverd's udev rule takes over the bar, and removing sliverd gives it back to tiny-dfr.
Recommends:     tiny-dfr

%description -n sliverd
sliverd owns the Touch Bar display, touch input, backlight and key emission. It shows a built-in
safe layer (F-keys, media keys, Fn double-press-hold safe hatch) whenever no session agent drives
the bar. It runs unprivileged and is started by udev in place of tiny-dfr. If it keeps failing,
systemd hands the bar back to tiny-dfr.

%package -n sliver-agent
Summary:        Sliver Touch Bar session agent
License:        (%{sliver_license}) AND %{bundled_licenses}
Requires:       sliverd = %{version}-%{release}

%description -n sliver-agent
The per-user agent: reads ~/.config/sliver, follows the focused app, workspace and system state,
renders the bar and drives sliverd. It serves the public io.github.wawow830.Sliver D-Bus API.

%package -n sliverctl
Summary:        Command-line control for the Sliver Touch Bar
License:        (%{sliver_license}) AND %{bundled_licenses}

%description -n sliverctl
sliverctl switches layers and profiles, edits and checks the config, publishes items from
scripts, and imports tiny-dfr and waybar configs.

%package -n sliver-settings
Summary:        Sliver Touch Bar settings app
License:        (%{sliver_license}) AND %{bundled_licenses}
Requires:       sliver-agent = %{version}-%{release}

%description -n sliver-settings
GTK4/libadwaita settings app with a live WYSIWYG editor for Touch Bar layouts.

%prep
%autosetup -n %{name}-%{version} -p1 -a1
%cargo_prep -v vendor

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies
%cargo_vendor_manifest

%install
install -Dpm0755 target/rpm/sliverd %{buildroot}%{_libexecdir}/sliverd
install -Dpm0755 target/rpm/sliver-agent %{buildroot}%{_bindir}/sliver-agent
install -Dpm0755 target/rpm/sliverctl %{buildroot}%{_bindir}/sliverctl
%{_mandir}/man1/sliverctl.1*
install -Dpm0755 target/rpm/sliver-settings %{buildroot}%{_bindir}/sliver-settings

install -Dpm0644 packaging/systemd/sliverd.service %{buildroot}%{_unitdir}/sliverd.service
install -Dpm0644 packaging/systemd/sliverd.socket %{buildroot}%{_unitdir}/sliverd.socket
install -Dpm0644 packaging/udev/99-z-sliverd.rules %{buildroot}%{_udevrulesdir}/99-z-sliverd.rules
%{_mandir}/man8/sliverd.8*
install -Dpm0644 packaging/systemd/user/sliver-agent.service %{buildroot}%{_userunitdir}/sliver-agent.service
install -Dpm0644 packaging/systemd/user-preset/90-sliver.preset %{buildroot}%{_userpresetdir}/90-sliver.preset
%{_mandir}/man1/sliver-agent.1*
%{_mandir}/man5/sliver-config.5*
%{_mandir}/man5/sliver-stylesheet.5*
# T36: D-Bus activation once the agent owns the bus name:
# install -Dpm0644 packaging/dbus/io.github.wawow830.Sliver.service %%{buildroot}%%{_datadir}/dbus-1/services/io.github.wawow830.Sliver.service

install -Dpm0644 man/sliverctl.1 %{buildroot}%{_mandir}/man1/sliverctl.1
install -Dpm0644 man/sliver-agent.1 %{buildroot}%{_mandir}/man1/sliver-agent.1
install -Dpm0644 man/sliverd.8 %{buildroot}%{_mandir}/man8/sliverd.8
install -Dpm0644 man/sliver-config.5 %{buildroot}%{_mandir}/man5/sliver-config.5
install -Dpm0644 man/sliver-stylesheet.5 %{buildroot}%{_mandir}/man5/sliver-stylesheet.5

install -Dpm0644 packaging/desktop/io.github.wawow830.Sliver.Settings.desktop \
    %{buildroot}%{_datadir}/applications/io.github.wawow830.Sliver.Settings.desktop
install -Dpm0644 packaging/desktop/io.github.wawow830.Sliver.Settings.metainfo.xml \
    %{buildroot}%{_metainfodir}/io.github.wawow830.Sliver.Settings.metainfo.xml
sed -i 's|@LICENSE@|%{sliver_license}|' %{buildroot}%{_metainfodir}/io.github.wawow830.Sliver.Settings.metainfo.xml

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/*.desktop
appstreamcli validate --no-net --explain %{buildroot}%{_metainfodir}/*.metainfo.xml
%if %{with check}
# Hardware-free crates only. The D-Bus suites need a session bus, which mock doesn't provide reliably.
%cargo_test -- -p sliverd -p sliver-ipc -p sliver-render -p sliver-config
%endif

%post -n sliverd
%systemd_post sliverd.service sliverd.socket
%udev_rules_update
# No takeover at install time: the bar switches to sliverd at the next boot, or now with
# `systemctl start sliverd` (Conflicts= stops tiny-dfr). docs/packaging.md.

%preun -n sliverd
%systemd_preun sliverd.service sliverd.socket

%postun -n sliverd
%systemd_postun_with_restart sliverd.service
%udev_rules_update
# Uninstalled while running: tiny-dfr gets the bar back at the next udev event or boot. Start it
# now so the user isn't left without Esc/F-keys until then (invariant 1).
if [ "$1" -eq 0 ] && [ -e %{_unitdir}/tiny-dfr.service ]; then
    systemctl start --no-block tiny-dfr.service >/dev/null 2>&1 || :
fi

%post -n sliver-agent
%systemd_user_post sliver-agent.service

%preun -n sliver-agent
%systemd_user_preun sliver-agent.service

%files
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies cargo-vendor.txt
%doc README.md GOAL.md docs/user-guide.md docs/api.md docs/customization.md docs/styling.md
%doc examples

%files -n sliverd
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies
%doc docs/sliverd.md docs/decisions/0005-sliverd-system-integration.md
%{_libexecdir}/sliverd
%{_unitdir}/sliverd.service
%{_unitdir}/sliverd.socket
%{_udevrulesdir}/99-z-sliverd.rules

%files -n sliver-agent
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies
%doc docs/agent.md
%{_bindir}/sliver-agent
%{_userunitdir}/sliver-agent.service
%{_userpresetdir}/90-sliver.preset

%files -n sliverctl
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies
%{_bindir}/sliverctl

%files -n sliver-settings
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies
%{_bindir}/sliver-settings
%{_datadir}/applications/io.github.wawow830.Sliver.Settings.desktop
%{_metainfodir}/io.github.wawow830.Sliver.Settings.metainfo.xml

%changelog
* Mon Oct 12 2026 hazel <hazel@sliver.invalid> - 0.1.0-0.1
- First package: sliverd, sliver-agent, sliverctl, sliver-settings
