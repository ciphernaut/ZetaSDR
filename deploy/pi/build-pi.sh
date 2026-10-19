#!/usr/bin/env bash
# Cross-build zeta and zeta-mcp for a Raspberry Pi 5 (aarch64 Linux) into deploy/pi/out/.
# Needs docker. Soapy is not built; see README.md for the on-Pi --cfg soapy path.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
target=aarch64-unknown-linux-gnu

export PATH="$HOME/.cargo/bin:$PATH"
command -v cross >/dev/null || cargo install cross --locked
cd "$root"
rustup target add "$target" >/dev/null 2>&1 || true
cross build --release --target "$target" -p zeta -p zeta-mcp

mkdir -p "$here/out"
for b in zeta zeta-mcp; do
  install -m 0755 "target/$target/release/$b" "$here/out/$b"
done
cd "$here/out"
ls -l zeta zeta-mcp | awk '{print $5, $NF}'
sha256sum zeta zeta-mcp
