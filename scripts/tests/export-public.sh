#!/usr/bin/env bash
# Tests for scripts/export-public.sh, on a tiny fixture repository holding copies
# of the release scripts. The cargo gates are skipped (EXPORT_SKIP_CARGO=1).
# Leak-shaped strings are built at run time so the gate does not flag this file.
set -uo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scripts="$here/.."
if [ -n "${CLAUDE_JOB_DIR:-}" ]; then
  mkdir -p "$CLAUDE_JOB_DIR/tmp"
  base=$(mktemp -d "$CLAUDE_JOB_DIR/tmp/ep-test.XXXXXX")
else
  base=$(mktemp -d "${TMPDIR:-/tmp}/ep-test.XXXXXX")
fi
trap 'rm -rf "$base"' EXIT

secret_host="secret""-host"
printf '%s\n' "# c" "$secret_host" >"$base/deny.txt"
printf '%s\n' "# private paths" "private/**" "NOTES.md" >"$base/excl.txt"

fail() { echo "FAIL $1: $2" >&2; exit 1; }
pass() { echo "ok $1"; }

# fixture NAME: a committed repo in $base/NAME with the release scripts; prints its path.
fixture() {
  local d="$base/$1"
  mkdir -p "$d/scripts" "$d/private"
  cp "$scripts/export-public.sh" "$scripts/public-check.sh" "$scripts/check-doc-links.sh" "$d/scripts/"
  cp "$scripts/../.gitleaks.toml" "$d/"
  echo "# Fixture" >"$d/README.md"
  printf '%s\n' "target/" "*.log" >"$d/.gitignore"
  echo "ssh $secret_host" >"$d/private/deploy.sh"
  echo "notes on $secret_host" >"$d/NOTES.md"
  git -C "$d" init -q -b main
  git -C "$d" add -A
  git -C "$d" -c user.name=Private -c user.email=private@example.invalid commit -q -m "private history"
  echo "$d"
}

# run REPO DEST [VAR=value...]: sets out and rc.
run() {
  local r=$1 dst=$2
  shift 2
  out=$(env ZETA_PUBLIC_DENYLIST="$base/deny.txt" ZETA_PUBLIC_EXCLUDE="$base/excl.txt" \
    EXPORT_SKIP_CARGO=1 "$@" bash "$r/scripts/export-public.sh" "$dst" 2>&1)
  rc=$?
}

refuses_dirty_tree() {
  local d
  d=$(fixture dirty)
  echo "wip" >>"$d/README.md"
  run "$d" "$base/dirty-out"
  [ $rc -eq 1 ] || fail refuses_dirty_tree "rc=$rc $out"
  case $out in *dirty*) ;; *) fail refuses_dirty_tree "$out" ;; esac
  [ ! -e "$base/dirty-out" ] || fail refuses_dirty_tree "DEST was created"
  pass refuses_dirty_tree
}

refuses_nonempty_dest() {
  local d
  d=$(fixture nonempty)
  mkdir -p "$base/nonempty-out"
  echo x >"$base/nonempty-out/x"
  run "$d" "$base/nonempty-out"
  [ $rc -eq 1 ] || fail refuses_nonempty_dest "rc=$rc $out"
  case $out in *"not empty"*) ;; *) fail refuses_nonempty_dest "$out" ;; esac
  pass refuses_nonempty_dest
}

export_has_one_commit_and_tag() {
  local d o
  d=$(fixture one)
  echo "second" >>"$d/README.md"
  git -C "$d" -c user.name=Private -c user.email=private@example.invalid commit -q -am "more history"
  o="$base/one-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail export_has_one_commit_and_tag "rc=$rc $out"
  case $out in *"export: ready at $o ("*")"*) ;; *) fail export_has_one_commit_and_tag "$out" ;; esac
  [ "$(git -C "$o" rev-list --count HEAD)" = 1 ] || fail export_has_one_commit_and_tag "commits"
  [ "$(git -C "$o" log -1 --format=%s)" = "ZetaSDR 0.1.0" ] || fail export_has_one_commit_and_tag "subject"
  [ "$(git -C "$o" rev-parse --abbrev-ref HEAD)" = main ] || fail export_has_one_commit_and_tag "branch"
  [ "$(git -C "$o" rev-parse 'v0.1.0^{commit}')" = "$(git -C "$o" rev-parse HEAD)" ] ||
    fail export_has_one_commit_and_tag "tag"
  case $out in *"($(git -C "$o" rev-parse HEAD))"*) ;; *) fail export_has_one_commit_and_tag "sha: $out" ;; esac
  grep -q second "$o/README.md" || fail export_has_one_commit_and_tag "tree is not the ref's"
  pass export_has_one_commit_and_tag
}

exports_the_named_ref() {
  local d o first
  d=$(fixture ref)
  first=$(git -C "$d" rev-parse HEAD)
  echo "later" >>"$d/README.md"
  git -C "$d" -c user.name=Private -c user.email=private@example.invalid commit -q -am "later"
  o="$base/ref-out"
  out=$(env ZETA_PUBLIC_DENYLIST="$base/deny.txt" ZETA_PUBLIC_EXCLUDE="$base/excl.txt" \
    EXPORT_SKIP_CARGO=1 bash "$d/scripts/export-public.sh" "$o" "$first" 2>&1)
  rc=$?
  [ $rc -eq 0 ] || fail exports_the_named_ref "rc=$rc $out"
  ! grep -q later "$o/README.md" || fail exports_the_named_ref "exported HEAD"
  pass exports_the_named_ref
}

export_has_no_ignored_files() {
  local d o
  d=$(fixture ignored)
  mkdir -p "$d/target"
  echo "build" >"$d/target/out.bin"
  echo "log" >"$d/run.log"
  o="$base/ignored-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail export_has_no_ignored_files "rc=$rc $out"
  [ ! -e "$o/target" ] || fail export_has_no_ignored_files "target/ exported"
  [ ! -e "$o/run.log" ] || fail export_has_no_ignored_files "run.log exported"
  [ -f "$o/.gitignore" ] || fail export_has_no_ignored_files ".gitignore missing"
  pass export_has_no_ignored_files
}

# A tracked file the tree's .gitignore matches is still tracked, so it is exported.
export_keeps_force_tracked_files() {
  local d o
  d=$(fixture forced)
  echo "kept" >"$d/fixture.log"
  git -C "$d" add -f fixture.log
  git -C "$d" -c user.name=Private -c user.email=private@example.invalid commit -q -m "forced"
  o="$base/forced-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail export_keeps_force_tracked_files "rc=$rc $out"
  git -C "$o" ls-files --error-unmatch fixture.log >/dev/null 2>&1 ||
    fail export_keeps_force_tracked_files "fixture.log not in the commit"
  pass export_keeps_force_tracked_files
}

export_fails_on_leak() {
  local d o
  d=$(fixture leak)
  echo "ssh $secret_host" >"$d/docs.md"
  git -C "$d" add docs.md
  git -C "$d" -c user.name=Private -c user.email=private@example.invalid commit -q -m "leak"
  o="$base/leak-out"
  run "$d" "$o"
  [ $rc -eq 1 ] || fail export_fails_on_leak "rc=$rc $out"
  case $out in *"export: public-check failed"*) ;; *) fail export_fails_on_leak "$out" ;; esac
  case $out in *"export: ready"*) fail export_fails_on_leak "said ready: $out" ;; esac
  case $out in *"$secret_host"*) fail export_fails_on_leak "pattern printed: $out" ;; esac
  pass export_fails_on_leak
}

export_drops_excluded_paths() {
  local d o
  d=$(fixture drop)
  o="$base/drop-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail export_drops_excluded_paths "rc=$rc $out"
  [ ! -e "$o/private" ] || fail export_drops_excluded_paths "private/ exported"
  [ ! -e "$o/NOTES.md" ] || fail export_drops_excluded_paths "NOTES.md exported"
  [ -z "$(git -C "$o" ls-files private NOTES.md)" ] || fail export_drops_excluded_paths "tracked in DEST"
  # The check inside DEST runs with no exclude file: a private path the exclude
  # file misses fails there.
  printf '%s\n' "NOTES.md" >"$base/excl-short.txt"
  run "$d" "$base/drop-out2" ZETA_PUBLIC_EXCLUDE="$base/excl-short.txt"
  [ $rc -eq 1 ] || fail export_drops_excluded_paths "missed exclusion: rc=$rc $out"
  case $out in *"private/deploy.sh:1: denylist"*) ;; *) fail export_drops_excluded_paths "$out" ;; esac
  pass export_drops_excluded_paths
}

refuses_without_private_files() {
  local d
  d=$(fixture nofiles)
  run "$d" "$base/nofiles-out" ZETA_PUBLIC_DENYLIST="$base/none"
  [ $rc -eq 1 ] || fail refuses_without_private_files "denylist: rc=$rc $out"
  run "$d" "$base/nofiles-out" ZETA_PUBLIC_EXCLUDE="$base/none"
  [ $rc -eq 1 ] || fail refuses_without_private_files "exclude: rc=$rc $out"
  pass refuses_without_private_files
}

author_is_public_author() {
  local d o
  d=$(fixture author)
  o="$base/author-out"
  run "$d" "$o" PUBLIC_DATE=2026-12-01
  [ $rc -eq 0 ] || fail author_is_public_author "rc=$rc $out"
  [ "$(git -C "$o" log -1 --format='%an <%ae>|%cn <%ce>')" = \
    "Sage Grigull <ciphernaut@proton.me>|Sage Grigull <ciphernaut@proton.me>" ] ||
    fail author_is_public_author "default: $(git -C "$o" log -1 --format='%an <%ae>|%cn <%ce>')"
  [ "$(git -C "$o" log -1 --format='%aI|%cI')" = "2026-12-01T00:00:00+00:00|2026-12-01T00:00:00+00:00" ] ||
    fail author_is_public_author "date: $(git -C "$o" log -1 --format='%aI|%cI')"
  [ "$(git -C "$o" for-each-ref --format='%(taggername) %(taggeremail)' refs/tags/v0.1.0)" = \
    "Sage Grigull <ciphernaut@proton.me>" ] || fail author_is_public_author "tagger"
  o="$base/author-out2"
  run "$d" "$o" PUBLIC_AUTHOR="Some One <one@example.invalid>"
  [ $rc -eq 0 ] || fail author_is_public_author "override: rc=$rc $out"
  [ "$(git -C "$o" log -1 --format='%an <%ae>|%cn <%ce>')" = \
    "Some One <one@example.invalid>|Some One <one@example.invalid>" ] ||
    fail author_is_public_author "override"
  run "$d" "$base/author-out3" PUBLIC_DATE=01-12-2026
  [ $rc -eq 1 ] || fail author_is_public_author "bad date: rc=$rc"
  pass author_is_public_author
}

no_remote() {
  local d o
  d=$(fixture remote)
  git -C "$d" remote add origin "https://example.invalid/private.git"
  o="$base/remote-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail no_remote "rc=$rc $out"
  [ -z "$(git -C "$o" remote)" ] || fail no_remote "DEST has a remote"
  ! grep -q example.invalid "$o/.git/config" || fail no_remote "config names the private remote"
  pass no_remote
}

refuses_dirty_tree
refuses_nonempty_dest
refuses_without_private_files
export_has_one_commit_and_tag
exports_the_named_ref
export_has_no_ignored_files
export_keeps_force_tracked_files
export_fails_on_leak
export_drops_excluded_paths
author_is_public_author
no_remote
echo "all export-public tests passed"
