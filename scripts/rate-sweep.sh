#!/usr/bin/env bash
# Sweep a radio's sample rates through the node and print what it delivers.
#
# usage: rate-sweep.sh <zeta-invocation> <source> <center_hz> <rate_hz>...
#
# <zeta-invocation> is the client command, which may contain spaces:
#   "zeta --node http://127.0.0.1:7979"   or   deploy/field-host/z.sh
# For each rate: pin the source at that band, wait SWEEP_DWELL seconds
# (default 60), then print one line:
#   rate=<r> measured=<hz|null> overflows=+<n> timeouts=+<n> restarts=+<n> fault=<f>
# The counters are deltas against the `source list` taken just before the pin.
# The pin is released on exit. Needs jq.
set -euo pipefail

if [ "$#" -lt 4 ]; then
  echo "usage: $0 <zeta-invocation> <source> <center_hz> <rate_hz>..." >&2
  exit 2
fi
zeta=$1
src=$2
center=$3
shift 3
dwell=${SWEEP_DWELL:-60}

# $zeta is deliberately unquoted: it is a command with arguments.
z() { $zeta "$@"; }

stat() { # <list json> <field>
  jq -r --arg s "$src" --arg f "$2" \
    '.value[] | select(.id == $s) | .stats[$f] // 0' <<<"$1"
}

measured() { # <list json>: the delivered rate, or null when there is none yet
  jq -r --arg s "$src" \
    '.value[] | select(.id == $s) | .stats.measured_rate_hz // "null"' <<<"$1"
}

trap 'z source unpin --source "$src" >/dev/null 2>&1 || true' EXIT

for rate in "$@"; do
  before=$(z source list)
  z source pin --source "$src" --center-hz "$center" --bandwidth-hz "$rate" >/dev/null
  sleep "$dwell"
  after=$(z source list)
  fault=$(jq -r --arg s "$src" '.value[] | select(.id == $s) | .fault // "none"' <<<"$after")
  printf 'rate=%s measured=%s overflows=+%s timeouts=+%s restarts=+%s fault=%s\n' \
    "$rate" "$(measured "$after")" \
    "$(($(stat "$after" overflows) - $(stat "$before" overflows)))" \
    "$(($(stat "$after" timeouts) - $(stat "$before" timeouts)))" \
    "$(($(stat "$after" restarts) - $(stat "$before" restarts)))" \
    "$fault"
done
