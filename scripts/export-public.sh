#!/usr/bin/env bash
# export-public.sh DEST [REF]: build the public repository at DEST from the
# committed tree of REF (default HEAD): a fresh history of one commit, tagged,
# with the operator's private paths dropped, and every gate run again inside it.
# Prints "export: ready at DEST (<sha>)", or "export: <gate> failed" and exits 1
# (DEST is left for inspection). It never adds a remote and never pushes.
#
# ZETA_PUBLIC_DENYLIST  the private denylist public-check reads (required)
#                       (default $XDG_CONFIG_HOME/zetasdr/public-denylist)
# ZETA_PUBLIC_EXCLUDE   git pathspec globs of the private paths to drop (required)
#                       (default $XDG_CONFIG_HOME/zetasdr/public-exclude)
# PUBLIC_AUTHOR         author and committer (default Sage Grigull <ciphernaut@proton.me>)
# PUBLIC_DATE           release date YYYY-MM-DD for the commit and tag (default today, UTC)
# CARGO_TARGET_DIR      build output for the cargo gates (default $CLAUDE_JOB_DIR/target,
#                       else the source checkout's target/); never inside DEST
# EXPORT_SKIP_CARGO=1   skip the cargo gates (the script's own tests)
#
# Without a denylist or an exclude file the export is refused: point the variables
# at empty files to export a tree that has no private paths.
set -euo pipefail

version=0.1.0
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

die() {
  echo "export: $*" >&2
  exit 1
}

[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: export-public.sh DEST [REF]"
dest_arg=$1
ref=${2:-HEAD}

src=$(git -C "$here/.." rev-parse --show-toplevel) || die "not in a git repository"
cfg=${XDG_CONFIG_HOME:-$HOME/.config}/zetasdr
denylist=${ZETA_PUBLIC_DENYLIST:-$cfg/public-denylist}
exclude=${ZETA_PUBLIC_EXCLUDE:-$cfg/public-exclude}
author=${PUBLIC_AUTHOR:-Sage Grigull <ciphernaut@proton.me>}
date=${PUBLIC_DATE:-$(date -u +%Y-%m-%d)}

[ -f "$denylist" ] || die "no denylist at $denylist (set ZETA_PUBLIC_DENYLIST)"
[ -f "$exclude" ] || die "no exclude file at $exclude (set ZETA_PUBLIC_EXCLUDE)"
[[ $date =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || die "PUBLIC_DATE must be YYYY-MM-DD"
date -u -d "$date" +%F >/dev/null 2>&1 || die "PUBLIC_DATE is not a date"
[[ $author =~ ^([^<>]+)\ \<([^<>@ ]+@[^<>@ ]+)\>$ ]] || die "PUBLIC_AUTHOR must be 'Name <email>'"
author_name=${BASH_REMATCH[1]}
author_email=${BASH_REMATCH[2]}

if [ -n "$(git -C "$src" status --porcelain)" ]; then
  die "the working tree at $src is dirty; commit or remove the changes first"
fi
sha_src=$(git -C "$src" rev-parse --verify --quiet "$ref^{commit}") || die "unknown ref $ref"

if [ -e "$dest_arg" ]; then
  [ -d "$dest_arg" ] || die "$dest_arg exists and is not a directory"
  [ -z "$(ls -A "$dest_arg")" ] || die "$dest_arg is not empty"
fi
mkdir -p "$dest_arg"
dest=$(cd "$dest_arg" && pwd -P)
srcp=$(cd "$src" && pwd -P)
case $dest/ in "$srcp"/*) die "DEST must lie outside the source checkout" ;; esac

if [ -n "${CLAUDE_JOB_DIR:-}" ]; then
  mkdir -p "$CLAUDE_JOB_DIR/tmp"
  tmp=$(mktemp -d "$CLAUDE_JOB_DIR/tmp/export-public.XXXXXX")
else
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/export-public.XXXXXX")
fi
trap 'rm -rf "$tmp"' EXIT

# git in DEST: no system or global config, no hooks, no templates, no signing,
# so nothing of the operator's setup (remotes, insteadOf, hooks) reaches it.
dgit() {
  GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null \
    GIT_AUTHOR_NAME="$author_name" GIT_AUTHOR_EMAIL="$author_email" \
    GIT_COMMITTER_NAME="$author_name" GIT_COMMITTER_EMAIL="$author_email" \
    GIT_AUTHOR_DATE="${date}T00:00:00Z" GIT_COMMITTER_DATE="${date}T00:00:00Z" \
    git -C "$dest" -c core.hooksPath=/dev/null -c commit.gpgSign=false -c tag.gpgSign=false "$@"
}

gate_failed() {
  echo "export: $1 failed; $dest is left for inspection" >&2
  exit 1
}

echo "export: $ref ($sha_src) to $dest"
git -C "$src" archive --format=tar "$sha_src" | tar -x -C "$dest"

dgit init -q -b main --template=
# -f: a tracked file the tree's own .gitignore matches is still part of the export.
dgit add -A -f

# Drop the private paths, with the same pathspec globs public-check skips.
dropped=0
while IFS= read -r line || [ -n "$line" ]; do
  line=${line%%#*}
  line=$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
  [ -n "$line" ] || continue
  dgit ls-files -z -- ":(glob)$line" >"$tmp/drop"
  if [ -s "$tmp/drop" ]; then
    n=$(tr -cd '\0' <"$tmp/drop" | wc -c)
    dropped=$((dropped + n))
    xargs -0 git -C "$dest" rm -q --cached -- <"$tmp/drop" >/dev/null
    (cd "$dest" && xargs -0 rm -f -- <"$tmp/drop")
  fi
done <"$exclude"
find "$dest" -mindepth 1 -path "$dest/.git" -prune -o -type d -empty -print0 | xargs -0 -r rmdir -p --ignore-fail-on-non-empty 2>/dev/null || true
echo "export: dropped $dropped private paths"

dgit commit -q -m "ZetaSDR $version"
dgit tag -a "v$version" -m "ZetaSDR $version"
sha=$(dgit rev-parse HEAD)
[ -z "$(dgit remote)" ] || gate_failed "no-remote"

# The gates, in DEST, with DEST's own scripts.
nofile="$tmp/no-exclude"
echo "export: gate public-check"
ZETA_PUBLIC_DENYLIST="$denylist" ZETA_PUBLIC_EXCLUDE="$nofile" \
  bash "$dest/scripts/public-check.sh" "$dest" || gate_failed public-check

gl=$(command -v gitleaks || true)
if [ -z "$gl" ] && [ -x "$HOME/.local/bin/gitleaks" ]; then gl="$HOME/.local/bin/gitleaks"; fi
[ -n "$gl" ] || gate_failed "gitleaks (not found)"
echo "export: gate gitleaks"
"$gl" git --no-banner --redact -c "$dest/.gitleaks.toml" "$dest" || gate_failed gitleaks

if [ -f "$dest/scripts/check-doc-links.sh" ]; then
  echo "export: gate doc-links"
  bash "$dest/scripts/check-doc-links.sh" "$dest" || gate_failed doc-links
fi

if [ "${EXPORT_SKIP_CARGO:-0}" = 1 ]; then
  echo "export: cargo gates skipped (EXPORT_SKIP_CARGO=1)"
else
  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    target=$CARGO_TARGET_DIR
  elif [ -n "${CLAUDE_JOB_DIR:-}" ]; then
    target=$CLAUDE_JOB_DIR/target
  else
    target=$srcp/target
  fi
  mkdir -p "$target"
  target=$(cd "$target" && pwd -P)
  case $target/ in "$dest"/*) die "CARGO_TARGET_DIR must lie outside DEST" ;; esac
  export CARGO_TARGET_DIR=$target
  export PYTHONDONTWRITEBYTECODE=1

  echo "export: gate fmt"
  for d in "$dest"/crates/*/; do
    name=$(basename "$d")
    [ "$name" = zeta-source-soapy-driver ] && continue
    (cd "$dest" && cargo fmt -p "$name" -- --check) || gate_failed "fmt ($name)"
  done
  echo "export: gate clippy"
  (cd "$dest" && cargo clippy --workspace --all-targets --all-features -- -D warnings) ||
    gate_failed clippy
  echo "export: gate build"
  (cd "$dest" && cargo build --workspace) || gate_failed build
  echo "export: gate test"
  (cd "$dest" && cargo test --workspace -- --skip ui_scenarios_pass_in_headless_chromium) ||
    gate_failed test
  echo "export: gate agents-md"
  (cd "$dest" && cargo run -q -p zeta -- agents-md --check) || gate_failed agents-md
  (cd "$dest" && cargo run -q -p zeta -- agents-md --skill --check) || gate_failed "agents-md --skill"
  if command -v cargo-deny >/dev/null 2>&1; then
    echo "export: gate deny"
    (cd "$dest" && cargo deny check licenses bans sources) || gate_failed deny
  else
    echo "export: warning: cargo-deny not found; licence check skipped (CI runs it)" >&2
  fi
fi

# Python plugins the tests start write bytecode beside their sources (their
# environment is cleared, so PYTHONDONTWRITEBYTECODE does not reach them): drop
# the ignored __pycache__ directories, and nothing else.
dgit status --porcelain --ignored | sed -n 's|^!! \(.*/\)\{0,1\}\(__pycache__/\)$|\1\2|p' >"$tmp/pyc"
while IFS= read -r p; do
  [ -n "$p" ] && rm -rf -- "${dest:?}/$p"
done <"$tmp/pyc"

# The gates must leave DEST exactly as committed: no build output, no strays.
if [ -n "$(dgit status --porcelain --ignored)" ]; then
  dgit status --porcelain --ignored | sed 's/^/export: stray /' >&2
  gate_failed "clean-tree"
fi
[ "$(dgit rev-list --count HEAD)" = 1 ] || gate_failed "one-commit"

echo "export: ready at $dest ($sha)"
