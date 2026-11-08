Name:           sliver
Version:        0.1.0
Release:        1%{?dist}
Summary:        The Touch Bar's display server
License:        MIT
URL:            https://github.com/wawow830/sliver
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  cmake
BuildRequires:  ninja-build
BuildRequires:  gcc-c++
BuildRequires:  qt6-qtbase-devel
BuildRequires:  qt6-qtdeclarative-devel
BuildRequires:  wayland-devel
BuildRequires:  libinput-devel
BuildRequires:  systemd-devel
BuildRequires:  pixman-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  systemd-rpm-macros
Requires:       tiny-dfr
Requires:       brightnessctl
Recommends:     cava

%description
sliverd makes the Touch Bar an ordinary Wayland screen. The bar is a Quickshell
config: the Sliver QML module gives it pills, keys, levels, workspaces, media and
F-keys, and ~/.config/quickshell/sliver/shell.qml replaces the default one.
Esc and F1-F12 show when nothing is open or while Fn is held.

%prep
%autosetup

%build
cargo build --release --locked
pushd bar/Sliver
%cmake -G Ninja
%cmake_build
popd

%install
install -Dm755 target/release/sliverd %{buildroot}%{_bindir}/sliverd
pushd bar/Sliver
%cmake_install
popd
install -Dm644 bar/shell.qml %{buildroot}%{_sysconfdir}/xdg/quickshell/sliver/shell.qml
install -Dm644 packaging/sliverd.service %{buildroot}%{_unitdir}/sliverd.service
install -Dm644 packaging/sliver-bar.service %{buildroot}%{_userunitdir}/sliver-bar.service
install -Dm644 packaging/99-z-sliverd.rules %{buildroot}%{_udevrulesdir}/99-z-sliverd.rules

%post
%systemd_post sliverd.service
%systemd_user_post sliver-bar.service

%preun
%systemd_preun sliverd.service
%systemd_user_preun sliver-bar.service

%postun
%systemd_postun sliverd.service
%systemd_user_postun sliver-bar.service

%files
%{_bindir}/sliverd
%{_libdir}/qt6/qml/Sliver/
%dir %{_sysconfdir}/xdg/quickshell/sliver
%config(noreplace) %{_sysconfdir}/xdg/quickshell/sliver/shell.qml
%{_unitdir}/sliverd.service
%{_userunitdir}/sliver-bar.service
%{_udevrulesdir}/99-z-sliverd.rules
