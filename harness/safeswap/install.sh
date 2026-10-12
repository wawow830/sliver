#!/usr/bin/env bash
# Install the sliver safe-swap harness. Run by the HUMAN with sudo (it writes /usr/local,
# /usr/share/polkit-1 and /etc/polkit-1):
#   sudo harness/safeswap/install.sh [--user NAME] [--no-rule]
# --user: who may run it without a password (default: $SUDO_USER).
# --no-rule: skip the no-password rule (then every run asks for an admin password).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
user="${SUDO_USER:-}"
rule=1
while (($#)); do
  case "$1" in
    --user) user="$2"; shift 2 ;;
    --no-rule) rule=0; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
python3 -m unittest discover -s "$HERE" -p 'test_*.py' -q

install -D -m 0755 -o root -g root "$HERE/sliver-safeswap" /usr/local/libexec/sliver-safeswap
install -D -m 0644 -o root -g root "$HERE/org.sliver.safeswap.policy" \
  /usr/share/polkit-1/actions/org.sliver.safeswap.policy
if ((rule)); then
  [[ "$user" =~ ^[a-z_][a-z0-9_-]*$ ]] || { echo "bad/missing --user" >&2; exit 2; }
  sed "s/@USER@/$user/g" "$HERE/49-sliver-safeswap.rules.in" >/etc/polkit-1/rules.d/49-sliver-safeswap.rules
  chmod 0644 /etc/polkit-1/rules.d/49-sliver-safeswap.rules
fi
command -v restorecon >/dev/null && restorecon -F /usr/local/libexec/sliver-safeswap \
  /usr/share/polkit-1/actions/org.sliver.safeswap.policy /etc/polkit-1/rules.d/49-sliver-safeswap.rules 2>/dev/null || true
echo "installed. try (as $user): pkexec /usr/local/libexec/sliver-safeswap status"
