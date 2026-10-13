#!/usr/bin/env bash
# Tests the Unit-section logic of packaging/systemd/sliverd.{service,socket} (invariant 1)
# against the *user* systemd manager, with no root and no hardware:
#   1. sliverd keeps crashing → start limit → OnFailure starts tiny-dfr, the socket goes away
#   2. tiny-dfr starting while sliverd runs → sliverd (and its socket) stop
#   3. after the fallback, an agent connecting can't reactivate sliverd
#   5. after the fallback, `systemctl start sliverd` (refused: start limit) leaves tiny-dfr running
#      (this left the real bar dead once, with Conflicts=)
#   6. starting only the socket doesn't take the bar from tiny-dfr
#   7. both started at once → exactly one runs
#   8. no tiny-dfr installed at all → sliverd still starts
# The real units' [Unit] sections are used as-is (renamed with a unique prefix). [Service] is
# replaced by a stand-in, and device bindings are dropped (no devices in a user manager).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

if ! systemctl --user is-system-running >/dev/null 2>&1 && ! systemctl --user list-units >/dev/null 2>&1; then
  echo "itest-units: no user systemd manager, skipping"; exit 0
fi

P="sliveritest$$"
DIR="${XDG_RUNTIME_DIR:?}/systemd/user"
mkdir -p "$DIR"
SOCK="$XDG_RUNTIME_DIR/$P.sock"
units=("$P-sliverd.service" "$P-sliverd.socket" "$P-tiny-dfr.service")

cleanup() {
  systemctl --user stop "${units[@]}" >/dev/null 2>&1 || true
  systemctl --user reset-failed "${units[@]}" >/dev/null 2>&1 || true
  for u in "${units[@]}"; do rm -f "$DIR/$u"; done
  systemctl --user daemon-reload
  rm -f "$SOCK"
}
trap cleanup EXIT

prefix() {
  sed -e "s/\bsliverd\.socket\b/$P-sliverd.socket/g" \
      -e "s/\bsliverd\.service\b/$P-sliverd.service/g" \
      -e "s/\btiny-dfr\.service\b/$P-tiny-dfr.service/g" \
      -e 's|/usr/bin/systemctl |/usr/bin/systemctl --user |'
}
# [Unit] of the real file, with names prefixed and device bindings removed.
unit_section() {
  sed -n '/^\[Unit\]/,/^\[/p' "$1" | sed '$!b;/^\[/d' | prefix \
    | sed -e '/^BindsTo=dev-/d' -e 's/ dev-sliver_display\.device//'
}
# The real handover commands (ExecStartPre=), prefixed and pointed at the user manager.
handover() { { grep -E '^ExecStartPre=' "$1" || true; } | prefix; }

write_sliverd() { # $1 = ExecStart
  { unit_section packaging/systemd/sliverd.service
    printf '[Service]\nType=exec\nExecStart=%s\nSockets=%s\n' "$1" "$P-sliverd.socket"
    handover packaging/systemd/sliverd.service
    grep -E '^(Restart|RestartSec|RestartMode)=' packaging/systemd/sliverd.service
  } > "$DIR/$P-sliverd.service"
}

{ unit_section packaging/systemd/sliverd.socket
  printf '[Socket]\nListenSequentialPacket=%s\nFileDescriptorName=ipc\nRemoveOnStop=yes\n' "$SOCK"
} > "$DIR/$P-sliverd.socket"
{ printf '[Unit]\nDescription=fake tiny-dfr\n[Service]\nExecStart=/usr/bin/sleep infinity\nRestart=always\n'
  handover packaging/systemd/tiny-dfr.service.d/50-sliver.conf
} > "$DIR/$P-tiny-dfr.service"

state() { systemctl --user is-active "$1" 2>/dev/null || true; }
wait_for() { # unit state timeout_s
  for _ in $(seq $((${3:-10} * 10))); do [[ "$(state "$1")" == "$2" ]] && return 0; sleep 0.1; done
  echo "FAIL: $1 is $(state "$1"), wanted $2"; systemctl --user status "$1" --no-pager | tail -5; exit 1
}
fresh() {
  systemctl --user stop "${units[@]}" >/dev/null 2>&1 || true
  systemctl --user reset-failed "${units[@]}" >/dev/null 2>&1 || true
  systemctl --user daemon-reload
}

echo "== 1. crash loop → start limit → tiny-dfr"
write_sliverd "/usr/bin/false"; fresh
systemctl --user start "$P-sliverd.service" || true
# While restarting, tiny-dfr must NOT be started yet (fast restarts are the first line of defence).
sleep 1.2
[[ "$(state "$P-tiny-dfr.service")" != active ]] || { echo "FAIL: tiny-dfr started before the start limit"; exit 1; }
wait_for "$P-tiny-dfr.service" active 15
wait_for "$P-sliverd.service" failed 2
starts=$(systemctl --user show -P NRestarts "$P-sliverd.service")
echo "sliverd restarted $starts times before the fallback"
[[ "$(state "$P-sliverd.socket")" != active ]] || { echo "FAIL: socket still active"; exit 1; }
echo ok

echo "== 3. agent connect after fallback doesn't reactivate sliverd"
python3 - "$SOCK" <<'EOF' || true
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
try:
    s.connect(sys.argv[1]); s.send(b'{"t":"hello","proto":[1,0],"agent":"itest"}')
except OSError:
    pass
EOF
sleep 0.5
[[ "$(state "$P-sliverd.service")" != active ]] || { echo "FAIL: sliverd reactivated"; exit 1; }
wait_for "$P-tiny-dfr.service" active 1
echo ok

echo "== 5. a refused start after the fallback leaves tiny-dfr running"
systemctl --user start "$P-sliverd.service" 2>/dev/null && { echo "FAIL: start not refused"; exit 1; }
sleep 0.5
wait_for "$P-tiny-dfr.service" active 1
echo ok

echo "== 2. tiny-dfr preempts a healthy sliverd"
write_sliverd "/usr/bin/sleep infinity"; fresh
systemctl --user start "$P-sliverd.service"
wait_for "$P-sliverd.service" active 5
wait_for "$P-sliverd.socket" active 1
systemctl --user start "$P-tiny-dfr.service"
wait_for "$P-sliverd.service" inactive 5
wait_for "$P-sliverd.socket" inactive 1
echo ok

echo "== 4. sliverd preempts tiny-dfr (udev wants sliverd)"
systemctl --user start "$P-sliverd.service"
wait_for "$P-tiny-dfr.service" inactive 5
wait_for "$P-sliverd.service" active 5
echo ok

echo "== 6. starting only the socket leaves tiny-dfr alone"
systemctl --user start "$P-tiny-dfr.service"
wait_for "$P-sliverd.service" inactive 5
systemctl --user start "$P-sliverd.socket"
sleep 0.5
wait_for "$P-tiny-dfr.service" active 1
echo ok

echo "== 7. both started at once: one of them ends up driving the bar"
for i in 1 2 3 4 5; do
  fresh
  case $i in
    1|2) systemctl --user start --no-block "$P-sliverd.service" "$P-tiny-dfr.service" ;;
    3) systemctl --user start --no-block "$P-sliverd.service"; systemctl --user start --no-block "$P-tiny-dfr.service" ;;
    *) systemctl --user start --no-block "$P-tiny-dfr.service"; systemctl --user start --no-block "$P-sliverd.service" ;;
  esac
  sleep 1.5
  a=$(state "$P-sliverd.service"); b=$(state "$P-tiny-dfr.service")
  [[ "$a" == active || "$b" == active ]] || { echo "FAIL: neither runs (sliverd $a, tiny-dfr $b)"; exit 1; }
  [[ "$a" != active || "$b" != active ]] || { echo "FAIL: both run"; exit 1; }
done
echo ok

echo "== 8. no tiny-dfr installed: sliverd still starts"
fresh
mv "$DIR/$P-tiny-dfr.service" "$DIR/$P-tiny-dfr.service.away"
systemctl --user daemon-reload
systemctl --user start "$P-sliverd.service" || true
mv "$DIR/$P-tiny-dfr.service.away" "$DIR/$P-tiny-dfr.service"
wait_for "$P-sliverd.service" active 5
systemctl --user daemon-reload
echo ok
echo "itest-units: all ok"
