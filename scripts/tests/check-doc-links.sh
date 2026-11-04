#!/usr/bin/env bash
# Tests for scripts/check-doc-links.sh: each case builds a small git repo of
# Markdown files and runs the checker on it.
set -uo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
check="$here/../check-doc-links.sh"
if [ -n "${CLAUDE_JOB_DIR:-}" ]; then
  mkdir -p "$CLAUDE_JOB_DIR/tmp"
  base=$(mktemp -d "$CLAUDE_JOB_DIR/tmp/dl-test.XXXXXX")
else
  base=$(mktemp -d "${TMPDIR:-/tmp}/dl-test.XXXXXX")
fi
trap 'rm -rf "$base"' EXIT

fail() { echo "FAIL $1: $2" >&2; exit 1; }
pass() { echo "ok $1"; }

# repo NAME: a fresh git repo in $base/NAME; prints its path.
repo() {
  local d="$base/$1"
  mkdir -p "$d"
  git -C "$d" init -q
  echo "$d"
}
track() { git -C "$1" add -A >/dev/null; }
# run ROOT: sets out and rc.
run() {
  out=$(bash "$check" "$1" 2>&1)
  rc=$?
}

ok_link() {
  local d
  d=$(repo ok)
  mkdir -p "$d/docs/sub"
  cat >"$d/README.md" <<'EOF'
# Title

See [the guide](docs/guide.md), [its sources](docs/guide.md#reading-a-sources-health),
[a `code` heading](docs/guide.md#the-zeta-serve-command), [the second one](docs/guide.md#notes-1),
[here](#title), [a dir](docs/sub), [web](https://example.com/x.md#nope) and
[mail](mailto:a@example.com). ![figure](docs/sub/fig.svg)

```bash
echo "[not a link](missing.md)"
```

Inline `[not a link](missing.md)` too. <img src="docs/sub/fig.svg" alt="fig">
EOF
  cat >"$d/docs/guide.md" <<'EOF'
# Guide

## Reading a source's health

### The `zeta serve` command

## Notes

## Notes

Back to [the front page](../README.md#title).
EOF
  echo "<svg/>" >"$d/docs/sub/fig.svg"
  track "$d"
  run "$d"
  [ $rc -eq 0 ] || fail ok_link "rc=$rc $out"
  case $out in *"doc-links: clean (9 links)"*) ;; *) fail ok_link "$out" ;; esac
  pass ok_link
}

broken_file() {
  local d
  d=$(repo file)
  printf '%s\n' "# A" "" "Read [this](missing.md) and [that](docs/gone.md#x)." >"$d/README.md"
  track "$d"
  run "$d"
  [ $rc -eq 1 ] || fail broken_file "rc=$rc $out"
  case $out in *"README.md:3: missing.md"*) ;; *) fail broken_file "$out" ;; esac
  case $out in *"README.md:3: docs/gone.md#x"*) ;; *) fail broken_file "$out" ;; esac
  pass broken_file
}

broken_anchor() {
  local d
  d=$(repo anchor)
  mkdir -p "$d/docs"
  printf '%s\n' "# Guide" "" "## Sources" "## Notes" >"$d/docs/guide.md"
  printf '%s\n' "# A" "[x](docs/guide.md#federation)" "[y](#nowhere)" \
    "[z](docs/guide.md#notes-1)" "[ok](docs/guide.md#sources)" >"$d/README.md"
  track "$d"
  run "$d"
  [ $rc -eq 1 ] || fail broken_anchor "rc=$rc $out"
  case $out in *"README.md:2: docs/guide.md#federation"*) ;; *) fail broken_anchor "$out" ;; esac
  case $out in *"README.md:3: #nowhere"*) ;; *) fail broken_anchor "$out" ;; esac
  case $out in *"README.md:4: docs/guide.md#notes-1"*) ;; *) fail broken_anchor "$out" ;; esac
  case $out in *"#sources"*) fail broken_anchor "a good anchor was listed: $out" ;; esac
  pass broken_anchor
}

design_history_and_untracked_are_skipped() {
  local d
  d=$(repo skip)
  mkdir -p "$d/docs/superpowers/plans"
  echo "[old](../../gone.md)" >"$d/docs/superpowers/plans/p.md"
  echo "# A" >"$d/README.md"
  track "$d"
  echo "[new](gone.md)" >"$d/scratch.md"
  run "$d"
  [ $rc -eq 0 ] || fail design_history_and_untracked_are_skipped "rc=$rc $out"
  case $out in *"doc-links: clean (0 links)"*) ;; *) fail design_history_and_untracked_are_skipped "$out" ;; esac
  pass design_history_and_untracked_are_skipped
}

ok_link
broken_file
broken_anchor
design_history_and_untracked_are_skipped
echo "all check-doc-links tests passed"
