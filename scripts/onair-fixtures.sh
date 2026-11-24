#!/usr/bin/env bash
# Make the generated SigMF pairs the OnAir-only rows' tests read.
#
# usage: onair-fixtures.sh <onair-gen> <dir>
#
# For every row in modes/ whose test is a fixture under onair/, writes
# <dir>/onair/<mode>.sigmf-data and .sigmf-meta with `onair-gen --sigmf`, at the rate
# the row's OnAir entry in handlers/onair.json takes (the rate the check feeds the
# handler). Prints one line per pair; exits non-zero if a generator fails. The pairs
# are megabytes and are never committed: point ZETA_FIXTURES_DIR at <dir> to check
# the rows (zeta modes check <id> --handler onair --strict). Needs jq.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <onair-gen> <dir>" >&2
  exit 2
fi
if ! command -v jq >/dev/null; then
  echo "$0: needs jq (it reads modes/*.json and handlers/onair.json)" >&2
  exit 2
fi
gen=$1
dir=$2
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
mkdir -p "$dir/onair"

status=0
for row in "$root"/modes/*.json; do
  path=$(jq -r 'select(.test != null and .test.kind == "fixture") | .test.path // empty' "$row")
  case "$path" in
    onair/*.sigmf-meta) ;;
    *) continue ;;
  esac
  id=$(jq -r .id "$row")
  mode=$(basename "$path" .sigmf-meta)
  rate=$(jq -r --arg id "$id" '.modes[$id].input.rate_hz // empty' "$root/handlers/onair.json")
  if [ -z "$rate" ]; then
    echo "$id: no OnAir entry names its rate" >&2
    status=1
    continue
  fi
  if err=$("$gen" --mode "$mode" --rate "$rate" --sigmf "$dir/onair/$mode" 2>&1 >/dev/null); then
    echo "$id: onair/$mode.sigmf-{data,meta} at $rate Hz"
  else
    echo "$id: onair-gen --mode $mode failed: $err" >&2
    status=1
  fi
done
exit $status
