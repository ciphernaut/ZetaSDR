#!/usr/bin/env bash
# Offline smoke test of the aarch64 build under qemu-user. Run from anywhere.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
zeta="$here/out/zeta"
qemu="${QEMU_AARCH64:-qemu-aarch64-static}"

[ -x "$zeta" ] || { echo "FAIL: $zeta missing; run deploy/pi/build-pi.sh" >&2; exit 1; }
command -v "$qemu" >/dev/null || { echo "FAIL: $qemu not found" >&2; exit 1; }

# qemu-user needs an aarch64 glibc to load a dynamic binary. Use QEMU_LD_PREFIX if set,
# then /usr/aarch64-linux-gnu (apt libc6-arm64-cross), else copy the sysroot out of the
# cross image once (needs docker) into out/sysroot.
if [ -z "${QEMU_LD_PREFIX:-}" ] && [ ! -e /lib/ld-linux-aarch64.so.1 ]; then
  if [ -d /usr/aarch64-linux-gnu/lib ]; then
    export QEMU_LD_PREFIX=/usr/aarch64-linux-gnu
  else
    sys="$here/out/sysroot"
    if [ ! -d "$sys/lib" ]; then
      img="ghcr.io/cross-rs/aarch64-unknown-linux-gnu:0.2.5"
      cid="$(docker create "$img")" || { echo "FAIL: no aarch64 sysroot (set QEMU_LD_PREFIX)" >&2; exit 1; }
      mkdir -p "$sys"
      docker cp "$cid:/usr/aarch64-linux-gnu/lib" "$sys/lib" >/dev/null
      docker rm "$cid" >/dev/null
    fi
    export QEMU_LD_PREFIX="$sys"
  fi
fi

schema="$("$qemu" "$zeta" schema)" || { echo "FAIL: 'zeta schema' exited non-zero" >&2; exit 1; }
printf '%s' "$schema" | python3 -I -c '
import json, sys
d = json.load(sys.stdin)
v = d.get("value") if isinstance(d, dict) else None
if not isinstance(v, dict) or "op" not in v:
    sys.exit("FAIL: schema JSON has no \"op\" key under \"value\"")
' || exit 1
echo "schema: $(printf '%s' "$schema" | head -n 1 | cut -c1-100)"

help="$("$qemu" "$zeta" --help)" || { echo "FAIL: 'zeta --help' exited non-zero" >&2; exit 1; }
line="$(printf '%s\n' "$help" | grep -m1 -e '--source-node')" || { echo "FAIL: --help does not mention --source-node" >&2; exit 1; }
echo "help:   $line"
echo "PASS"
