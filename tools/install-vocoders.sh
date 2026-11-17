#!/usr/bin/env bash
# install-vocoders.sh: build zeta-mbe, the P25 and DMR speech decoder, on this machine.
#
# The project ships no IMBE or AMBE implementation, and no zeta-mbe. This script downloads
# zeta-mbe (https://github.com/ciphernaut/zeta-mbe), a stdin/stdout wrapper around mbelib-neo,
# at one pinned tag and commit, and runs its install.sh, which downloads mbelib-neo from its
# authors at its own pinned commit, builds both here, tests the program and installs it under
# --prefix. Nothing it builds belongs to, or is redistributed with, this project:
# docs/vocoders.md.
#
#   bash tools/install-vocoders.sh [--yes] [--src DIR] [--zeta-mbe DIR] [--prefix DIR]
set -euo pipefail

ZETA_MBE_TAG=v1.0.0
ZETA_MBE_COMMIT=bcaa67a1d612f21b571144d3122b9e39a72c63a3
ZETA_MBE_UPSTREAM=https://github.com/ciphernaut/zeta-mbe

usage() {
  cat <<EOF
usage: bash tools/install-vocoders.sh [--yes] [--src DIR] [--zeta-mbe DIR] [--prefix DIR]

Builds zeta-mbe $ZETA_MBE_TAG, the speech decoder for the node's P25 (IMBE) and
DMR (AMBE+2) rows, against mbelib-neo, on this machine. Read docs/vocoders.md
first.

  --yes           accept the notice without the prompt
  --src DIR       build from an mbelib-neo source tree you already have and
                  download no mbelib-neo (it needs the tree's bundled PFFFT
                  archive, third_party/pffft-*.tar.gz, as in mbelib-neo's
                  release source archives)
  --zeta-mbe DIR  build from a zeta-mbe checkout you already have and clone
                  nothing; with --src as well, nothing is downloaded
  --prefix DIR    install DIR/bin/zeta-mbe (default: \$HOME/.local)
  --help          print this and exit

Needs git (without both --src and --zeta-mbe), cmake and a C compiler.
EOF
}

die() {
  echo "install-vocoders: $*" >&2
  exit 1
}

yes=0
src=""
wrapper=""
prefix=""
while [ $# -gt 0 ]; do
  case "$1" in
    --yes) yes=1 ;;
    --src | --zeta-mbe | --prefix)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      case "$1" in
        --src) src=$2 ;;
        --zeta-mbe) wrapper=$2 ;;
        --prefix) prefix=$2 ;;
      esac
      shift
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  shift
done
if [ -z "$prefix" ]; then
  [ -n "${HOME:-}" ] || die "no --prefix and no HOME: give --prefix DIR"
  prefix=$HOME/.local
fi

# The tools, by the Debian package that has each, before anything is asked or fetched.
cc=${CC:-cc}
missing=()
if { [ -z "$src" ] || [ -z "$wrapper" ]; } && ! command -v git >/dev/null 2>&1; then
  missing+=(git)
fi
command -v cmake >/dev/null 2>&1 || missing+=(cmake)
command -v "$cc" >/dev/null 2>&1 || missing+=(build-essential)
if [ ${#missing[@]} -gt 0 ]; then
  echo "install-vocoders: missing tools; on Debian or Ubuntu: sudo apt install ${missing[*]}" >&2
  exit 1
fi
if [ -n "$wrapper" ]; then
  [ -f "$wrapper/install.sh" ] && [ -f "$wrapper/zeta-mbe.c" ] ||
    die "--zeta-mbe $wrapper is not a zeta-mbe checkout (no install.sh and zeta-mbe.c)"
  wrapper=$(cd "$wrapper" && pwd)
fi
if [ -n "$src" ]; then
  src=$(cd "$src" && pwd) || die "--src $src: not a directory"
fi

cat <<EOF
Before you go on:

- AMBE and IMBE are patented. This project ships no implementation of them and
  builds none.
- This script downloads zeta-mbe $ZETA_MBE_TAG ($ZETA_MBE_UPSTREAM), a
  wrapper around mbelib-neo, and runs its install script. That downloads
  mbelib-neo from its authors (https://github.com/arancormonk/mbelib-neo),
  under their licence (GPL-2.0-or-later), and builds both on your machine at
  your request; the library's own build also fetches the FFT code it compiles
  in (PFFFT, from the same authors, checksum-verified). With --src and
  --zeta-mbe it downloads nothing. The program it produces, zeta-mbe, is yours
  and is not to be redistributed with this project.
- Whether you may build or use it where you live is your responsibility.
- Encrypted calls are never decoded.
- Without the vocoder everything except the speech still works.

See docs/vocoders.md.
EOF

if [ "$yes" -ne 1 ]; then
  printf 'Type yes to build zeta-mbe: '
  answer=""
  read -r answer || true
  [ "$answer" = "yes" ] || die "not confirmed; nothing downloaded or built"
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/zeta-mbe-wrapper.XXXXXX")
trap 'rm -rf "$work"' EXIT

if [ -z "$wrapper" ]; then
  wrapper="$work/zeta-mbe"
  echo "install-vocoders: cloning $ZETA_MBE_UPSTREAM at $ZETA_MBE_TAG"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$ZETA_MBE_TAG" \
    "$ZETA_MBE_UPSTREAM" "$wrapper"
  got=$(git -C "$wrapper" rev-parse HEAD)
  [ "$got" = "$ZETA_MBE_COMMIT" ] ||
    die "zeta-mbe $ZETA_MBE_TAG is at $got upstream, not the pinned $ZETA_MBE_COMMIT; refusing to build it"
  echo "install-vocoders: zeta-mbe $ZETA_MBE_TAG ($ZETA_MBE_COMMIT)"
else
  echo "install-vocoders: the zeta-mbe checkout $wrapper"
fi

# The notice above is the one zeta-mbe's script would stop at, so it is accepted for it.
args=(--yes --prefix "$prefix")
[ -z "$src" ] || args+=(--src "$src")
bash "$wrapper/install.sh" "${args[@]}" || die "zeta-mbe's install.sh failed; see above"
