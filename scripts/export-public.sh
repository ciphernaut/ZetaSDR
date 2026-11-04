#!/usr/bin/env bash
# export-public.sh [--squash] DEST [REF]: build the public repository at DEST and
# run every gate again inside it. It never adds a remote and never pushes, and it
# never writes to the source repository: history is rewritten only in DEST.
#
# History mode (the default): DEST is a fresh `git clone --no-local` of REF's
# branch (REF is a branch, or HEAD on one), renamed `main`, with every remote
# removed. `git filter-repo` then rewrites that clone's whole history: the
# exclude file's paths and every private path that existed only in history
# (one the tip's .gitignore ignores, such as scratch/, or one whose name the
# denylist matches) are dropped from every commit, and the replacements file is
# applied to every blob and every commit message. Other identities are mapped to
# PUBLIC_AUTHOR; commit dates are kept. The tip is tagged v0.1.0.
# --squash: a fresh history of one commit holding REF's tree (REF may be any
# commit), the private paths dropped, authored by PUBLIC_AUTHOR at PUBLIC_DATE.
#
# Prints "export: ready at DEST (<sha>, <n> commits); skipped: <gates|none>", or
# "export: <gate> failed" and exits 1 (DEST is left for inspection). A history
# gate names each hit as "history: <commit7>:<path>: <check>" and never prints
# the text that matched. An inherited git environment (GIT_DIR and friends) is
# dropped, every git command in DEST is checked to act on DEST's own repository,
# and DEST/.git outside its objects is checked too (gate metadata).
#
# ZETA_PUBLIC_DENYLIST      the private denylist public-check reads (required)
#                           (default $XDG_CONFIG_HOME/zetasdr/public-denylist)
# ZETA_PUBLIC_EXCLUDE       git pathspec globs of the private paths to drop (required)
#                           (default $XDG_CONFIG_HOME/zetasdr/public-exclude)
# ZETA_PUBLIC_REPLACEMENTS  git filter-repo --replace-text expressions, one per line
#                           (`literal==>replacement`, `regex:PATTERN==>replacement`;
#                           `#` lines and blanks are dropped first), required in
#                           history mode (default $XDG_CONFIG_HOME/zetasdr/public-replacements)
# GIT_FILTER_REPO           the git-filter-repo program (default: on PATH, else
#                           ~/.local/bin/git-filter-repo; `uv tool install git-filter-repo`)
# PUBLIC_AUTHOR             author, committer and tagger (default Sage Grigull <ciphernaut@proton.me>)
# PUBLIC_DATE               release date YYYY-MM-DD for the tag (and the --squash commit)
#                           (default today, UTC)
# CARGO_TARGET_DIR          build output for the cargo gates (default $CLAUDE_JOB_DIR/target,
#                           else the source checkout's target/); never inside DEST
# EXPORT_SKIP_CARGO=1       skip the cargo gates (the script's own tests); listed as skipped
set -euo pipefail

version=0.1.0
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# A git environment inherited from the caller overrides `git -C`: with GIT_DIR
# set, the rewrite and the cleanup below would act on that repository instead
# of DEST. None of it is used.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY \
  GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR GIT_NAMESPACE GIT_PREFIX \
  GIT_CONFIG GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT GIT_CEILING_DIRECTORIES

die() {
  echo "export: $*" >&2
  exit 1
}

mode=history
if [ "${1:-}" = --squash ]; then
  mode=squash
  shift
fi
[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: export-public.sh [--squash] DEST [REF]"
dest_arg=$1
ref=${2:-HEAD}

src=$(git -C "$here/.." rev-parse --show-toplevel) || die "not in a git repository"
srcp=$(cd "$src" && pwd -P)
cfg=${XDG_CONFIG_HOME:-$HOME/.config}/zetasdr
denylist=${ZETA_PUBLIC_DENYLIST:-$cfg/public-denylist}
exclude=${ZETA_PUBLIC_EXCLUDE:-$cfg/public-exclude}
replacements=${ZETA_PUBLIC_REPLACEMENTS:-$cfg/public-replacements}
author=${PUBLIC_AUTHOR:-Sage Grigull <ciphernaut@proton.me>}
date=${PUBLIC_DATE:-$(date -u +%Y-%m-%d)}

[ -f "$denylist" ] || die "no denylist at $denylist (set ZETA_PUBLIC_DENYLIST)"
[ -f "$exclude" ] || die "no exclude file at $exclude (set ZETA_PUBLIC_EXCLUDE)"
if [ $mode = history ]; then
  [ -f "$replacements" ] || die "no replacements file at $replacements (set ZETA_PUBLIC_REPLACEMENTS)"
fi
[[ $date =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || die "PUBLIC_DATE must be YYYY-MM-DD"
date -u -d "$date" +%F >/dev/null 2>&1 || die "PUBLIC_DATE is not a date"
[[ $author =~ ^([^<>]+)\ \<([^<>@ ]+@[^<>@ ]+)\>$ ]] || die "PUBLIC_AUTHOR must be 'Name <email>'"
author_name=${BASH_REMATCH[1]}
author_email=${BASH_REMATCH[2]}

if [ -n "$(git -C "$src" status --porcelain)" ]; then
  die "the working tree at $src is dirty; commit or remove the changes first"
fi
sha_src=$(git -C "$src" rev-parse --verify --quiet "$ref^{commit}") || die "unknown ref $ref"

branch=
if [ $mode = history ]; then
  if [ "$ref" = HEAD ]; then
    branch=$(git -C "$src" symbolic-ref --quiet --short HEAD) ||
      die "HEAD is detached; history mode needs a branch (or use --squash)"
  elif git -C "$src" show-ref --verify --quiet "refs/heads/$ref"; then
    branch=$ref
  else
    die "$ref is not a branch; history mode needs one (or use --squash)"
  fi
  fr=${GIT_FILTER_REPO:-$(command -v git-filter-repo || true)}
  if [ -z "$fr" ] && [ -x "$HOME/.local/bin/git-filter-repo" ]; then fr="$HOME/.local/bin/git-filter-repo"; fi
  [ -n "$fr" ] && [ -x "$fr" ] || die "git-filter-repo not found (uv tool install git-filter-repo, or set GIT_FILTER_REPO)"
fi

# DEST: refused inside the source checkout before anything is created.
dest_abs=$(realpath -m -- "$dest_arg")
case $dest_abs/ in "$srcp"/*) die "DEST must lie outside the source checkout" ;; esac
if [ -e "$dest_abs" ]; then
  [ -d "$dest_abs" ] || die "$dest_arg exists and is not a directory"
  [ -z "$(ls -A "$dest_abs")" ] || die "$dest_arg is not empty"
fi
mkdir -p "$dest_abs"
dest=$(cd "$dest_abs" && pwd -P)
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
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
# Every identity git records in DEST (reflogs included) is the public one, never
# the machine's default (user@host) or an inherited EMAIL.
export GIT_AUTHOR_NAME="$author_name" GIT_AUTHOR_EMAIL="$author_email" \
  GIT_COMMITTER_NAME="$author_name" GIT_COMMITTER_EMAIL="$author_email"
unset EMAIL
qgit() {
  git -C "$dest" -c core.hooksPath=/dev/null -c commit.gpgSign=false -c tag.gpgSign=false "$@"
}
# ... and as PUBLIC_AUTHOR at PUBLIC_DATE, for the tag and the --squash commit.
dgit() {
  GIT_AUTHOR_NAME="$author_name" GIT_AUTHOR_EMAIL="$author_email" \
    GIT_COMMITTER_NAME="$author_name" GIT_COMMITTER_EMAIL="$author_email" \
    GIT_AUTHOR_DATE="${date}T00:00:00Z" GIT_COMMITTER_DATE="${date}T00:00:00Z" \
    qgit "$@"
}

gate_failed() {
  echo "export: $1 failed; $dest is left for inspection" >&2
  exit 1
}

# assert_dest_repo: git run with -C DEST acts on DEST's own repository (checked
# before every step that rewrites or deletes), and borrows no objects.
assert_dest_repo() {
  local g
  g=$(qgit rev-parse --absolute-git-dir 2>/dev/null) || gate_failed "dest-repo (no repository in DEST)"
  [ "$(realpath -- "$g")" = "$(realpath -- "$dest/.git")" ] ||
    gate_failed "dest-repo (git would act on another repository)"
  [ ! -e "$dest/.git/objects/info/alternates" ] || gate_failed "alternates (DEST borrows objects)"
}

skipped=()

# The exclude file's globs, comments and blanks dropped.
sed -e 's/#.*$//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e '/^$/d' "$exclude" >"$tmp/globs"
sed -e 's/^[[:space:]]*#.*$//' -e '/^[[:space:]]*$/d' "$denylist" >"$tmp/deny"

# git_glob_match LIST: prints the lines of LIST (paths) that a glob of the
# exclude file matches, with git's pathspec glob rules (`*` stops at `/`, `**`
# does not).
git_glob_match() {
  python3 -I - "$tmp/globs" "$1" <<'PY'
import re, sys
def rx(g):
    out, i = "", 0
    while i < len(g):
        if g.startswith("**/", i):
            out += "(?:.*/)?"; i += 3
        elif g.startswith("/**", i) and i + 3 == len(g):
            out += "/.*"; i += 3
        elif g.startswith("**", i):
            out += ".*"; i += 2
        elif g[i] == "*":
            out += "[^/]*"; i += 1
        elif g[i] == "?":
            out += "[^/]"; i += 1
        elif g[i] == "[":
            j = g.find("]", i + 1)
            if j < 0:
                out += re.escape(g[i]); i += 1
            else:
                out += g[i:j + 1]; i = j + 1
        else:
            out += re.escape(g[i]); i += 1
    return re.compile(out + r"\Z")
globs = [rx(l.rstrip("\n")) for l in open(sys.argv[1]) if l.strip()]
for p in open(sys.argv[2], encoding="utf-8", errors="surrogateescape"):
    p = p.rstrip("\n")
    if p and any(g.match(p) for g in globs):
        print(p)
PY
}

# deny_match LIST: prints the lines of LIST the denylist matches.
deny_match() {
  [ -s "$tmp/deny" ] || return 0
  local s=0
  grep -iE -f "$tmp/deny" "$1" 2>/dev/null || s=$?
  [ "$s" -le 1 ] || die "grep failed on the denylist (status $s)"
}

echo "export: $mode of $ref ($sha_src) to $dest"

if [ $mode = squash ]; then
  git -C "$src" archive --format=tar "$sha_src" | tar -x -C "$dest" || gate_failed archive
  dgit init -q -b main --template= || gate_failed init
  assert_dest_repo
  # -f: a tracked file the tree's own .gitignore matches is still part of the export.
  dgit add -A -f || gate_failed add

  # Drop the private paths, with the same pathspec globs public-check skips.
  dropped=0
  while IFS= read -r line; do
    dgit ls-files -z -- ":(glob)$line" >"$tmp/drop" || gate_failed drop
    if [ -s "$tmp/drop" ]; then
      n=$(tr -cd '\0' <"$tmp/drop" | wc -c)
      dropped=$((dropped + n))
      xargs -0 git -C "$dest" rm -q --cached -- <"$tmp/drop" >/dev/null || gate_failed drop
      (cd "$dest" && xargs -0 rm -f -- <"$tmp/drop") || gate_failed drop
    fi
  done <"$tmp/globs"
  # Empty directories left by the drop, removed from inside DEST so that
  # `rmdir -p` never climbs above it.
  (cd "$dest" && find . -mindepth 1 -path ./.git -prune -o -type d -empty -print0 |
    xargs -0 -r rmdir -p --ignore-fail-on-non-empty 2>/dev/null) || true
  echo "export: dropped $dropped private paths"

  dgit commit -q -m "ZetaSDR $version" || gate_failed commit
else
  # A fresh clone of the one branch: the source's objects are only read.
  git clone -q --no-local --no-tags --single-branch --branch "$branch" --template= \
    -c core.hooksPath=/dev/null "$srcp" "$dest" || gate_failed clone
  assert_dest_repo
  [ "$(qgit rev-parse "refs/heads/$branch")" = "$sha_src" ] ||
    gate_failed "clone (the branch moved while cloning)"
  if [ "$branch" != main ]; then qgit branch -m "$branch" main || gate_failed clone; fi
  for r in $(qgit remote); do qgit remote remove "$r" || gate_failed "remote removal"; done
  [ "$(qgit for-each-ref --format='%(refname)')" = refs/heads/main ] || gate_failed "clone (refs)"
  commits_before=$(qgit rev-list --count main)

  # The tip as it must come out: REF's tree minus the excluded paths.
  GIT_INDEX_FILE="$tmp/index" qgit read-tree main || gate_failed tip-tree
  while IFS= read -r g; do
    GIT_INDEX_FILE="$tmp/index" qgit rm -r -q --cached --ignore-unmatch -- ":(glob)$g" >/dev/null ||
      gate_failed tip-tree
  done <"$tmp/globs"
  want_tree=$(GIT_INDEX_FILE="$tmp/index" qgit write-tree) || gate_failed tip-tree
  qgit ls-tree -r "$want_tree" >"$tmp/want-tree"

  # Every path in any commit, and those only history had.
  qgit rev-list --all >"$tmp/commits"
  while IFS= read -r c; do qgit ls-tree -r -z --name-only "$c"; done <"$tmp/commits" |
    tr '\0' '\n' | sort -u >"$tmp/paths-ever"
  qgit ls-tree -r --name-only main | sort -u >"$tmp/paths-tip"
  comm -23 "$tmp/paths-ever" "$tmp/paths-tip" >"$tmp/paths-gone"
  # Private paths of the past: ignored by the tip's .gitignore, or named by the denylist.
  s=0
  qgit check-ignore --no-index --stdin <"$tmp/paths-gone" >"$tmp/gone-ignored" || s=$?
  [ "$s" -le 1 ] || gate_failed "drop (check-ignore)"
  deny_match "$tmp/paths-gone" >"$tmp/gone-denied"
  {
    sed 's/^/glob:/' "$tmp/globs"
    sort -u "$tmp/gone-ignored" "$tmp/gone-denied" | sed 's/^/literal:/'
  } >"$tmp/drop-paths"
  n_glob=$(git_glob_match "$tmp/paths-ever" | wc -l)
  n_gone=$(sort -u "$tmp/gone-ignored" "$tmp/gone-denied" | wc -l)
  echo "export: dropping $n_glob excluded and $n_gone history-only private paths from every commit"

  # The replacements, comments and blanks dropped (filter-repo would otherwise
  # replace a comment line itself); every rule must name its replacement.
  sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$replacements" >"$tmp/replace"
  n=0
  while IFS= read -r rule || [ -n "$rule" ]; do
    n=$((n + 1))
    case $rule in *'==>'*) ;; *) die "replacements: rule $n has no ==>" ;; esac
  done <"$tmp/replace"
  echo "export: $n replacement rules"

  # Other identities in history become PUBLIC_AUTHOR.
  qgit log --format='%an <%ae>%n%cn <%ce>' main | sort -u >"$tmp/idents"
  mailmap=()
  : >"$tmp/mailmap"
  while IFS= read -r id; do
    [ "$id" = "$author_name <$author_email>" ] && continue
    printf '%s <%s> %s\n' "$author_name" "$author_email" "$id" >>"$tmp/mailmap"
  done <"$tmp/idents"
  if [ -s "$tmp/mailmap" ]; then
    mailmap=(--mailmap "$tmp/mailmap")
    echo "export: mapping $(wc -l <"$tmp/mailmap") other identities to PUBLIC_AUTHOR"
  fi

  qgit cat-file --batch-all-objects --batch-check='%(objectname) %(objecttype)' |
    sed -n 's/ blob$//p' | sort >"$tmp/blobs-before"
  qgit log -z --format=%B main >"$tmp/msgs-before"

  # The rewrite. main is the only ref, so the whole history is that branch; the
  # full (not --refs) mode also expires the reflogs and repacks, so the old
  # objects leave DEST.
  assert_dest_repo
  (cd "$dest" && "$fr" --force --quiet \
    --invert-paths --paths-from-file "$tmp/drop-paths" \
    --replace-text "$tmp/replace" --replace-message "$tmp/replace" "${mailmap[@]}") ||
    gate_failed filter-repo
  rm -rf "$dest/.git/filter-repo"
  assert_dest_repo
  qgit reflog expire --expire=now --all || gate_failed filter-repo
  qgit gc -q --prune=now || gate_failed filter-repo
  qgit reset -q --hard main || gate_failed filter-repo
  qgit clean -q -ffdx || gate_failed filter-repo
  # No remote-tracking refs (a dangling origin/HEAD) and no reflog entries left.
  qgit for-each-ref --format='%(refname)' refs/remotes >"$tmp/remote-refs"
  while IFS= read -r r; do qgit update-ref -d --no-deref "$r" || gate_failed "remote refs"; done <"$tmp/remote-refs"
  rm -rf "$dest/.git/refs/remotes" "$dest/.git/logs/refs/remotes"
  qgit reflog expire --expire=now --all || gate_failed "reflog"

  qgit cat-file --batch-all-objects --batch-check='%(objectname) %(objecttype)' |
    sed -n 's/ blob$//p' | sort >"$tmp/blobs-after"
  qgit log -z --format=%B main >"$tmp/msgs-after"
  n_blobs=$(comm -13 "$tmp/blobs-before" "$tmp/blobs-after" | wc -l)
  n_msgs=$(python3 -I - "$tmp/msgs-before" "$tmp/msgs-after" <<'PY'
import collections, sys
before = collections.Counter(open(sys.argv[1], "rb").read().split(b"\0"))
n = 0
for m in open(sys.argv[2], "rb").read().split(b"\0"):
    if before[m] > 0:
        before[m] -= 1
    else:
        n += 1
print(n)
PY
)
  echo "export: $(qgit rev-list --count main) of $commits_before commits kept; rewrote $n_blobs blobs and $n_msgs messages"
fi

dgit tag -a "v$version" -m "ZetaSDR $version" || gate_failed tag
sha=$(qgit rev-parse HEAD)
ncommits=$(qgit rev-list --count HEAD)
[ -z "$(qgit remote)" ] || gate_failed "no-remote"

# The allowlist of DEST's tip, for the built-in checks by path.
allow=""
[ -f "$dest/scripts/public-allow.txt" ] && allow="$dest/scripts/public-allow.txt"
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

if [ $mode = history ]; then
  echo "export: gate history"
  [ "$(qgit for-each-ref --format='%(refname)' | sort | tr '\n' ' ')" = "refs/heads/main refs/tags/v$version " ] ||
    gate_failed "history (refs)"
  # Every object DEST holds, reachable or not: blobs as text (binaries through
  # strings), commits and tags as their raw headers and messages.
  mkdir -p "$tmp/obj/blob" "$tmp/obj/commit" "$tmp/obj/tag"
  python3 -I - "$dest" "$tmp/obj" <<'PY' || gate_failed "history (read)"
import os, subprocess, sys
repo, out = sys.argv[1], sys.argv[2]
def git(*a):
    return subprocess.run(["git", "-C", repo, *a], check=True, capture_output=True).stdout
names = {}
for line in git("rev-list", "--objects", "--all").split(b"\n"):
    sha, _, path = line.partition(b" ")
    if sha and path and sha not in names:
        names[sha] = path
objs = [l.split() for l in git("cat-file", "--batch-all-objects",
        "--batch-check=%(objectname) %(objecttype)").split(b"\n") if l]
cat = subprocess.Popen(["git", "-C", repo, "cat-file", "--batch"],
                       stdin=subprocess.PIPE, stdout=subprocess.PIPE)
with open(os.path.join(out, "names"), "wb") as m:
    for sha, typ in objs:
        if typ not in (b"blob", b"commit", b"tag"):
            continue
        cat.stdin.write(sha + b"\n"); cat.stdin.flush()
        size = int(cat.stdout.readline().split()[2])
        data = cat.stdout.read(size); cat.stdout.read(1)
        if typ == b"blob" and b"\0" in data[:8192]:
            data = subprocess.run(["strings", "-n", "4"], input=data, check=True,
                                  capture_output=True).stdout
        with open(os.path.join(out, typ.decode(), sha.decode()), "wb") as f:
            f.write(data)
        if typ == b"blob":
            m.write(sha + b"\t" + names.get(sha, b"") + b"\n")
cat.stdin.close(); cat.wait()
PY
  : >"$tmp/hist-hits"
  # Every (blob, path) pair of every commit: a blob is judged by every path it
  # was stored under.
  qgit rev-list --all >"$tmp/commits"
  while IFS= read -r c; do
    qgit ls-tree -r -z --format='%(objecttype) %(objectname)%x09%(path)' "$c"
  done <"$tmp/commits" | tr '\0' '\n' | sed -n 's/^blob //p' | sort -u >"$tmp/blob-paths"
  cut -f2- "$tmp/blob-paths" | sort -u >"$tmp/paths-after"
  # paths_of SHA: the paths blob SHA was stored under, one per line (none when unreachable).
  paths_of() { sed -n "s|^$1\t||p" "$tmp/blob-paths"; }
  # where SHA [PATH]: "<commit7>:<path>" of the first commit holding blob SHA (at PATH).
  where() {
    local p c
    p=${2:-$(paths_of "$1" | sed -n 1p)}
    [ -n "$p" ] && c=$(qgit log --all --reverse --format=%h --find-object="$1" -- ":(literal)$p" | sed -n 1p)
    if [ -n "$p" ] && [ -n "${c:-}" ]; then echo "$c:$p"; else echo "unreachable ${1:0:7}"; fi
  }
  hist_grep() {
    local s=0
    grep "$@" 2>/dev/null || s=$?
    [ "$s" -le 1 ] || gate_failed "history (grep status $s)"
  }
  coord_re='-?[0-9]{1,3}\.[0-9]{4,}, ?-?[0-9]{1,3}\.[0-9]{4,}'
  host_re='[a-z0-9-]+\.local:[0-9]+'
  # (each grep writes a file first: a failure inside a process substitution
  # would not end the gate)
  if [ -s "$tmp/deny" ]; then
    hist_grep -rlaiE -f "$tmp/deny" "$tmp/obj/blob" >"$tmp/hit-blobs"
    while IFS= read -r f; do
      echo "history: $(where "$(basename "$f")"): denylist" >>"$tmp/hist-hits"
    done <"$tmp/hit-blobs"
    hist_grep -rlaiE -f "$tmp/deny" "$tmp/obj/commit" "$tmp/obj/tag" >"$tmp/hit-commits"
    while IFS= read -r f; do
      echo "history: $(qgit rev-parse --short "$(basename "$f")"): message or identity: denylist" >>"$tmp/hist-hits"
    done <"$tmp/hit-commits"
  fi
  for check in coordinates local-host; do
    if [ $check = coordinates ]; then re=$coord_re; else re=$host_re; fi
    # Blobs: a matching line is excused only when every path of the blob allows it.
    hist_grep -rlaE -e "$re" "$tmp/obj/blob" >"$tmp/hit-$check"
    while IFS= read -r f; do
      b=$(basename "$f")
      paths_of "$b" >"$tmp/hit-paths"
      [ -s "$tmp/hit-paths" ] || echo "" >"$tmp/hit-paths"
      hist_grep -naE -e "$re" "$f" >"$tmp/hit-lines"
      while IFS= read -r p; do
        while IFS= read -r m; do
          if ! allowed "$p" "${m#*:}"; then
            echo "history: $(where "$b" "$p"): $check" >>"$tmp/hist-hits"
            break
          fi
        done <"$tmp/hit-lines"
      done <"$tmp/hit-paths"
    done <"$tmp/hit-$check"
    # Messages and identities: only the allowlist's entries for every path (`*`) apply.
    hist_grep -rlaE -e "$re" "$tmp/obj/commit" "$tmp/obj/tag" >"$tmp/hit-msg-$check"
    while IFS= read -r f; do
      hist_grep -aE -e "$re" "$f" >"$tmp/hit-lines"
      while IFS= read -r m; do
        if ! allowed "" "$m"; then
          echo "history: $(qgit rev-parse --short "$(basename "$f")"): message or identity: $check" >>"$tmp/hist-hits"
          break
        fi
      done <"$tmp/hit-lines"
    done <"$tmp/hit-msg-$check"
  done
  # No path in any commit is excluded, named by the denylist or caught by a
  # built-in check (the path is not printed: it may be the private text itself).
  git_glob_match "$tmp/paths-after" >"$tmp/bad-excluded"
  deny_match "$tmp/paths-after" >"$tmp/bad-named"
  : >"$tmp/bad-coordinates"
  : >"$tmp/bad-local-host"
  hist_grep -aE -e "$coord_re" "$tmp/paths-after" >"$tmp/path-coords"
  hist_grep -aE -e "$host_re" "$tmp/paths-after" >"$tmp/path-hosts"
  while IFS= read -r p; do allowed "$p" "$p" || echo "$p" >>"$tmp/bad-coordinates"; done <"$tmp/path-coords"
  while IFS= read -r p; do allowed "$p" "$p" || echo "$p" >>"$tmp/bad-local-host"; done <"$tmp/path-hosts"
  for kind in excluded named coordinates local-host; do
    while IFS= read -r p; do
      c=$(qgit log --all --format=%h -1 -- ":(literal)$p")
      case $kind in
      excluded) echo "history: $c: a path the exclude file matches" >>"$tmp/hist-hits" ;;
      named) echo "history: $c: a path name the denylist matches" >>"$tmp/hist-hits" ;;
      *) echo "history: $c: a path name the $kind check matches" >>"$tmp/hist-hits" ;;
      esac
    done <"$tmp/bad-$kind"
  done
  nobj=$(wc -l <"$tmp/obj/names")
  if [ -s "$tmp/hist-hits" ]; then
    sort -u "$tmp/hist-hits" | sed 's/^/export: /' >&2
    gate_failed history
  fi
  echo "export: history clean ($nobj blobs, $(wc -l <"$tmp/commits") commits)"

  echo "export: gate tip-tree"
  if [ "$(qgit rev-parse "HEAD^{tree}")" != "$want_tree" ]; then
    qgit ls-tree -r HEAD >"$tmp/got-tree"
    diff "$tmp/want-tree" "$tmp/got-tree" | sed -n 's/^[<>] [0-9]* [a-z]* [0-9a-f]*\t/export: tip differs: /p' |
      sort -u >&2 || true
    gate_failed tip-tree
  fi
fi

# DEST/.git outside its objects (config, HEAD, refs, packed-refs, logs, info,
# hooks, the index): nothing the denylist matches, and no borrowed objects.
echo "export: gate metadata"
assert_dest_repo
(cd "$dest/.git" && find . -path ./objects -prune -o -type f -print) | sed 's|^\./||' | sort >"$tmp/meta-files"
: >"$tmp/meta-hits"
if [ -s "$tmp/deny" ]; then
  while IFS= read -r f; do
    s=0
    grep -qaiE -f "$tmp/deny" "$dest/.git/$f" 2>/dev/null || s=$?
    [ "$s" -le 1 ] || gate_failed "metadata (grep status $s)"
    [ "$s" -eq 0 ] && echo "metadata: .git/$f: denylist" >>"$tmp/meta-hits"
  done <"$tmp/meta-files"
fi
if [ -s "$tmp/meta-hits" ]; then
  sed 's/^/export: /' "$tmp/meta-hits" >&2
  gate_failed metadata
fi

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

echo "export: gate doc-links"
[ -f "$dest/scripts/check-doc-links.sh" ] || gate_failed "doc-links (scripts/check-doc-links.sh missing)"
bash "$dest/scripts/check-doc-links.sh" "$dest" || gate_failed doc-links

if [ "${EXPORT_SKIP_CARGO:-0}" = 1 ]; then
  echo "export: cargo gates skipped (EXPORT_SKIP_CARGO=1)"
  skipped+=("cargo (EXPORT_SKIP_CARGO=1)")
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
    skipped+=("deny (cargo-deny not found)")
  fi
fi

# Python plugins the tests start write bytecode beside their sources (their
# environment is cleared, so PYTHONDONTWRITEBYTECODE does not reach them): drop
# the ignored __pycache__ directories, and nothing else.
qgit status --porcelain --ignored | sed -n 's|^!! \(.*/\)\{0,1\}\(__pycache__/\)$|\1\2|p' >"$tmp/pyc"
while IFS= read -r p; do
  [ -n "$p" ] && rm -rf -- "${dest:?}/$p"
done <"$tmp/pyc"

# The gates must leave DEST exactly as committed: no build output, no strays.
if [ -n "$(qgit status --porcelain --ignored)" ]; then
  qgit status --porcelain --ignored | sed 's/^/export: stray /' >&2
  gate_failed "clean-tree"
fi
if [ $mode = squash ]; then
  [ "$ncommits" = 1 ] || gate_failed "one-commit"
fi

if [ ${#skipped[@]} -eq 0 ]; then
  skip_line=none
else
  skip_line=$(printf '%s, ' "${skipped[@]}")
  skip_line=${skip_line%, }
fi
echo "export: ready at $dest ($sha, $ncommits commits); skipped: $skip_line"
