#!/usr/bin/env bash
# Integration test of sliver-safeswap's restore guarantees against the *user* systemd
# manager, with a fake "tiny-dfr" (sleep infinity). No root, no hardware. ~40 s.
# Not part of scripts/check (needs a systemd user session). Run it after changing the helper:
#   harness/safeswap/itest-user.sh
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
HELPER="$HERE/sliver-safeswap"
TARGET=sliver-fake-tinydfr.service
ALT=sliver-fake-sliverd.service
RUN=sliver-swap-test.service
TIMER=sliver-swap-test-restore.timer
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
STATE="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/sliver-safeswap-itest"
WORK="$(mktemp -d /tmp/sliver-safeswap-itest.XXXXXX)"
ss() { "$HELPER" --user-mode --target "$TARGET" --alt-target "$ALT" --state-dir "$STATE" --allow-script "$@"; }
sc() { systemctl --user "$@"; }
active() { [[ "$(sc is-active "$1" 2>/dev/null)" =~ ^(active|activating|deactivating)$ ]]; }
pass=0
fail() { echo "FAIL: $*" >&2; sc status "$TARGET" "$RUN" --no-pager -n 5 >&2 || true; exit 1; }
ok() { echo "ok: $*"; pass=$((pass + 1)); }

cleanup() {
  sc stop "$RUN" "$TIMER" "$TARGET" "$ALT" >/dev/null 2>&1 || true
  sc reset-failed "$RUN" "${TIMER%.timer}.service" "$TARGET" "$ALT" >/dev/null 2>&1 || true
  rm -f "$UNIT_DIR/$TARGET" "$UNIT_DIR/$ALT"
  sc daemon-reload || true
  rm -rf "$WORK" "$STATE"
}
trap cleanup EXIT

mkdir -p "$UNIT_DIR"
cat >"$UNIT_DIR/$TARGET" <<EOF
[Unit]
Description=fake tiny-dfr for sliver-safeswap itest
[Service]
ExecStart=/usr/bin/sleep infinity
EOF
cat >"$UNIT_DIR/$ALT" <<EOF
[Unit]
Description=fake sliverd for sliver-safeswap itest
Conflicts=$TARGET
[Service]
ExecStart=/usr/bin/sleep infinity
EOF
sc daemon-reload
sc start "$TARGET"

payload() { # name body
  printf '#!/bin/sh\n%s\n' "$2" >"$WORK/$1"
  chmod +x "$WORK/$1"
  echo "$WORK/$1"
}

wait_restored() { # timeout_s
  for _ in $(seq 1 $(($1 * 10))); do
    if active "$TARGET" && ! active "$RUN"; then return 0; fi
    sleep 0.1
  done
  return 1
}

# 1. normal exit: target stopped during the run, back afterwards, timer cancelled.
p=$(payload ok 'echo "fds=$LISTEN_FDS names=$LISTEN_FDNAMES safeswap=$SLIVER_SAFESWAP"; sleep 2')
ss run "$p" --seconds 10
sleep 0.5
active "$RUN" || fail "payload not running"
active "$TARGET" && fail "target still running during the run (Conflicts= broken)"
wait_restored 8 || fail "target not restored after normal exit"
active "$TIMER" && fail "restore timer not cancelled"
grep -q 'fds=1 names=backlight safeswap=1' "$STATE/out/sliverd.log" || fail "env/OpenFile not passed: $(cat "$STATE/out/sliverd.log")"
ok "normal exit restores; backlight fd + env passed"

# 2. crash.
p=$(payload crash 'sleep 1; kill -SEGV $$')
ss run "$p" --seconds 10
wait_restored 8 || fail "target not restored after crash"
ok "crash restores"

# 3. timeout: payload ignores the deadline and SIGTERM.
p=$(payload hang 'trap "" TERM; while :; do sleep 1; done')
start=$(date +%s)
ss run "$p" --seconds 2 2>/dev/null
wait_restored 15 || fail "target not restored after timeout"
took=$(($(date +%s) - start))
((took <= 2 + 5 + 2)) || fail "timeout took ${took}s"
ok "timeout + SIGKILL restores (${took}s)"

# 4. something starts the target mid-run (e.g. a udev event): run must yield.
p=$(payload long 'sleep 60')
ss run "$p" --seconds 30 2>/dev/null
sleep 0.5
sc start "$TARGET"
wait_restored 8 || fail "run didn't yield to target start"
ok "target start preempts the run"

# 5. explicit stop.
ss run "$p" --seconds 30 2>/dev/null
sleep 0.5
ss stop 2>/dev/null
wait_restored 8 || fail "stop didn't restore"
ok "stop restores"

# 6. --wait caller dies: PDEATHSIG ends the run early.
# The helper's direct parent is this sh (the trailing `; :` stops sh from exec'ing it).
sh -c '"$@"; :' _ "$HELPER" --user-mode --target "$TARGET" --alt-target "$ALT" --state-dir "$STATE" --allow-script \
  run "$p" --seconds 30 --wait 2>/dev/null &
sub=$!
sleep 1.5
active "$RUN" || fail "--wait run didn't start"
kill -KILL "$sub" 2>/dev/null || true # the parent dies; the helper gets PDEATHSIG
wait_restored 8 || fail "caller death didn't end the run"
ok "caller death ends the run"

# 7. payload that can't exec: still restored.
p=$(payload noexec 'exec /nonexistent/binary')
ss run "$p" --seconds 10 2>/dev/null || true
wait_restored 8 || fail "failed exec didn't restore"
ok "failed exec restores"

# 8. dead-man timer on its own: kill the target behind the harness's back with no run
#    and no ExecStopPost, arm the helper's real timer command with a 2 s delay, and expect it back.
sc stop "$TARGET"
python3 - "$HELPER" "$TARGET" "$STATE" <<'PY'
import importlib.machinery, importlib.util, subprocess, sys
from pathlib import Path
loader = importlib.machinery.SourceFileLoader("ss", sys.argv[1])
spec = importlib.util.spec_from_loader("ss", loader); ss = importlib.util.module_from_spec(spec)
sys.modules["ss"] = ss; loader.exec_module(ss)
m = ss.Mode(user=True, target=sys.argv[2], state_dir=Path(sys.argv[3]), helper=sys.argv[1],
            timer_name="sliver-swap-test-restore", owners=[sys.argv[2], "sliver-fake-sliverd.service"])
cmd = [c if not c.startswith("--on-active=") else "--on-active=2s" for c in ss.restore_timer_cmd(m, 1)]
subprocess.run(cmd, check=True)
PY
active "$TARGET" && fail "target came back before the timer fired"
wait_restored 6 || fail "dead-man timer didn't restore"
ok "dead-man timer restores on its own"
"$HELPER" --user-mode --target "$TARGET" --alt-target "$ALT" --state-dir "$STATE" restore --reason itest 2>/dev/null
wait_restored 5 || fail "manual restore"
ok "restore is idempotent"

# 9. refuse double runs.
ss run "$p" --seconds 30 2>/dev/null
if ss run "$p" --seconds 30 2>/dev/null; then fail "second concurrent run accepted"; fi
ss stop 2>/dev/null
wait_restored 8 || fail "stop after double-run check"
ok "concurrent runs refused"

# 10. sliverd owns the bar (installed packaging): the run must stop it and bring IT back,
#     not tiny-dfr.
sc start "$ALT" # Conflicts= stops the fake tiny-dfr
sleep 0.3
active "$TARGET" && fail "fake sliverd didn't displace fake tiny-dfr"
ss run "$(payload alt 'sleep 2')" --seconds 10 2>/dev/null
sleep 0.5
active "$ALT" && fail "fake sliverd still running during the run"
for _ in $(seq 1 80); do active "$ALT" && ! active "$RUN" && break; sleep 0.1; done
active "$ALT" || fail "fake sliverd not restored"
active "$TARGET" && fail "restored tiny-dfr instead of the owner (sliverd)"
ok "restores whichever daemon owned the bar (sliverd)"
sc stop "$ALT"
sc start "$TARGET"

echo "itest-user: $pass passed"
