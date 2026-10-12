#!/usr/bin/env bash
# Remove the sliver safe-swap harness (restores tiny-dfr first). sudo harness/safeswap/uninstall.sh
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
if [[ -x /usr/local/libexec/sliver-safeswap ]]; then
  SUDO_UID=${SUDO_UID:-65534} /usr/local/libexec/sliver-safeswap restore --reason uninstall || true
fi
rm -f /usr/local/libexec/sliver-safeswap /usr/share/polkit-1/actions/org.sliver.safeswap.policy \
  /etc/polkit-1/rules.d/49-sliver-safeswap.rules
rm -rf /run/sliver-safeswap
systemctl start tiny-dfr.service || true
echo "uninstalled"
