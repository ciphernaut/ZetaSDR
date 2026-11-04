#!/usr/bin/env bash
# Tests for scripts/public-check.sh. Leak-shaped strings are built at run time
# so the gate does not flag this file.
set -uo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
check="$here/../public-check.sh"
if [ -n "${CLAUDE_JOB_DIR:-}" ]; then
  mkdir -p "$CLAUDE_JOB_DIR/tmp"
  base=$(mktemp -d "$CLAUDE_JOB_DIR/tmp/pc-test.XXXXXX")
else
  base=$(mktemp -d "${TMPDIR:-/tmp}/pc-test.XXXXXX")
fi
trap 'rm -rf "$base"' EXIT

coord="-27.38""4212, 153.11""7501"
akia="AK""IA""ABCDEFGHIJKLMNOP"
lhost="http://box.lo""cal:8080"
secret_host="secret""-host"

fail() { echo "FAIL $1: $2" >&2; exit 1; }

# repo NAME: a fresh git repo in $base/NAME; prints its path.
repo() {
  local d="$base/$1"
  mkdir -p "$d"
  git -C "$d" init -q
  echo "$d"
}
track() { git -C "$1" add -A >/dev/null; }
# run ROOT [VAR=value...]: sets out and rc; no denylist or exclude unless given.
run() {
  local r=$1
  shift
  out=$(env ZETA_PUBLIC_DENYLIST="$base/none" ZETA_PUBLIC_EXCLUDE="$base/none" "$@" bash "$check" "$r" 2>&1)
  rc=$?
}
pass() { echo "ok $1"; }

printf '%s\n' "# c" "$secret_host" >"$base/deny.txt"

clean_tree_passes() {
  local d
  d=$(repo clean)
  echo "hello world" >"$d/a.md"
  track "$d"
  run "$d"
  [ $rc -eq 0 ] || fail clean_tree_passes "rc=$rc $out"
  case $out in *"public-check: clean (1 files)"*) ;; *) fail clean_tree_passes "$out" ;; esac
  pass clean_tree_passes
}

denylist_hit_fails() {
  local d
  d=$(repo deny)
  echo "ssh $secret_host" >"$d/a.md"
  track "$d"
  run "$d" ZETA_PUBLIC_DENYLIST="$base/deny.txt"
  [ $rc -eq 1 ] || fail denylist_hit_fails "rc=$rc $out"
  case $out in *"public-check: a.md:1: denylist"*) ;; *) fail denylist_hit_fails "$out" ;; esac
  pass denylist_hit_fails
}

message_omits_pattern() {
  case $out in *"$secret_host"*) fail message_omits_pattern "$out" ;; esac
  local d
  d=$(repo deny2)
  echo "SSH ${secret_host^^}" >"$d/a.md"
  track "$d"
  run "$d" ZETA_PUBLIC_DENYLIST="$base/deny.txt"
  [ $rc -eq 1 ] || fail message_omits_pattern "case: rc=$rc"
  case $out in *"$secret_host"* | *"${secret_host^^}"*) fail message_omits_pattern "$out" ;; esac
  pass message_omits_pattern
}

coordinates_fail_unless_allowed() {
  local d
  d=$(repo coord)
  echo "station at $coord" >"$d/a.md"
  track "$d"
  run "$d"
  [ $rc -eq 1 ] || fail coordinates_fail_unless_allowed "rc=$rc $out"
  case $out in *"a.md:1: coordinates"*) ;; *) fail coordinates_fail_unless_allowed "$out" ;; esac
  mkdir -p "$d/scripts"
  echo "a.md:station at -27\\." >"$d/scripts/public-allow.txt"
  track "$d"
  run "$d"
  [ $rc -eq 0 ] || fail coordinates_fail_unless_allowed "allowed: rc=$rc $out"
  pass coordinates_fail_unless_allowed
}

local_host_with_port_fails() {
  local d
  d=$(repo lhost)
  echo "curl $lhost/x" >"$d/a.md"
  track "$d"
  run "$d"
  [ $rc -eq 1 ] || fail local_host_with_port_fails "rc=$rc $out"
  case $out in *"a.md:1: local-host"*) ;; *) fail local_host_with_port_fails "$out" ;; esac
  pass local_host_with_port_fails
}

svg_and_binary_are_scanned() {
  local d
  d=$(repo bin)
  printf '<svg><text>ssh %s</text></svg>\n' "$secret_host" >"$d/i.svg"
  printf '\x89PNG\r\n\x1a\n\x00\x00\x00ssh %s\x00\x01\x02' "$secret_host" >"$d/p.png"
  track "$d"
  run "$d" ZETA_PUBLIC_DENYLIST="$base/deny.txt"
  [ $rc -eq 1 ] || fail svg_and_binary_are_scanned "rc=$rc $out"
  case $out in *"i.svg:1: denylist"*) ;; *) fail svg_and_binary_are_scanned "svg: $out" ;; esac
  case $out in *"p.png:"*": denylist"*) ;; *) fail svg_and_binary_are_scanned "png: $out" ;; esac
  pass svg_and_binary_are_scanned
}

no_denylist_still_runs() {
  local d
  d=$(repo nodeny)
  echo "hello" >"$d/a.md"
  track "$d"
  run "$d"
  [ $rc -eq 0 ] || fail no_denylist_still_runs "rc=$rc $out"
  case $out in *"public-check: no private denylist; built-in checks only"*) ;; *) fail no_denylist_still_runs "$out" ;; esac
  pass no_denylist_still_runs
}

gitleaks_hit_fails() {
  local d
  d=$(repo gl)
  printf 'aws_access_key_id = %s\n' "$akia" >"$d/creds.txt"
  track "$d"
  run "$d"
  [ $rc -eq 1 ] || fail gitleaks_hit_fails "rc=$rc $out"
  case $out in *"creds.txt:1: gitleaks:"*) ;; *) fail gitleaks_hit_fails "$out" ;; esac
  pass gitleaks_hit_fails
}

excluded_paths_are_skipped() {
  local d
  d=$(repo excl)
  mkdir -p "$d/private"
  echo "ssh $secret_host" >"$d/private/x.sh"
  track "$d"
  printf '%s\n' "# private" "private/**" >"$base/excl.txt"
  run "$d" ZETA_PUBLIC_DENYLIST="$base/deny.txt" ZETA_PUBLIC_EXCLUDE="$base/excl.txt"
  [ $rc -eq 0 ] || fail excluded_paths_are_skipped "with exclude: rc=$rc $out"
  run "$d" ZETA_PUBLIC_DENYLIST="$base/deny.txt"
  [ $rc -eq 1 ] || fail excluded_paths_are_skipped "without exclude: rc=$rc $out"
  case $out in *"private/x.sh:1: denylist"*) ;; *) fail excluded_paths_are_skipped "$out" ;; esac
  pass excluded_paths_are_skipped
}

clean_tree_passes
denylist_hit_fails
message_omits_pattern
coordinates_fail_unless_allowed
local_host_with_port_fails
svg_and_binary_are_scanned
no_denylist_still_runs
gitleaks_hit_fails
excluded_paths_are_skipped
echo "all public-check tests passed"
