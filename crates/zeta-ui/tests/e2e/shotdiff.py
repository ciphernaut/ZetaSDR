# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow"]
# ///
"""Compare two directories of screenshots (scenario_theme_shots in ui.py).

Usage: uv run --script crates/zeta-ui/tests/e2e/shotdiff.py BEFORE AFTER

For each PNG in BEFORE, the file of the same name in AFTER must have the same size, and at
most 0.1 % of its pixels may differ by more than 16 in any channel (font hinting moves a few).
Prints `<name>: <n> px differ` per file and exits 1 on any failure.
"""

import os
import sys

from PIL import Image, ImageChops

LEVEL = 16
SHARE = 0.001


def differing(a, b):
    diff = ImageChops.difference(a.convert("RGBA"), b.convert("RGBA"))
    # The largest channel difference per pixel, then the pixels above LEVEL.
    worst = None
    for band in diff.split():
        worst = band if worst is None else ImageChops.lighter(worst, band)
    return sum(worst.point(lambda v: 255 if v > LEVEL else 0).histogram()[255:])


def main(argv):
    if len(argv) != 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    before, after = argv[1], argv[2]
    names = sorted(n for n in os.listdir(before) if n.endswith(".png"))
    if not names:
        print(f"no PNG in {before}", file=sys.stderr)
        return 1
    failed = False
    for name in names:
        other = os.path.join(after, name)
        if not os.path.exists(other):
            print(f"{name}: missing in {after}")
            failed = True
            continue
        with Image.open(os.path.join(before, name)) as a, Image.open(other) as b:
            if a.size != b.size:
                print(f"{name}: size {a.size[0]}x{a.size[1]} became {b.size[0]}x{b.size[1]}")
                failed = True
                continue
            n = differing(a, b)
            limit = a.size[0] * a.size[1] * SHARE
            print(f"{name}: {n} px differ" + ("" if n <= limit else f" (more than {limit:.0f})"))
            failed |= n > limit
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
