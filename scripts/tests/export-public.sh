#!/usr/bin/env bash
# Tests for scripts/export-public.sh, on tiny fixture repositories holding copies
# of the release scripts. The cargo gates are skipped (EXPORT_SKIP_CARGO=1).
# History mode needs git-filter-repo (uv tool install git-filter-repo).
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
akia="AK""IA""ABCDEFGHIJKLMNOP"
printf '%s\n' "# c" "$secret_host" >"$base/deny.txt"
printf '%s\n' "# private paths" "private/**" "NOTES.md" >"$base/excl.txt"
# A comment line that equals a line of the fixture's README: it must not become a rule.
printf '%s\n' "# Fixture" "$secret_host==>field-host" >"$base/repl.txt"
printf '%s\n' "# Fixture" >"$base/repl-none.txt"

fail() { echo "FAIL $1: $2" >&2; exit 1; }
pass() { echo "ok $1"; }

# commit REPO MESSAGE: commit everything staged, as a private identity.
commit() {
  git -C "$1" -c user.name=Private -c user.email=private@example.invalid commit -q -m "$2"
}

# fixture NAME: a committed repo in $base/NAME with the release scripts; prints its path.
fixture() {
  local d="$base/$1"
  mkdir -p "$d/scripts" "$d/private"
  cp "$scripts/export-public.sh" "$scripts/public-check.sh" "$scripts/check-doc-links.sh" "$d/scripts/"
  cp "$scripts/../.gitleaks.toml" "$d/"
  echo "# Fixture" >"$d/README.md"
  printf '%s\n' "target/" "*.log" "/scratch" "__pycache__/" >"$d/.gitignore"
  echo "ssh $secret_host" >"$d/private/deploy.sh"
  echo "notes on $secret_host" >"$d/NOTES.md"
  git -C "$d" init -q -b main
  git -C "$d" add -A
  commit "$d" "private history"
  echo "$d"
}

# history_fixture NAME: a fixture whose old commit leaks in a file, in its
# message and in two private paths that are gone from the tip.
history_fixture() {
  local d
  d=$(fixture "$1")
  echo "ssh $secret_host" >"$d/docs.md"
  mkdir -p "$d/scratch"
  echo "scratch" >"$d/scratch/old.sh"
  echo "kept apart" >"$d/notes-$secret_host.txt"
  git -C "$d" add docs.md "notes-$secret_host.txt"
  git -C "$d" add -f scratch/old.sh
  commit "$d" "deploy to $secret_host"
  echo "ssh field-host" >"$d/docs.md"
  git -C "$d" rm -q --cached scratch/old.sh "notes-$secret_host.txt"
  rm -rf "$d/scratch" "$d/notes-$secret_host.txt"
  # (a commit left empty by the rewrite is pruned: this one also changes the README)
  echo "Docs in docs.md." >>"$d/README.md"
  git -C "$d" add docs.md README.md
  commit "$d" "docs: the role, not the name"
  echo "$d"
}

# run [--squash] REPO DEST [VAR=value...]: sets out and rc.
run() {
  local m=()
  if [ "$1" = --squash ]; then
    m=(--squash)
    shift
  fi
  local r=$1 dst=$2
  shift 2
  out=$(env ZETA_PUBLIC_DENYLIST="$base/deny.txt" ZETA_PUBLIC_EXCLUDE="$base/excl.txt" \
    ZETA_PUBLIC_REPLACEMENTS="$base/repl.txt" EXPORT_SKIP_CARGO=1 "$@" \
    bash "$r/scripts/export-public.sh" "${m[@]}" "$dst" 2>&1)
  rc=$?
}

# no_secret WHERE: the output never names the private string.
no_secret() {
  case $out in *"$secret_host"*) fail "$1" "pattern printed: $out" ;; esac
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

dest_inside_source_refused_before_mkdir() {
  local d
  d=$(fixture inside)
  run "$d" "$d/out/public"
  [ $rc -eq 1 ] || fail dest_inside_source_refused_before_mkdir "rc=$rc $out"
  case $out in *"outside the source checkout"*) ;; *) fail dest_inside_source_refused_before_mkdir "$out" ;; esac
  [ ! -e "$d/out" ] || fail dest_inside_source_refused_before_mkdir "DEST was created inside the source"
  pass dest_inside_source_refused_before_mkdir
}

refuses_without_private_files() {
  local d
  d=$(fixture nofiles)
  run "$d" "$base/nofiles-out" ZETA_PUBLIC_DENYLIST="$base/none"
  [ $rc -eq 1 ] || fail refuses_without_private_files "denylist: rc=$rc $out"
  run "$d" "$base/nofiles-out" ZETA_PUBLIC_EXCLUDE="$base/none"
  [ $rc -eq 1 ] || fail refuses_without_private_files "exclude: rc=$rc $out"
  run "$d" "$base/nofiles-out" ZETA_PUBLIC_REPLACEMENTS="$base/none"
  [ $rc -eq 1 ] || fail refuses_without_private_files "replacements: rc=$rc $out"
  case $out in *"no replacements file"*) ;; *) fail refuses_without_private_files "$out" ;; esac
  # --squash rewrites nothing, so it needs no replacements file.
  run --squash "$d" "$base/nofiles-out2" ZETA_PUBLIC_REPLACEMENTS="$base/none"
  [ $rc -eq 0 ] || fail refuses_without_private_files "squash: rc=$rc $out"
  pass refuses_without_private_files
}

refuses_a_rule_without_replacement() {
  local d
  d=$(fixture norule)
  printf '%s\n' "# c" "field-host==>x" "$secret_host" >"$base/repl-bad.txt"
  run "$d" "$base/norule-out" ZETA_PUBLIC_REPLACEMENTS="$base/repl-bad.txt"
  [ $rc -eq 1 ] || fail refuses_a_rule_without_replacement "rc=$rc $out"
  case $out in *"rule 2 has no ==>"*) ;; *) fail refuses_a_rule_without_replacement "$out" ;; esac
  no_secret refuses_a_rule_without_replacement
  pass refuses_a_rule_without_replacement
}

history_refuses_a_non_branch_ref() {
  local d
  d=$(fixture nonbranch)
  out=$(env ZETA_PUBLIC_DENYLIST="$base/deny.txt" ZETA_PUBLIC_EXCLUDE="$base/excl.txt" \
    ZETA_PUBLIC_REPLACEMENTS="$base/repl.txt" EXPORT_SKIP_CARGO=1 \
    bash "$d/scripts/export-public.sh" "$base/nonbranch-out2" "$(git -C "$d" rev-parse HEAD)" 2>&1)
  rc=$?
  [ $rc -eq 1 ] || fail history_refuses_a_non_branch_ref "rc=$rc $out"
  case $out in *"is not a branch"*) ;; *) fail history_refuses_a_non_branch_ref "$out" ;; esac
  pass history_refuses_a_non_branch_ref
}

squash_mode_still_works() {
  local d o
  d=$(fixture one)
  echo "second" >>"$d/README.md"
  git -C "$d" add README.md
  commit "$d" "more history"
  o="$base/one-out"
  run --squash "$d" "$o"
  [ $rc -eq 0 ] || fail squash_mode_still_works "rc=$rc $out"
  case $out in *"export: ready at $o ($(git -C "$o" rev-parse HEAD), 1 commits); skipped: "*) ;;
  *) fail squash_mode_still_works "$out" ;; esac
  [ "$(git -C "$o" rev-list --count HEAD)" = 1 ] || fail squash_mode_still_works "commits"
  [ "$(git -C "$o" log -1 --format=%s)" = "ZetaSDR 0.1.0" ] || fail squash_mode_still_works "subject"
  [ "$(git -C "$o" rev-parse --abbrev-ref HEAD)" = main ] || fail squash_mode_still_works "branch"
  [ "$(git -C "$o" rev-parse 'v0.1.0^{commit}')" = "$(git -C "$o" rev-parse HEAD)" ] ||
    fail squash_mode_still_works "tag"
  grep -q second "$o/README.md" || fail squash_mode_still_works "tree is not the ref's"
  pass squash_mode_still_works
}

exports_the_named_ref() {
  local d o first
  d=$(fixture ref)
  first=$(git -C "$d" rev-parse HEAD)
  echo "later" >>"$d/README.md"
  git -C "$d" add README.md
  commit "$d" "later"
  o="$base/ref-out"
  out=$(env ZETA_PUBLIC_DENYLIST="$base/deny.txt" ZETA_PUBLIC_EXCLUDE="$base/excl.txt" \
    EXPORT_SKIP_CARGO=1 bash "$d/scripts/export-public.sh" --squash "$o" "$first" 2>&1)
  rc=$?
  [ $rc -eq 0 ] || fail exports_the_named_ref "squash: rc=$rc $out"
  ! grep -q later "$o/README.md" || fail exports_the_named_ref "squash exported HEAD"
  # History mode: a named branch, not HEAD's.
  git -C "$d" branch -q older "$first"
  o="$base/ref-out2"
  out=$(env ZETA_PUBLIC_DENYLIST="$base/deny.txt" ZETA_PUBLIC_EXCLUDE="$base/excl.txt" \
    ZETA_PUBLIC_REPLACEMENTS="$base/repl.txt" EXPORT_SKIP_CARGO=1 \
    bash "$d/scripts/export-public.sh" "$o" older 2>&1)
  rc=$?
  [ $rc -eq 0 ] || fail exports_the_named_ref "history: rc=$rc $out"
  ! grep -q later "$o/README.md" || fail exports_the_named_ref "history exported HEAD"
  [ "$(git -C "$o" for-each-ref --format='%(refname)' | tr '\n' ' ')" = "refs/heads/main refs/tags/v0.1.0 " ] ||
    fail exports_the_named_ref "refs: $(git -C "$o" for-each-ref --format='%(refname)')"
  pass exports_the_named_ref
}

export_has_no_ignored_files() {
  local d o
  d=$(fixture ignored)
  mkdir -p "$d/target"
  echo "build" >"$d/target/out.bin"
  echo "log" >"$d/run.log"
  for m in history squash; do
    o="$base/ignored-out-$m"
    if [ $m = squash ]; then run --squash "$d" "$o"; else run "$d" "$o"; fi
    [ $rc -eq 0 ] || fail export_has_no_ignored_files "$m: rc=$rc $out"
    [ ! -e "$o/target" ] || fail export_has_no_ignored_files "$m: target/ exported"
    [ ! -e "$o/run.log" ] || fail export_has_no_ignored_files "$m: run.log exported"
    [ -f "$o/.gitignore" ] || fail export_has_no_ignored_files "$m: .gitignore missing"
  done
  pass export_has_no_ignored_files
}

# A tracked file the tree's .gitignore matches is still tracked, so it is exported.
export_keeps_force_tracked_files() {
  local d o m
  d=$(fixture forced)
  echo "kept" >"$d/fixture.log"
  git -C "$d" add -f fixture.log
  commit "$d" "forced"
  for m in history squash; do
    o="$base/forced-out-$m"
    if [ $m = squash ]; then run --squash "$d" "$o"; else run "$d" "$o"; fi
    [ $rc -eq 0 ] || fail export_keeps_force_tracked_files "$m: rc=$rc $out"
    git -C "$o" ls-files --error-unmatch fixture.log >/dev/null 2>&1 ||
      fail export_keeps_force_tracked_files "$m: fixture.log not in the commit"
  done
  pass export_keeps_force_tracked_files
}

export_fails_on_leak() {
  local d o
  d=$(fixture leak)
  echo "ssh $secret_host" >"$d/docs.md"
  git -C "$d" add docs.md
  commit "$d" "leak"
  o="$base/leak-out"
  run --squash "$d" "$o"
  [ $rc -eq 1 ] || fail export_fails_on_leak "rc=$rc $out"
  case $out in *"export: public-check failed"*) ;; *) fail export_fails_on_leak "$out" ;; esac
  case $out in *"export: ready"*) fail export_fails_on_leak "said ready: $out" ;; esac
  no_secret export_fails_on_leak
  pass export_fails_on_leak
}

export_drops_excluded_paths() {
  local d o
  d=$(fixture drop)
  o="$base/drop-out"
  run --squash "$d" "$o"
  [ $rc -eq 0 ] || fail export_drops_excluded_paths "rc=$rc $out"
  [ ! -e "$o/private" ] || fail export_drops_excluded_paths "private/ exported"
  [ ! -e "$o/NOTES.md" ] || fail export_drops_excluded_paths "NOTES.md exported"
  [ -z "$(git -C "$o" ls-files private NOTES.md)" ] || fail export_drops_excluded_paths "tracked in DEST"
  [ -d "$base" ] || fail export_drops_excluded_paths "the prune climbed above DEST"
  # The check inside DEST runs with no exclude file: a private path the exclude
  # file misses fails there.
  printf '%s\n' "NOTES.md" >"$base/excl-short.txt"
  run --squash "$d" "$base/drop-out2" ZETA_PUBLIC_EXCLUDE="$base/excl-short.txt"
  [ $rc -eq 1 ] || fail export_drops_excluded_paths "missed exclusion: rc=$rc $out"
  case $out in *"private/deploy.sh:1: denylist"*) ;; *) fail export_drops_excluded_paths "$out" ;; esac
  pass export_drops_excluded_paths
}

# The empty-directory prune runs from inside DEST: it removes the directories
# the drop emptied and nothing above DEST.
prune_stays_inside_dest() {
  local d o
  d=$(fixture prune)
  mkdir -p "$base/prune-parent/empty-sibling"
  o="$base/prune-parent/out"
  run --squash "$d" "$o"
  [ $rc -eq 0 ] || fail prune_stays_inside_dest "rc=$rc $out"
  [ ! -e "$o/private" ] || fail prune_stays_inside_dest "private/ left"
  [ -d "$base/prune-parent/empty-sibling" ] || fail prune_stays_inside_dest "a directory outside DEST was removed"
  [ -d "$o/.git" ] || fail prune_stays_inside_dest "DEST damaged"
  pass prune_stays_inside_dest
}

author_is_public_author() {
  local d o
  d=$(fixture author)
  o="$base/author-out"
  run --squash "$d" "$o" PUBLIC_DATE=2026-12-01
  [ $rc -eq 0 ] || fail author_is_public_author "rc=$rc $out"
  [ "$(git -C "$o" log -1 --format='%an <%ae>|%cn <%ce>')" = \
    "Sage Grigull <ciphernaut@proton.me>|Sage Grigull <ciphernaut@proton.me>" ] ||
    fail author_is_public_author "default: $(git -C "$o" log -1 --format='%an <%ae>|%cn <%ce>')"
  # Compare epoch seconds: git versions print UTC in %aI as +00:00 or as Z.
  [ "$(git -C "$o" log -1 --format='%at|%ct')" = "1796083200|1796083200" ] ||
    fail author_is_public_author "date: $(git -C "$o" log -1 --format='%aI|%cI')"
  [ "$(git -C "$o" for-each-ref --format='%(taggername) %(taggeremail)' refs/tags/v0.1.0)" = \
    "Sage Grigull <ciphernaut@proton.me>" ] || fail author_is_public_author "tagger"
  o="$base/author-out2"
  run --squash "$d" "$o" PUBLIC_AUTHOR="Some One <one@example.invalid>"
  [ $rc -eq 0 ] || fail author_is_public_author "override: rc=$rc $out"
  [ "$(git -C "$o" log -1 --format='%an <%ae>|%cn <%ce>')" = \
    "Some One <one@example.invalid>|Some One <one@example.invalid>" ] ||
    fail author_is_public_author "override"
  run --squash "$d" "$base/author-out3" PUBLIC_DATE=01-12-2026
  [ $rc -eq 1 ] || fail author_is_public_author "bad date: rc=$rc"
  # History mode: other identities become PUBLIC_AUTHOR, the commit dates stay,
  # the tag is PUBLIC_AUTHOR's at PUBLIC_DATE.
  o="$base/author-out4"
  run "$d" "$o" PUBLIC_DATE=2026-12-01
  [ $rc -eq 0 ] || fail author_is_public_author "history: rc=$rc $out"
  [ "$(git -C "$o" log --all --format='%an <%ae>|%cn <%ce>' | sort -u)" = \
    "Sage Grigull <ciphernaut@proton.me>|Sage Grigull <ciphernaut@proton.me>" ] ||
    fail author_is_public_author "history identities: $(git -C "$o" log --all --format='%an <%ae>|%cn <%ce>' | sort -u)"
  [ "$(git -C "$o" log -1 --format=%aI)" = "$(git -C "$d" log -1 --format=%aI)" ] ||
    fail author_is_public_author "history: the commit date changed"
  [ "$(git -C "$o" for-each-ref --format='%(taggername) %(taggeremail) %(taggerdate:short)' refs/tags/v0.1.0)" = \
    "Sage Grigull <ciphernaut@proton.me> 2026-12-01" ] || fail author_is_public_author "history tagger"
  pass author_is_public_author
}

no_remote() {
  local d o m
  d=$(fixture remote)
  git -C "$d" remote add origin "https://example.invalid/private.git"
  for m in history squash; do
    o="$base/remote-out-$m"
    if [ $m = squash ]; then run --squash "$d" "$o"; else run "$d" "$o"; fi
    [ $rc -eq 0 ] || fail no_remote "$m: rc=$rc $out"
    [ -z "$(git -C "$o" remote)" ] || fail no_remote "$m: DEST has a remote"
    ! grep -q example.invalid "$o/.git/config" || fail no_remote "$m: config names the private remote"
    ! grep -q "$d" "$o/.git/config" || fail no_remote "$m: config names the source"
  done
  pass no_remote
}

history_leak_in_old_blob_is_rewritten() {
  local d o
  d=$(history_fixture hblob)
  o="$base/hblob-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail history_leak_in_old_blob_is_rewritten "rc=$rc $out"
  [ "$(git -C "$o" rev-list --count HEAD)" = 3 ] || fail history_leak_in_old_blob_is_rewritten "commits"
  [ "$(git -C "$o" show HEAD~1:docs.md)" = "ssh field-host" ] ||
    fail history_leak_in_old_blob_is_rewritten "old blob: $(git -C "$o" show HEAD~1:docs.md)"
  ! git -C "$o" log --all -p | grep -q -- "$secret_host" || fail history_leak_in_old_blob_is_rewritten "leak left"
  ! git -C "$o" log --all -p | grep -q -- 'REMOVED' || fail history_leak_in_old_blob_is_rewritten "a comment became a rule"
  case $out in *"export: ready at $o ($(git -C "$o" rev-parse HEAD), 3 commits); skipped: cargo (EXPORT_SKIP_CARGO=1)"*) ;;
  *) fail history_leak_in_old_blob_is_rewritten "ready line: $out" ;; esac
  no_secret history_leak_in_old_blob_is_rewritten
  pass history_leak_in_old_blob_is_rewritten
}

history_leak_in_message_is_rewritten() {
  local d o
  d=$(history_fixture hmsg)
  o="$base/hmsg-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail history_leak_in_message_is_rewritten "rc=$rc $out"
  [ "$(git -C "$o" log -1 --format=%s HEAD~1)" = "deploy to field-host" ] ||
    fail history_leak_in_message_is_rewritten "$(git -C "$o" log -1 --format=%s HEAD~1)"
  ! git -C "$o" log --all --format=%B | grep -q -- "$secret_host" || fail history_leak_in_message_is_rewritten "leak left"
  pass history_leak_in_message_is_rewritten
}

history_private_path_is_dropped() {
  local d o paths c
  d=$(history_fixture hpath)
  o="$base/hpath-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail history_private_path_is_dropped "rc=$rc $out"
  paths=$(for c in $(git -C "$o" rev-list --all); do git -C "$o" ls-tree -r --name-only "$c"; done | sort -u)
  case $paths in *private/* | *NOTES.md* | *scratch/* | *notes-*) fail history_private_path_is_dropped "$paths" ;; esac
  case $paths in *docs.md*) ;; *) fail history_private_path_is_dropped "docs.md lost: $paths" ;; esac
  case $out in *"dropping 2 excluded and 2 history-only private paths"*) ;; *) fail history_private_path_is_dropped "$out" ;; esac
  no_secret history_private_path_is_dropped
  pass history_private_path_is_dropped
}

history_unreplaced_leak_fails() {
  local d o
  d=$(history_fixture hleak)
  o="$base/hleak-out"
  run "$d" "$o" ZETA_PUBLIC_REPLACEMENTS="$base/repl-none.txt"
  [ $rc -eq 1 ] || fail history_unreplaced_leak_fails "rc=$rc $out"
  case $out in *"export: history failed"*) ;; *) fail history_unreplaced_leak_fails "$out" ;; esac
  case $out in *"history: "???????":docs.md: denylist"*) ;; *) fail history_unreplaced_leak_fails "blob not named: $out" ;; esac
  case $out in *"message or identity"*) ;; *) fail history_unreplaced_leak_fails "message not named: $out" ;; esac
  case $out in *"export: ready"*) fail history_unreplaced_leak_fails "said ready: $out" ;; esac
  no_secret history_unreplaced_leak_fails
  pass history_unreplaced_leak_fails
}

history_tip_tree_matches_source() {
  local d o
  d=$(history_fixture htip)
  o="$base/htip-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail history_tip_tree_matches_source "rc=$rc $out"
  [ "$(git -C "$o" ls-tree -r HEAD)" = "$(git -C "$d" ls-tree -r HEAD | grep -v -e 'private/' -e 'NOTES.md')" ] ||
    fail history_tip_tree_matches_source "tip tree differs"
  # A rule that changes today's files fails the gate, naming the file.
  printf '%s\n' "$secret_host==>field-host" "field-host==>elsewhere" >"$base/repl-tip.txt"
  run "$d" "$base/htip-out2" ZETA_PUBLIC_REPLACEMENTS="$base/repl-tip.txt"
  [ $rc -eq 1 ] || fail history_tip_tree_matches_source "changed tip: rc=$rc $out"
  case $out in *"export: tip differs: docs.md"*"export: tip-tree failed"*) ;; *) fail history_tip_tree_matches_source "$out" ;; esac
  pass history_tip_tree_matches_source
}

# state REPO: everything an export could change in the source repository.
state() {
  git -C "$1" for-each-ref
  git -C "$1" count-objects -v
  git -C "$1" status --porcelain --ignored
  git -C "$1" worktree list --porcelain
  cat "$1/.git/config"
  find "$1/.git" -newer "$base/stamp" ! -name index 2>/dev/null
}

source_repo_untouched() {
  local d before after
  d=$(history_fixture untouched)
  git -C "$d" remote add origin "https://example.invalid/private.git"
  git -C "$d" status --porcelain >/dev/null # refresh the index once, before the stamp
  touch "$base/stamp"
  sleep 1
  before=$(state "$d")
  run "$d" "$base/untouched-out"
  [ $rc -eq 0 ] || fail source_repo_untouched "rc=$rc $out"
  run "$d" "$base/untouched-out2" ZETA_PUBLIC_REPLACEMENTS="$base/repl-none.txt"
  [ $rc -eq 1 ] || fail source_repo_untouched "failing run: rc=$rc"
  after=$(state "$d")
  [ "$before" = "$after" ] || fail source_repo_untouched "$(diff <(echo "$before") <(echo "$after"))"
  pass source_repo_untouched
}

# A git environment inherited from the caller (GIT_DIR and friends override
# `git -C`) must not point the rewrite at the source.
inherited_git_dir_is_ignored() {
  local d o before after
  d=$(history_fixture inherited)
  git -C "$d" status --porcelain >/dev/null
  touch "$base/stamp"
  sleep 1
  before=$(state "$d")
  o="$base/inherited-out"
  run "$d" "$o" GIT_DIR="$d/.git" GIT_WORK_TREE="$d" GIT_INDEX_FILE="$d/.git/index" \
    GIT_OBJECT_DIRECTORY="$d/.git/objects" GIT_COMMON_DIR="$d/.git"
  after=$(state "$d")
  [ "$before" = "$after" ] || fail inherited_git_dir_is_ignored "source changed: $(diff <(echo "$before") <(echo "$after"))"
  [ $rc -eq 0 ] || fail inherited_git_dir_is_ignored "rc=$rc $out"
  [ "$(git -C "$o" log -1 --format=%s HEAD~1)" = "deploy to field-host" ] ||
    fail inherited_git_dir_is_ignored "DEST not rewritten"
  pass inherited_git_dir_is_ignored
}

# Grafts, a shallow file and replace refs inherited from the caller change which
# history git sees: none of them is honoured, so DEST keeps every commit.
inherited_grafts_are_ignored() {
  local d o tip n
  d=$(history_fixture grafts)
  tip=$(git -C "$d" rev-parse HEAD)
  n=$(git -C "$d" rev-list --count HEAD)
  echo "$tip" >"$base/graft-file"
  o="$base/grafts-out"
  run "$d" "$o" GIT_GRAFT_FILE="$base/graft-file" GIT_SHALLOW_FILE="$base/graft-file" \
    GIT_REPLACE_REF_BASE=refs/elsewhere/ GIT_NO_REPLACE_OBJECTS=1
  [ $rc -eq 0 ] || fail inherited_grafts_are_ignored "rc=$rc $out"
  [ "$(git -C "$o" rev-list --count HEAD)" = "$n" ] ||
    fail inherited_grafts_are_ignored "$(git -C "$o" rev-list --count HEAD) of $n commits"
  pass inherited_grafts_are_ignored
}

# DEST/.git holds nothing private outside its objects: no machine identity in a
# reflog, no remote-tracking refs or their logs.
dest_metadata_is_clean() {
  local d o
  d=$(history_fixture meta)
  o="$base/meta-out"
  run "$d" "$o" EMAIL="someone@$secret_host" GIT_COMMITTER_NAME="$secret_host" GIT_AUTHOR_NAME="$secret_host"
  [ $rc -eq 0 ] || fail dest_metadata_is_clean "rc=$rc $out"
  ! grep -rqa --exclude-dir=objects -- "$secret_host" "$o/.git" || fail dest_metadata_is_clean "metadata names the private host"
  [ ! -e "$o/.git/refs/remotes" ] || fail dest_metadata_is_clean "refs/remotes left"
  [ ! -e "$o/.git/logs/refs/remotes" ] || fail dest_metadata_is_clean "logs/refs/remotes left"
  [ -z "$(git -C "$o" for-each-ref refs/remotes)" ] || fail dest_metadata_is_clean "a remote-tracking ref left"
  [ ! -s "$o/.git/logs/HEAD" ] || fail dest_metadata_is_clean "a reflog left: $(cat "$o/.git/logs/HEAD")"
  pass dest_metadata_is_clean
}

# fr_wrapper NAME SCRIPT: a git-filter-repo that runs the real one, then SCRIPT in DEST.
fr_wrapper() {
  local real
  real=$(command -v git-filter-repo || echo "$HOME/.local/bin/git-filter-repo")
  printf '#!/bin/sh\n"%s" "$@" || exit 1\n%s\n' "$real" "$2" >"$base/$1"
  chmod +x "$base/$1"
}

metadata_gate_fails() {
  local d
  d=$(history_fixture metagate)
  fr_wrapper fr-meta "echo '$secret_host' >>.git/info/exclude"
  run "$d" "$base/metagate-out" GIT_FILTER_REPO="$base/fr-meta"
  [ $rc -eq 1 ] || fail metadata_gate_fails "rc=$rc $out"
  case $out in *"metadata: .git/info/exclude: denylist"*"export: metadata failed"*) ;; *) fail metadata_gate_fails "$out" ;; esac
  no_secret metadata_gate_fails
  pass metadata_gate_fails
}

no_alternates() {
  local d
  d=$(history_fixture alt)
  mkdir -p "$base/empty-objects"
  fr_wrapper fr-alt "mkdir -p .git/objects/info && echo '$base/empty-objects' >.git/objects/info/alternates"
  run "$d" "$base/alt-out" GIT_FILTER_REPO="$base/fr-alt"
  [ $rc -eq 1 ] || fail no_alternates "rc=$rc $out"
  case $out in *"export: alternates"*"failed"*) ;; *) fail no_alternates "$out" ;; esac
  pass no_alternates
}

# The built-in checks run over messages, identities and path names too.
history_builtin_checks_on_messages_and_paths() {
  local d host
  host="box"".local:8080"
  d=$(fixture builtin)
  echo "a" >"$d/a.md"
  git -C "$d" add a.md
  commit "$d" "served from $host"
  run "$d" "$base/builtin-out1"
  [ $rc -eq 1 ] || fail history_builtin_checks_on_messages_and_paths "message: rc=$rc $out"
  case $out in *"message or identity: local-host"*"export: history failed"*) ;; *) fail history_builtin_checks_on_messages_and_paths "message: $out" ;; esac
  d=$(fixture builtin2)
  mkdir -p "$d/notes"
  echo "a" >"$d/notes/$host.md"
  git -C "$d" add notes
  commit "$d" "notes"
  git -C "$d" rm -q -r notes
  commit "$d" "no notes"
  run "$d" "$base/builtin-out2"
  [ $rc -eq 1 ] || fail history_builtin_checks_on_messages_and_paths "path: rc=$rc $out"
  case $out in *"a path name the local-host check matches"*) ;; *) fail history_builtin_checks_on_messages_and_paths "path: $out" ;; esac
  pass history_builtin_checks_on_messages_and_paths
}

# A coordinate pair in an old blob is excused by the tip's allowlist only when
# every path the blob was stored under is allowed.
history_coordinates_allowed_by_path() {
  local d c
  c="-27.1234""56, 153.1234""56"
  d=$(fixture coords)
  printf '%s\n' "docs/allowed.md:EXAMPLE" >"$d/scripts/public-allow.txt"
  mkdir -p "$d/docs"
  echo "$c EXAMPLE" >"$d/docs/allowed.md"
  git -C "$d" add scripts/public-allow.txt docs
  commit "$d" "an allowed point"
  git -C "$d" rm -q docs/allowed.md
  commit "$d" "gone"
  run "$d" "$base/coords-out1"
  [ $rc -eq 0 ] || fail history_coordinates_allowed_by_path "allowed: rc=$rc $out"
  # The same blob, first under a path the allowlist does not name, later under
  # the allowed one (the newest commit holding it shows only the allowed path).
  d=$(fixture coords2)
  printf '%s\n' "docs/allowed.md:EXAMPLE" >"$d/scripts/public-allow.txt"
  mkdir -p "$d/docs"
  echo "$c EXAMPLE" >"$d/docs/other.md"
  git -C "$d" add scripts/public-allow.txt docs
  commit "$d" "a point elsewhere"
  git -C "$d" mv docs/other.md docs/allowed.md
  commit "$d" "moved where it is allowed"
  git -C "$d" rm -q docs/allowed.md
  commit "$d" "gone"
  run "$d" "$base/coords-out2"
  [ $rc -eq 1 ] || fail history_coordinates_allowed_by_path "elsewhere: rc=$rc $out"
  case $out in *":docs/other.md: coordinates"*) ;; *) fail history_coordinates_allowed_by_path "elsewhere: $out" ;; esac
  case $out in *":docs/allowed.md: coordinates"*) fail history_coordinates_allowed_by_path "the allowed path was flagged: $out" ;; esac
  pass history_coordinates_allowed_by_path
}

gitleaks_hit_fails() {
  local d o
  d=$(fixture leaks)
  printf 'aws_access_key_id = %s\n' "$akia" >"$d/creds.txt"
  git -C "$d" add creds.txt
  commit "$d" "creds"
  git -C "$d" rm -q creds.txt
  commit "$d" "no creds"
  o="$base/leaks-out"
  run "$d" "$o"
  [ $rc -eq 1 ] || fail gitleaks_hit_fails "rc=$rc $out"
  case $out in *"export: gitleaks failed"*) ;; *) fail gitleaks_hit_fails "$out" ;; esac
  pass gitleaks_hit_fails
}

doc_links_required() {
  local d
  d=$(fixture nolinks)
  git -C "$d" rm -q scripts/check-doc-links.sh
  commit "$d" "no link checker"
  run "$d" "$base/nolinks-out"
  [ $rc -eq 1 ] || fail doc_links_required "rc=$rc $out"
  case $out in *"export: doc-links (scripts/check-doc-links.sh missing) failed"*) ;; *) fail doc_links_required "$out" ;; esac
  pass doc_links_required
}

# A gate that leaves an ignored __pycache__ behind is cleaned up; any other
# stray fails clean-tree.
pycache_removed_other_strays_fail() {
  local d o
  d=$(fixture pyc)
  sed -i '1a mkdir -p "$1/scripts/__pycache__" && : >"$1/scripts/__pycache__/m.pyc"' "$d/scripts/check-doc-links.sh"
  git -C "$d" add scripts/check-doc-links.sh
  commit "$d" "a gate writes bytecode"
  o="$base/pyc-out"
  run "$d" "$o"
  [ $rc -eq 0 ] || fail pycache_removed_other_strays_fail "pycache: rc=$rc $out"
  [ ! -e "$o/scripts/__pycache__" ] || fail pycache_removed_other_strays_fail "__pycache__ left"
  sed -i '1a : >"$1/stray.log"' "$d/scripts/check-doc-links.sh"
  git -C "$d" add scripts/check-doc-links.sh
  commit "$d" "a gate leaves a stray"
  run "$d" "$base/pyc-out2"
  [ $rc -eq 1 ] || fail pycache_removed_other_strays_fail "stray: rc=$rc $out"
  case $out in *"export: stray !! stray.log"*"export: clean-tree failed"*) ;; *) fail pycache_removed_other_strays_fail "$out" ;; esac
  pass pycache_removed_other_strays_fail
}

# Every step that can fail is named: the archive and the drop of --squash, the
# rewrite of history mode.
unguarded_steps_named() {
  local d
  d=$(fixture steps)
  mkdir -p "$base/fakebin-tar" "$base/fakebin-xargs"
  printf '#!/bin/sh\nexit 1\n' >"$base/fakebin-tar/tar"
  printf '#!/bin/sh\nexit 1\n' >"$base/fakebin-xargs/xargs"
  chmod +x "$base/fakebin-tar/tar" "$base/fakebin-xargs/xargs"
  run --squash "$d" "$base/steps-out1" PATH="$base/fakebin-tar:$PATH"
  [ $rc -eq 1 ] || fail unguarded_steps_named "archive: rc=$rc $out"
  case $out in *"export: archive failed"*) ;; *) fail unguarded_steps_named "archive: $out" ;; esac
  run --squash "$d" "$base/steps-out2" PATH="$base/fakebin-xargs:$PATH"
  [ $rc -eq 1 ] || fail unguarded_steps_named "drop: rc=$rc $out"
  case $out in *"export: drop failed"*) ;; *) fail unguarded_steps_named "drop: $out" ;; esac
  run "$d" "$base/steps-out3" GIT_FILTER_REPO="$base/fakebin-tar/tar"
  [ $rc -eq 1 ] || fail unguarded_steps_named "filter-repo: rc=$rc $out"
  case $out in *"export: filter-repo failed"*) ;; *) fail unguarded_steps_named "filter-repo: $out" ;; esac
  pass unguarded_steps_named
}

refuses_dirty_tree
refuses_nonempty_dest
dest_inside_source_refused_before_mkdir
refuses_without_private_files
refuses_a_rule_without_replacement
history_refuses_a_non_branch_ref
squash_mode_still_works
exports_the_named_ref
export_has_no_ignored_files
export_keeps_force_tracked_files
export_fails_on_leak
export_drops_excluded_paths
prune_stays_inside_dest
author_is_public_author
no_remote
history_leak_in_old_blob_is_rewritten
history_leak_in_message_is_rewritten
history_private_path_is_dropped
history_unreplaced_leak_fails
history_tip_tree_matches_source
source_repo_untouched
inherited_git_dir_is_ignored
inherited_grafts_are_ignored
dest_metadata_is_clean
metadata_gate_fails
no_alternates
history_builtin_checks_on_messages_and_paths
history_coordinates_allowed_by_path
gitleaks_hit_fails
doc_links_required
pycache_removed_other_strays_fail
unguarded_steps_named
echo "all export-public tests passed"
