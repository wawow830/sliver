Name:           sliver
Version:        0.1.0
Release:        3%{?dist}
Summary:        Lua-scriptable Touch Bar service for Asahi Linux
License:        MIT OR Apache-2.0
URL:            https://github.com/wawow830/sliver
ExclusiveArch:  aarch64
Source0:        sliver-%{version}.tar.gz
Source1:        sliver.sysusers

BuildRequires:  cargo
BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  systemd-rpm-macros
BuildRequires:  pkgconfig(cairo)
BuildRequires:  pkgconfig(libsystemd)
BuildRequires:  pkgconfig(pango)
BuildRequires:  pkgconfig(pangocairo)
Requires:       systemd
Requires(pre):  systemd
Requires(post): systemd-udev
Requires(postun): systemd-udev
%systemd_requires

%description
Sliver owns the Apple Touch Bar on Asahi Linux through a non-root system
broker. Each user selects a Lua configuration through the sliver command. The
package includes systemd units for the broker, user supervisor, and disposable
Lua workers, but does not enable them or take over the Touch Bar during install.

%prep
%autosetup -n sliver-%{version}
%cargo_prep

%generate_buildrequires
%cargo_generate_buildrequires -t

%build
%cargo_build -- --package sliverd --bin sliver --bin sliver-broker --bin sliver-supervisor --bin sliver-lua-worker

%install
install -Dpm0755 target/release/sliver \
    %{buildroot}%{_bindir}/sliver
install -dpm0755 %{buildroot}%{_libexecdir}/sliver
for binary in sliver-broker sliver-supervisor sliver-lua-worker; do
    install -Dpm0755 "target/release/${binary}" \
        "%{buildroot}%{_libexecdir}/sliver/${binary}"
done

install -Dpm0644 systemd/sliver-broker.service \
    %{buildroot}%{_unitdir}/sliver-broker.service
install -Dpm0644 systemd/user/sliver-supervisor.service \
    %{buildroot}%{_userunitdir}/sliver-supervisor.service
install -Dpm0644 systemd/sliver-lua-worker-.service.d/50-defaults.conf \
    %{buildroot}%{_userunitdir}/sliver-lua-worker-.service.d/50-defaults.conf
install -Dpm0644 packaging/fedora/99-z-sliver.rules \
    %{buildroot}%{_udevrulesdir}/99-z-sliver.rules
install -Dpm0644 packaging/fedora/sliver.sysusers \
    %{buildroot}%{_sysusersdir}/sliver.conf

%check
# The production service tests exercise the transient user-service boundary.
# Fedora mock builds without a user manager may opt out explicitly, but an
# installed Fedora Asahi validation must run the complete suite.
if systemd-run --user --wait --quiet true; then
    SLIVER_LUA_WORKER=%{buildroot}%{_libexecdir}/sliver/sliver-lua-worker \
        %cargo_test -- --package sliverd --lib -- --test-threads=1
else
    echo 'Skipping user-manager integration tests: no systemd user manager' >&2
    SLIVER_LUA_WORKER=%{buildroot}%{_libexecdir}/sliver/sliver-lua-worker \
        %cargo_test -- --package sliverd --lib -- --skip systemd_worker_uses_the_declared_resource_and_device_policy --skip embedded_default_stays_healthy_through_systemd_worker_polling --skip production_peer_verification_accepts_a_real_supervisor_unit --test-threads=1
fi
%cargo_test -- --package sliverd --test sliver_cli
printf 'sliver package check: packaged worker and pure-Lua/C-module tests passed\n'
packaging/fedora/check-install.sh %{buildroot}
printf 'sliver package check: exact install manifest passed\n'

%pre
%sysusers_create_package %{name} %SOURCE1

%post
%systemd_post sliver-broker.service
%systemd_user_post sliver-supervisor.service
%udev_rules_update

%preun
%systemd_preun sliver-broker.service
%systemd_user_preun sliver-supervisor.service

%postun
%systemd_postun_with_restart sliver-broker.service
%systemd_user_postun_with_restart sliver-supervisor.service
%udev_rules_update

%files
%doc README.md packaging/fedora/INSTALL.md docs/lua.md docs/architecture.md docs/troubleshooting.md release-commit
%{_bindir}/sliver
%dir %{_libexecdir}/sliver
%{_libexecdir}/sliver/sliver-broker
%{_libexecdir}/sliver/sliver-supervisor
%{_libexecdir}/sliver/sliver-lua-worker
%{_unitdir}/sliver-broker.service
%{_userunitdir}/sliver-supervisor.service
%{_userunitdir}/sliver-lua-worker-.service.d/50-defaults.conf
%{_udevrulesdir}/99-z-sliver.rules
%{_sysusersdir}/sliver.conf

%changelog
* Mon Aug 31 2026 Sliver contributors - 0.1.0-3
- Authorize the unprivileged broker's cross-UID supervisor peer by kernel
  credentials and its systemd service cgroup.

* Mon Aug 31 2026 Sliver contributors - 0.1.0-2
- Run Sliver's input permission rules after the Touch Bar seat rules.

* Mon Aug 31 2026 Sliver contributors - 0.1.0-1
- Package the broker, user supervisor, worker policy, and M2 udev rules.
