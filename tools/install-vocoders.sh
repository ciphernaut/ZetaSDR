#!/usr/bin/env bash
# install-vocoders.sh: build zeta-mbe, the P25 and DMR speech decoder, on this machine.
#
# The project ships no IMBE or AMBE implementation. This script downloads mbelib-neo from its
# authors at one pinned commit, builds it here as a static library, links
# tools/vocoder/zeta-mbe.c against it, tests the program and installs it under --prefix.
# Nothing it builds belongs to, or is redistributed with, this project: docs/vocoders.md.
#
#   bash tools/install-vocoders.sh [--yes] [--src DIR] [--prefix DIR]
set -euo pipefail

PIN_TAG=v2.6.11
PIN_COMMIT=ec8f7d1353d2e234daf61790402866d13a5fd706
UPSTREAM=https://github.com/arancormonk/mbelib-neo

# The self-test: silence frames in, 160 samples of s16le out per frame.
SELFTEST_FRAMES=50
FRAME_OUT_BYTES=320
IMBE_FEED_BYTES=184
AMBE_FEED_BYTES=96

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source_c="$here/vocoder/zeta-mbe.c"

usage() {
  cat <<EOF
usage: bash tools/install-vocoders.sh [--yes] [--src DIR] [--prefix DIR]

Builds zeta-mbe, the speech decoder for the node's P25 (IMBE) and DMR (AMBE+2)
rows, against mbelib-neo $PIN_TAG, on this machine. Read docs/vocoders.md first.

  --yes         accept the notice without the prompt
  --src DIR     build from an mbelib-neo source tree you already have and
                download nothing (it needs the tree's bundled PFFFT archive,
                third_party/pffft-*.tar.gz, as in mbelib-neo's release source
                archives)
  --prefix DIR  install DIR/bin/zeta-mbe (default: \$HOME/.local)
  --help        print this and exit

Needs git (without --src), cmake and a C compiler.
EOF
}

die() {
  echo "install-vocoders: $*" >&2
  exit 1
}

yes=0
src=""
prefix=""
while [ $# -gt 0 ]; do
  case "$1" in
    --yes) yes=1 ;;
    --src)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      src=$2
      shift
      ;;
    --prefix)
      [ $# -ge 2 ] || { usage >&2; exit 2; }
      prefix=$2
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
if [ -z "$src" ] && ! command -v git >/dev/null 2>&1; then
  missing+=(git)
fi
command -v cmake >/dev/null 2>&1 || missing+=(cmake)
command -v "$cc" >/dev/null 2>&1 || missing+=(build-essential)
if [ ${#missing[@]} -gt 0 ]; then
  echo "install-vocoders: missing tools; on Debian or Ubuntu: sudo apt install ${missing[*]}" >&2
  exit 1
fi
[ -f "$source_c" ] || die "not found: $source_c"
if [ -n "$src" ]; then
  [ -f "$src/include/mbelib-neo/mbelib.h" ] || die "--src $src is not an mbelib-neo source tree"
  src=$(cd "$src" && pwd)
  compgen -G "$src/third_party/pffft-*.tar.gz" >/dev/null ||
    die "--src $src has no third_party/pffft-*.tar.gz, so its build would download PFFFT; use mbelib-neo's release source archive, or run without --src"
fi

cat <<EOF
Before you go on:

- AMBE and IMBE are patented. This project ships no implementation of them and
  builds none.
- This script downloads mbelib-neo $PIN_TAG from its authors
  ($UPSTREAM), under their licence (GPL-2.0-or-later),
  and builds it on your machine at your request; the library's own build also
  fetches the FFT code it compiles in (PFFFT, from the same authors,
  checksum-verified). With --src it downloads nothing. The program it produces,
  zeta-mbe, is yours and is not to be redistributed with this project.
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

work=$(mktemp -d "${TMPDIR:-/tmp}/zeta-mbe-build.XXXXXX")
trap 'rm -rf "$work"' EXIT

cmake_offline=()
if [ -z "$src" ]; then
  src="$work/mbelib-neo"
  echo "install-vocoders: cloning $UPSTREAM at $PIN_TAG"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$PIN_TAG" "$UPSTREAM" "$src"
  got=$(git -C "$src" rev-parse HEAD)
  [ "$got" = "$PIN_COMMIT" ] ||
    die "$PIN_TAG is at $got upstream, not the pinned $PIN_COMMIT; refusing to build it"
  built_from="mbelib-neo $PIN_TAG ($PIN_COMMIT)"
else
  cmake_offline=(-DFETCHCONTENT_FULLY_DISCONNECTED=ON)
  built_from="the mbelib-neo source tree $src"
fi

echo "install-vocoders: building mbelib-neo"
cmake -S "$src" -B "$work/build" -DCMAKE_BUILD_TYPE=Release \
  -DMBELIB_BUILD_TESTS=OFF -DMBELIB_BUILD_EXAMPLES=OFF -DMBELIB_WARNINGS_AS_ERRORS=OFF \
  "${cmake_offline[@]}" >"$work/cmake-configure.log" 2>&1 ||
  { cat "$work/cmake-configure.log" >&2; die "cmake could not configure mbelib-neo"; }
cmake --build "$work/build" --target mbe-static --parallel >"$work/cmake-build.log" 2>&1 ||
  { cat "$work/cmake-build.log" >&2; die "mbelib-neo did not build"; }
lib=$(find "$work/build" -name 'libmbe-neo.a' -print -quit)
[ -n "$lib" ] || die "the build left no libmbe-neo.a"

echo "install-vocoders: compiling zeta-mbe"
"$cc" -std=c99 -O2 -Wall -Wextra -I"$src/include" -I"$work/build/include" \
  -o "$work/zeta-mbe" "$source_c" "$lib" -lm

# The self-test runs before the install, so a program that fails it never replaces one the
# node may already use.
built="$work/zeta-mbe"
for codec in imbe7200x4400:$IMBE_FEED_BYTES ambe3600x2450:$AMBE_FEED_BYTES; do
  name=${codec%%:*}
  feed=${codec##*:}
  want=$((SELFTEST_FRAMES * FRAME_OUT_BYTES))
  got=$(head -c $((SELFTEST_FRAMES * feed)) /dev/zero | "$built" "$name" | wc -c) ||
    die "self-test $name: zeta-mbe failed; nothing installed"
  got=$((got))
  [ "$got" -eq "$want" ] ||
    die "self-test $name: $SELFTEST_FRAMES frames in, $got bytes out, not $want; nothing installed"
  echo "install-vocoders: self-test $name: $SELFTEST_FRAMES frames in, $got bytes out: ok"
done

mkdir -p "$prefix/bin"
# Absolute, so the ZETA_HANDLER_ZETA_MBE line below is one the node accepts.
prefix=$(cd "$prefix" && pwd)
bin="$prefix/bin/zeta-mbe"
install -m 755 "$built" "$bin"

cat <<EOF

Built:      $("$bin" --version)
From:       $built_from
Installed:  $bin

The node finds zeta-mbe on its PATH, or where ZETA_HANDLER_ZETA_MBE names it:
  ZETA_HANDLER_ZETA_MBE=$bin
A node running as a service needs it on the service's PATH or that variable in
the service's environment. Check with: zeta modes show p25_decode
EOF
