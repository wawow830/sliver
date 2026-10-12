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
BuildRequires:  pkgconfig(xkbcommon)
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

# Desktop integrations (T122): tell the agent the focused window and workspaces on GNOME and KDE.
# Installed alongside the desktop (Supplements:) but never enabled: each user turns them on.
%package -n gnome-shell-extension-sliver
Summary:        GNOME Shell extension that feeds the Sliver Touch Bar agent focus and workspaces
License:        %{sliver_license}
BuildArch:      noarch
Requires:       gnome-shell >= 45
Requires:       sliver-agent
Supplements:    (sliver-agent and gnome-shell)
Provides:       sliver-gnome-shell = %{version}-%{release}

%description -n gnome-shell-extension-sliver
Tells sliver-agent which window and workspace are focused on GNOME Shell (io.github.wawow830.Sliver.Desktop1),
so per-app layouts, the workspaces module and the taskbar work there. Not enabled by default:
gnome-extensions enable sliver-focus@sliver.wawow830.github.io, then log out and back in.

%package -n sliver-kwin
Summary:        KWin script that feeds the Sliver Touch Bar agent focus and virtual desktops
License:        %{sliver_license}
BuildArch:      noarch
Requires:       kwin
Requires:       sliver-agent
Supplements:    (sliver-agent and kwin)

%description -n sliver-kwin
Tells sliver-agent which window and virtual desktop are active on KDE Plasma 6, so per-app layouts,
the workspaces module and the taskbar work there. Not enabled by default: System Settings → Window
Management → KWin Scripts → Sliver focus (or: kwriteconfig6 --file kwinrc --group Plugins
--key sliver-focusEnabled true, then qdbus6 org.kde.KWin /KWin reconfigure).

# Not noarch: Firefox's system manifest directory is %%{_libdir}/mozilla/native-messaging-hosts.
%package -n sliver-browser
Summary:        Touch Bar tabs and navigation for Firefox and Chromium (Sliver)
License:        %{sliver_license}
Requires:       sliver-agent = %{version}-%{release}
Requires:       python3
Requires:       python3-gobject-base
Requires:       mozilla-filesystem

%description -n sliver-browser
The native-messaging host and browser extension that give the focused Firefox or Chromium window
Touch Bar controls: back, forward, reload, new tab, and its tabs as a scrollable strip. The host
publishes them through Sliver's public D-Bus API. Load the extension from
%{_datadir}/sliver/browser (see the README there).

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
install -Dpm0755 target/rpm/sliver-settings %{buildroot}%{_bindir}/sliver-settings
# Browser integration (T106/T108): host, per-browser native-messaging manifests, and ready-to-load
# extension folders (what integration/browser/install.sh builds for a checkout).
install -Dpm0755 integration/browser/host/sliver-browser-host %{buildroot}%{_libexecdir}/sliver-browser-host
nm=io.github.wawow830.sliver_browser
install -Dpm0644 integration/browser/native/$nm.firefox.json \
    %{buildroot}%{_libdir}/mozilla/native-messaging-hosts/$nm.json
for d in %{_sysconfdir}/chromium %{_sysconfdir}/opt/chrome; do
    install -Dpm0644 integration/browser/native/$nm.chromium.json \
        %{buildroot}$d/native-messaging-hosts/$nm.json
done
for b in firefox chromium; do
    install -Dpm0644 integration/browser/extension/background.js \
        %{buildroot}%{_datadir}/sliver/browser/$b/background.js
    install -Dpm0644 integration/browser/extension/manifest.$b.json \
        %{buildroot}%{_datadir}/sliver/browser/$b/manifest.json
done
install -Dpm0644 integration/browser/README.md %{buildroot}%{_datadir}/sliver/browser/README.md
# GNOME extension and KWin script. KWin 6.7 also reads /usr/share/kwin/scripts (checked in a nested
# kwin_wayland), and Plasma 6.0-6.3 only read it.
gs=sliver-focus@sliver.wawow830.github.io
for f in metadata.json extension.js; do
    install -Dpm0644 integration/gnome-shell/$gs/$f %{buildroot}%{_datadir}/gnome-shell/extensions/$gs/$f
done
install -Dpm0644 integration/kwin/sliver-focus/metadata.json \
    %{buildroot}%{_datadir}/kwin/scripts/sliver-focus/metadata.json
install -Dpm0644 integration/kwin/sliver-focus/contents/code/main.js \
    %{buildroot}%{_datadir}/kwin/scripts/sliver-focus/contents/code/main.js

install -Dpm0644 packaging/systemd/sliverd.service %{buildroot}%{_unitdir}/sliverd.service
install -Dpm0644 packaging/systemd/sliverd.socket %{buildroot}%{_unitdir}/sliverd.socket
# `systemctl start sliver` works too (people type the project name).
ln -s sliverd.service %{buildroot}%{_unitdir}/sliver.service
install -Dpm0644 packaging/systemd/tiny-dfr.service.d/50-sliver.conf \
    %{buildroot}%{_unitdir}/tiny-dfr.service.d/50-sliver.conf
install -Dpm0644 packaging/udev/99-z-sliverd.rules %{buildroot}%{_udevrulesdir}/99-z-sliverd.rules
install -Dpm0644 packaging/systemd/user/sliver-agent.service %{buildroot}%{_userunitdir}/sliver-agent.service
install -Dpm0644 packaging/systemd/user-preset/90-sliver.preset %{buildroot}%{_userpresetdir}/90-sliver.preset
install -Dpm0644 packaging/dbus/io.github.wawow830.Sliver.service %{buildroot}%{_datadir}/dbus-1/services/io.github.wawow830.Sliver.service

install -Dpm0644 man/sliverctl.1 %{buildroot}%{_mandir}/man1/sliverctl.1
install -Dpm0644 man/sliver-agent.1 %{buildroot}%{_mandir}/man1/sliver-agent.1
install -Dpm0644 man/sliverd.8 %{buildroot}%{_mandir}/man8/sliverd.8
install -Dpm0644 man/sliver-config.5 %{buildroot}%{_mandir}/man5/sliver-config.5
install -Dpm0644 man/sliver-stylesheet.5 %{buildroot}%{_mandir}/man5/sliver-stylesheet.5
install -Dpm0644 man/sliver-settings.1 %{buildroot}%{_mandir}/man1/sliver-settings.1

install -Dpm0644 packaging/desktop/io.github.wawow830.Sliver.Settings.desktop \
    %{buildroot}%{_datadir}/applications/io.github.wawow830.Sliver.Settings.desktop
install -Dpm0644 packaging/desktop/io.github.wawow830.Sliver.Settings.metainfo.xml \
    %{buildroot}%{_metainfodir}/io.github.wawow830.Sliver.Settings.metainfo.xml
install -Dpm0644 packaging/mime/io.github.wawow830.Sliver.xml \
    %{buildroot}%{_datadir}/mime/packages/io.github.wawow830.Sliver.xml
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
# Apply 99-z-sliverd.rules to the devices that already exist, so `systemctl start sliverd` works
# without a reboot: the bar card's /dev/sliver_display alias (BindsTo=) and the backlight's
# permissions. The rules reload normally happens at the end of the transaction, too late for this.
# Only the bar's own card and backlight get a change event (not the main display's card).
udevadm control --reload >/dev/null 2>&1 || :
for card in /sys/class/drm/card[0-9]*; do
    case "$(basename "$(readlink -f "$card/device/driver" 2>/dev/null)")" in
        adp|appletbdrm) udevadm trigger --action=change "$card" >/dev/null 2>&1 || : ;;
    esac
done
udevadm trigger --action=change --subsystem-match=backlight \
    --sysname-match=228600000.dsi.0 --sysname-match=228200000.display-pipe.0 \
    --sysname-match=appletb_backlight >/dev/null 2>&1 || :
# Without tiny-dfr, our rules also own the bar's seat (the digitizers) and the T2 USB switch to
# the display configuration (an add-only rule; it acts only while the configuration is still 1).
for name in "Apple Inc. Touch Bar Display Touchpad" "MacBookPro17,1 Touch Bar" "Mac14,7 Touch Bar"; do
    udevadm trigger --action=change --subsystem-match=input --attr-match=name="$name" >/dev/null 2>&1 || :
done
udevadm trigger --action=add --subsystem-match=usb --attr-match=idVendor=05ac \
    --attr-match=idProduct=8302 >/dev/null 2>&1 || :
udevadm settle --timeout=5 >/dev/null 2>&1 || :
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

# Upgrades restart running agents (systemd-update-helper, every user manager), so nobody keeps
# the old agent against a new sliverd until their next login (seen in T94).
%postun -n sliver-agent
%systemd_user_postun_with_restart sliver-agent.service

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
%{_unitdir}/sliver.service
%dir %{_unitdir}/tiny-dfr.service.d
%{_unitdir}/tiny-dfr.service.d/50-sliver.conf
%{_udevrulesdir}/99-z-sliverd.rules
%{_mandir}/man8/sliverd.8*

%files -n sliver-agent
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies
%doc docs/agent.md
%{_bindir}/sliver-agent
%{_userunitdir}/sliver-agent.service
%{_userpresetdir}/90-sliver.preset
%{_mandir}/man1/sliver-agent.1*
%{_mandir}/man5/sliver-config.5*
%{_mandir}/man5/sliver-stylesheet.5*
%{_datadir}/dbus-1/services/io.github.wawow830.Sliver.service

%files -n sliverctl
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies
%{_bindir}/sliverctl
%{_mandir}/man1/sliverctl.1*

%files -n sliver-settings
%license LICENSE-MIT LICENSE-APACHE LICENSE.dependencies
%{_bindir}/sliver-settings
%{_mandir}/man1/sliver-settings.1*
%{_datadir}/applications/io.github.wawow830.Sliver.Settings.desktop
%{_metainfodir}/io.github.wawow830.Sliver.Settings.metainfo.xml
# Opens .sliver presets (shared-mime-info's file trigger updates the MIME cache).
%{_datadir}/mime/packages/io.github.wawow830.Sliver.xml

%files -n gnome-shell-extension-sliver
%license LICENSE-MIT LICENSE-APACHE
%{_datadir}/gnome-shell/extensions/sliver-focus@sliver.wawow830.github.io

%files -n sliver-kwin
%license LICENSE-MIT LICENSE-APACHE
%dir %{_datadir}/kwin
%dir %{_datadir}/kwin/scripts
%{_datadir}/kwin/scripts/sliver-focus

%files -n sliver-browser
%license LICENSE-MIT LICENSE-APACHE
%{_libexecdir}/sliver-browser-host
%{_libdir}/mozilla/native-messaging-hosts/io.github.wawow830.sliver_browser.json
# Shared directory ownership, like gnome-browser-connector and other native-messaging hosts.
%dir %{_sysconfdir}/chromium
%dir %{_sysconfdir}/chromium/native-messaging-hosts
%{_sysconfdir}/chromium/native-messaging-hosts/io.github.wawow830.sliver_browser.json
%dir %{_sysconfdir}/opt/chrome
%dir %{_sysconfdir}/opt/chrome/native-messaging-hosts
%{_sysconfdir}/opt/chrome/native-messaging-hosts/io.github.wawow830.sliver_browser.json
%dir %{_datadir}/sliver
%{_datadir}/sliver/browser

%changelog
* Mon Oct 12 2026 hazel <hazel@sliver.invalid> - 0.1.0-0.1
- First package: sliverd, sliver-agent, sliverctl, sliver-settings
