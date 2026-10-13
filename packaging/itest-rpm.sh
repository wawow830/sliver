#!/usr/bin/env bash
# RPM lifecycle test (T131): installs the built sliverd and sliver-agent RPMs in a throwaway
# Fedora container with systemd as PID 1 and checks what the scriptlets do to the bar's owner:
#   A. tiny-dfr installed (a stand-in unit) and driving the bar
#      1. install starts nothing and leaves tiny-dfr alone; the agent's user preset enables it
#      2. upgrade (reinstall) while tiny-dfr runs doesn't take the bar
#      3. upgrade while sliverd runs restarts sliverd and keeps tiny-dfr off
#      4. uninstall while sliverd runs hands the bar to tiny-dfr (invariant 1) and removes our
#         tiny-dfr drop-in and udev rule
#   B. no tiny-dfr at all: install, run, uninstall all succeed and leave nothing running
# The scriptlets, presets, udev rule and drop-ins are the real ones from the RPMs. Only
# sliverd.service's [Service] is replaced by a stand-in (/etc override) since there's no bar:
# its [Unit] and the real handover command (ExecStartPre) are kept, minus the device binding.
# udevadm calls in %post have no udev to talk to here and are ignored, as they are on failure.
#
# Not part of scripts/check (needs podman, an image download on first run, ~1 min).
# Usage: packaging/itest-rpm.sh [RPM_DIR]   (default: target/rpmbuild/RPMS/<arch>)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

dir=$(realpath "${1:-target/rpmbuild/RPMS/$(uname -m)}")
for p in sliverd sliver-agent; do
  compgen -G "$dir/$p-[0-9]*.rpm" >/dev/null || { echo "itest-rpm: no $p RPM in $dir (scripts/rpm-build --binary)"; exit 1; }
done
command -v podman >/dev/null || { echo "itest-rpm: needs podman"; exit 1; }

image=localhost/sliver-itest-rpm:44
if ! podman image exists "$image"; then
  echo "== building $image (once)"
  printf 'FROM registry.fedoraproject.org/fedora:44\nRUN dnf -y install --setopt=install_weak_deps=False systemd systemd-udev procps-ng libxkbcommon && dnf clean all\n' \
    | podman build -q -t "$image" -f - >/dev/null
fi

c=""
cleanup() { [[ -n "$c" ]] && podman rm -f -t 0 "$c" >/dev/null 2>&1 || true; }
trap cleanup EXIT

x() { podman exec "$c" "$@"; }
state() { x systemctl is-active "$1" 2>/dev/null || true; }
expect() { # unit state [timeout_s]
  for _ in $(seq $((${3:-5} * 10))); do [[ "$(state "$1")" == "$2" ]] && return 0; sleep 0.1; done
  echo "FAIL: $1 is $(state "$1"), wanted $2"; x systemctl status "$1" --no-pager 2>&1 | tail -8; exit 1
}
pid() { x systemctl show -P MainPID "$1"; }
rpms() { echo /rpms/sliverd-[0-9]*.rpm /rpms/sliver-agent-[0-9]*.rpm; }

boot() {
  cleanup
  c=$(podman run -d --rm --systemd=always -v "$dir:/rpms:ro,Z" "$image" /sbin/init)
  for _ in $(seq 100); do
    s=$(x systemctl is-system-running 2>/dev/null || true)
    [[ "$s" == running || "$s" == degraded ]] && return 0
    sleep 0.1
  done
  echo "FAIL: systemd in the container didn't come up ($s)"; exit 1
}
# Libraries the RPMs need that the image lacks (it's built once; the packages' needs grow).
deps() {
  local missing
  missing=$(x bash -c "rpm -U --test $(rpms) 2>&1" | awk '/is needed by/ {print $1}' | sort -u)
  [[ -z "$missing" ]] || x dnf -q -y install --setopt=install_weak_deps=False $missing >/dev/null
}

# A stand-in for the installed sliverd.service: its real [Unit] minus the device binding, and the
# real ExecStartPre/Restart lines, around a sleeping process.
standin() {
  x bash -c '
    u=/usr/lib/systemd/system/sliverd.service
    { sed -n "/^\[Unit\]/,/^\[Service\]/p" $u | sed -e "\$d" -e "/^BindsTo=dev-/d" -e "s/ dev-sliver_display\.device//"
      printf "[Service]\nType=exec\nExecStart=/usr/bin/sleep infinity\nSockets=sliverd.socket\n"
      grep -E "^(ExecStartPre|Restart|RestartSec|RestartMode)=" $u
    } > /etc/systemd/system/sliverd.service
    systemctl daemon-reload'
}

fake_tiny_dfr() {
  x bash -c 'printf "[Unit]\nDescription=fake tiny-dfr\n[Service]\nExecStart=/usr/bin/sleep infinity\nRestart=always\n" \
    > /usr/lib/systemd/system/tiny-dfr.service && systemctl daemon-reload && systemctl start tiny-dfr.service'
}

echo "== A1. install starts nothing, tiny-dfr keeps the bar"
boot; deps
fake_tiny_dfr
expect tiny-dfr.service active
x bash -c "rpm -U $(rpms)" >/dev/null
sleep 1
expect sliverd.service inactive
expect sliverd.socket inactive
expect tiny-dfr.service active
[[ "$(x systemctl --global is-enabled sliver-agent.service)" == enabled ]] \
  || { echo "FAIL: sliver-agent.service not enabled by the user preset"; exit 1; }
x test -e /usr/lib/systemd/system/tiny-dfr.service.d/50-sliver.conf
echo ok

echo "== A2. upgrade while tiny-dfr runs doesn't take the bar"
standin
t=$(pid tiny-dfr.service)
x bash -c "rpm -U --replacepkgs $(rpms)" >/dev/null
sleep 1
expect sliverd.service inactive
expect tiny-dfr.service active
[[ "$(pid tiny-dfr.service)" == "$t" ]] || { echo "FAIL: tiny-dfr was restarted"; exit 1; }
echo ok

echo "== A3. upgrade while sliverd runs restarts it, tiny-dfr stays off"
x systemctl start sliverd.service
expect sliverd.service active
expect tiny-dfr.service inactive
s=$(pid sliverd.service)
x bash -c "rpm -U --replacepkgs $(rpms)" >/dev/null
expect sliverd.service active
expect tiny-dfr.service inactive
[[ "$(pid sliverd.service)" != "$s" ]] || { echo "FAIL: sliverd was not restarted by the upgrade"; exit 1; }
echo ok

echo "== A4. uninstall while sliverd runs hands the bar to tiny-dfr"
# sliverd is still running from A3. The stand-in stays through the uninstall: without it a reload
# would bring back the device binding and systemd would stop sliverd before %preun gets to.
x bash -c 'rpm -e sliver-agent sliverd' >/dev/null
expect tiny-dfr.service active
expect sliverd.service inactive
expect sliverd.socket inactive
for f in /usr/lib/systemd/system/tiny-dfr.service.d/50-sliver.conf /usr/lib/udev/rules.d/99-z-sliverd.rules; do
  x test ! -e "$f" || { echo "FAIL: $f left behind"; exit 1; }
done
echo ok

echo "== B. without tiny-dfr: install, run, uninstall"
boot; deps
x bash -c "rpm -U $(rpms)" >/dev/null
standin
x systemctl start sliverd.service
expect sliverd.service active
x bash -c 'rpm -e sliver-agent sliverd' >/dev/null
expect sliverd.service inactive
expect sliverd.socket inactive
echo ok
echo "itest-rpm: all ok"
