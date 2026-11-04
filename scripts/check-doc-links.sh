#!/usr/bin/env bash
# check-doc-links.sh [ROOT]: every relative link in the Markdown files git tracks
# under ROOT (default: the repository root), outside docs/superpowers (the design
# history, kept as written), must resolve: the target must be a tracked file or a
# directory holding one, and a #anchor into a Markdown file must name one of its
# headings by GitHub's slug rules (lower case, punctuation dropped, spaces as
# hyphens, a repeated heading numbered -1, -2, ...).
# Links inside code blocks and code spans are not links. Links with a scheme
# (https:, mailto:) are not checked.
# Exit 0 and "doc-links: clean (<n> links)", or exit 1 with one line per broken
# link:
#   <path>:<line>: <link>
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "${1:-$here/..}" && pwd)

if ! command -v python3 >/dev/null 2>&1; then
  echo "doc-links: python3 not found" >&2
  exit 2
fi

python3 -I - "$root" <<'PY'
import os, re, subprocess, sys
from urllib.parse import unquote

root = sys.argv[1]
tracked = subprocess.run(
    ["git", "-C", root, "ls-files", "-z"], check=True, capture_output=True
).stdout.decode().split("\0")
tracked = {t for t in tracked if t}
dirs = set()
for t in tracked:
    parts = t.split("/")[:-1]
    for i in range(1, len(parts) + 1):
        dirs.add("/".join(parts[:i]))
docs = sorted(
    t for t in tracked if t.endswith(".md") and not t.startswith("docs/superpowers/")
)

FENCE = re.compile(r"^\s{0,3}(`{3,}|~{3,})")
CODE_SPAN = re.compile(r"(`+)(?:.+?)\1")
LINK = re.compile(r"!?\[(?:[^\[\]]|\[[^\[\]]*\])*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+(?:\"[^\"]*\"|'[^']*'))?\s*\)")
REF_DEF = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*(\S+)")
HTML_ATTR = re.compile(r"\b(?:src|href)\s*=\s*\"([^\"]+)\"")
HEADING = re.compile(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$")
HTML_ID = re.compile(r"<a\s+(?:name|id)\s*=\s*\"([^\"]+)\"")
SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")


def lines_outside_code(path):
    """(line number, text with code spans removed) for lines outside fences."""
    out = []
    fence = None
    with open(os.path.join(root, path), encoding="utf-8", errors="replace") as f:
        for n, line in enumerate(f, 1):
            m = FENCE.match(line)
            if m:
                mark = m.group(1)
                if fence is None:
                    fence = mark[0] * len(mark)
                elif mark[0] == fence[0] and len(mark) >= len(fence):
                    fence = None
                continue
            if fence is None:
                out.append((n, CODE_SPAN.sub("", line.rstrip("\n"))))
    return out


def slug(text):
    text = re.sub(r"`([^`]*)`", r"\1", text)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"<[^>]+>", "", text)
    text = text.replace("**", "").replace("*", "")
    text = text.strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


anchor_cache = {}


def anchors(path):
    if path in anchor_cache:
        return anchor_cache[path]
    seen = {}
    found = set()
    fence = None
    with open(os.path.join(root, path), encoding="utf-8", errors="replace") as f:
        for line in f:
            m = FENCE.match(line)
            if m:
                mark = m.group(1)
                if fence is None:
                    fence = mark[0] * len(mark)
                elif mark[0] == fence[0] and len(mark) >= len(fence):
                    fence = None
                continue
            if fence is not None:
                continue
            for a in HTML_ID.findall(line):
                found.add(a)
            h = HEADING.match(line)
            if h:
                s = slug(h.group(2))
                k = seen.get(s, 0)
                found.add(s if k == 0 else "%s-%d" % (s, k))
                seen[s] = k + 1
    anchor_cache[path] = found
    return found


broken = []
count = 0
for doc in docs:
    base = os.path.dirname(doc)
    for n, text in lines_outside_code(doc):
        targets = [m.group(1) for m in LINK.finditer(text)]
        targets += HTML_ATTR.findall(text)
        m = REF_DEF.match(text)
        if m:
            targets.append(m.group(1))
        for raw in targets:
            link = raw[1:-1] if raw.startswith("<") and raw.endswith(">") else raw
            if not link or SCHEME.match(link) or link.startswith("//"):
                continue
            count += 1
            path, _, frag = link.partition("#")
            path = unquote(path)
            if path == "":
                target = doc
            elif path.startswith("/"):
                target = os.path.normpath(path.lstrip("/"))
            else:
                target = os.path.normpath(os.path.join(base, path))
            target = target.replace(os.sep, "/")
            if target.startswith("../") or target == "..":
                broken.append((doc, n, raw))
                continue
            if target not in tracked and target not in dirs and target != ".":
                broken.append((doc, n, raw))
                continue
            if frag and target.endswith(".md") and target in tracked:
                if unquote(frag).lower() not in anchors(target):
                    broken.append((doc, n, raw))

if broken:
    for doc, n, raw in broken:
        print("%s:%d: %s" % (doc, n, raw))
    sys.exit(1)
print("doc-links: clean (%d links)" % count)
PY
