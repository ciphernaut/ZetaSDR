#!/usr/bin/env bash
# Run on the Pi as root, from a directory holding zeta, zeta-mcp (here or in out/),
# zetasdr.service and zetasdr.env.example.
set -euo pipefail
[ "$(id -u)" -eq 0 ] || { echo "run as root (sudo ./install.sh)" >&2; exit 1; }
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="$here"; [ -x "$bin/zeta" ] || bin="$here/out"

install -m 0755 "$bin/zeta" "$bin/zeta-mcp" /usr/local/bin/
install -m 0755 "$here/zetasdr-check-env" /usr/local/bin/zetasdr-check-env
id zetasdr >/dev/null 2>&1 || useradd --system --home-dir /var/lib/zetasdr --shell /usr/sbin/nologin zetasdr
install -d -o zetasdr -g zetasdr -m 0750 /var/lib/zetasdr
install -d -m 0755 /etc/zetasdr
if [ ! -e /etc/zetasdr/env ]; then
  install -m 0600 -o root -g root "$here/zetasdr.env.example" /etc/zetasdr/env
  echo "wrote /etc/zetasdr/env: set ZETA_SECRET before starting"
fi
install -m 0644 "$here/zetasdr.service" /etc/systemd/system/zetasdr.service
systemctl daemon-reload
systemctl enable zetasdr.service
echo "installed."
echo "!! Set ZETA_SECRET (16+ random chars, openssl rand -hex 16) in /etc/zetasdr/env"
echo "!! BEFORE 'systemctl start zetasdr'. The service refuses to start until you do."
echo "On upgrade: systemctl try-restart zetasdr"
