#!/usr/bin/env bash
# public-check.sh [ROOT]: prove no private name, precise coordinate or secret is
# left in the files git tracks under ROOT (default: the repository root).
# Exit 0 and "public-check: clean (<n> files)", or exit 1 with one line per hit:
#   public-check: <path>:<line>: <check>
# <check> is denylist, coordinates, local-host or gitleaks:<rule>. A message
# never prints the pattern that matched.
#
# ZETA_PUBLIC_DENYLIST  extended regexes, one per line, # comments, matched
#                       case-insensitively
#                       (default $XDG_CONFIG_HOME/zetasdr/public-denylist)
# ZETA_PUBLIC_EXCLUDE   git pathspec globs, one per line, skipped entirely
#                       (default $XDG_CONFIG_HOME/zetasdr/public-exclude)
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "${1:-$here/..}" && pwd)
cfg=${XDG_CONFIG_HOME:-$HOME/.config}/zetasdr
denylist=${ZETA_PUBLIC_DENYLIST:-$cfg/public-denylist}
exclude=${ZETA_PUBLIC_EXCLUDE:-$cfg/public-exclude}

if [ -n "${CLAUDE_JOB_DIR:-}" ]; then
  mkdir -p "$CLAUDE_JOB_DIR/tmp"
  tmp=$(mktemp -d "$CLAUDE_JOB_DIR/tmp/public-check.XXXXXX")
else
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/public-check.XXXXXX")
fi
trap 'rm -rf "$tmp"' EXIT

gl=$(command -v gitleaks || true)
if [ -z "$gl" ] && [ -x "$HOME/.local/bin/gitleaks" ]; then gl="$HOME/.local/bin/gitleaks"; fi
if [ -z "$gl" ]; then
  echo "public-check: gitleaks not found (put it on PATH or at ~/.local/bin/gitleaks)" >&2
  exit 2
fi

# Tracked files, minus the excluded globs.
specs=(.)
if [ -f "$exclude" ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    line=${line%%#*}
    line=$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
    if [ -n "$line" ]; then specs+=(":(exclude,glob)$line"); fi
  done <"$exclude"
fi
git -C "$root" ls-files -z -- "${specs[@]}" | tr '\0' '\n' >"$tmp/files"
nfiles=$(grep -c . "$tmp/files" || true)

# Denylist patterns, comments and blanks dropped.
: >"$tmp/deny"
if [ -f "$denylist" ]; then
  sed -e 's/^[[:space:]]*#.*$//' -e '/^[[:space:]]*$/d' "$denylist" >"$tmp/deny"
else
  echo "public-check: no private denylist; built-in checks only"
fi

allow=""
if [ -f "$root/scripts/public-allow.txt" ]; then
  allow="$root/scripts/public-allow.txt"
elif [ -f "$here/public-allow.txt" ]; then
  allow="$here/public-allow.txt"
fi

coord_re='-?[0-9]{1,3}\.[0-9]{4,}, ?-?[0-9]{1,3}\.[0-9]{4,}'
host_re='[a-z0-9-]+\.local:[0-9]+'

: >"$tmp/hits"

# allowed PATH LINE-TEXT: true when public-allow.txt has a PATH-GLOB:REGEX entry
# that matches.
allowed() {
  [ -n "$allow" ] || return 1
  local entry p r
  while IFS= read -r entry || [ -n "$entry" ]; do
    case $entry in '#'* | '') continue ;; esac
    p=${entry%%:*}
    r=${entry#*:}
    # shellcheck disable=SC2053
    if [[ $1 == $p ]] && printf '%s\n' "$2" | grep -qE -- "$r"; then return 0; fi
  done <"$allow"
  return 1
}

# scan_file PATH TEXTFILE: appends "path:line: check" for every hit.
scan_file() {
  local f=$1 src=$2 m n text check re
  if [ -s "$tmp/deny" ]; then
    while IFS= read -r m; do
      echo "$f:${m%%:*}: denylist" >>"$tmp/hits"
    done < <(grep -n -iE -f "$tmp/deny" "$src" 2>/dev/null || true)
  fi
  for check in coordinates local-host; do
    if [ "$check" = coordinates ]; then re=$coord_re; else re=$host_re; fi
    while IFS= read -r m; do
      n=${m%%:*}
      text=${m#*:}
      if ! allowed "$f" "$text"; then echo "$f:$n: $check" >>"$tmp/hits"; fi
    done < <(grep -nE -- "$re" "$src" 2>/dev/null || true)
  done
}

while IFS= read -r f; do
  [ -f "$root/$f" ] || continue
  if [ ! -s "$root/$f" ] || grep -qI . "$root/$f" 2>/dev/null; then
    scan_file "$f" "$root/$f"
  else
    # binary: scan the printable strings (the line is the strings' line)
    strings -n 4 "$root/$f" >"$tmp/strings" 2>/dev/null || true
    scan_file "$f" "$tmp/strings"
  fi
done <"$tmp/files"

# gitleaks scans the directory; keep what git tracks and is not excluded.
cfgtoml="$root/.gitleaks.toml"
[ -f "$cfgtoml" ] || cfgtoml="$here/../.gitleaks.toml"
if ! "$gl" dir --no-banner --redact -c "$cfgtoml" -r "$tmp/gl.json" --exit-code 0 "$root" \
  >"$tmp/gl.out" 2>&1; then
  echo "public-check: gitleaks failed" >&2
  exit 2
fi
if [ -s "$tmp/gl.json" ]; then
  python3 -I - "$tmp/gl.json" "$tmp/files" "$root" >>"$tmp/hits" <<'PY'
import json, os, sys
hits = json.load(open(sys.argv[1]))
tracked = set(open(sys.argv[2]).read().split("\n"))
root = os.path.realpath(sys.argv[3])
for h in hits:
    p = os.path.relpath(os.path.realpath(h["File"]), root)
    if p in tracked:
        print("%s:%d: gitleaks:%s" % (p, h["StartLine"], h["RuleID"]))
PY
fi

if [ -s "$tmp/hits" ]; then
  sort -u "$tmp/hits" | sed 's/^/public-check: /'
  exit 1
fi
echo "public-check: clean ($nfiles files)"
