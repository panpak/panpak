#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Compare two runs of the app harness (`run.py --json`): the time to the first window of each app, side by side.

Usage: compare.py <main.json> <branch.json>

A comparison of the start of an app is only worth something when both sides ran the SAME app: the same permissions (and the same libraries allowed
to be missing). The harness writes a digest of both in every row; this tool refuses (exit status 2) when an app has a different digest on the two
sides, and says which. Otherwise it prints the two times and the difference, and exits 0.
"""
import json
import sys


def load(path):
    with open(path) as f:
        data = json.load(f)
    return {r["app"]: r for r in data["rows"]}


def compare(a, b):
    """Returns (lines, refused): the text of the comparison and the apps that differ in what they ask for or that one side has no digest for."""
    lines, refused = [], []
    for name in sorted(set(a) & set(b)):
        ra, rb = a[name], b[name]
        da, db = ra.get("permissions"), rb.get("permissions")
        if da is None or db is None or da != db:
            refused.append((name, da, db))
            continue
        wa, wb = ra.get("window_s"), rb.get("window_s")
        if wa is None or wb is None:
            lines.append(f"{name:<16} no window on one side ({wa} / {wb})")
            continue
        lines.append(f"{name:<16}{wa:>8.3f} s{wb:>8.3f} s{(wb - wa) * 1000:>+8.0f} ms   permissions {da}")
    for name in sorted(set(a) ^ set(b)):
        lines.append(f"{name:<16} only on one side")
    return lines, refused


def main(argv):
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    lines, refused = compare(load(argv[0]), load(argv[1]))
    print(f"{'app':<16}{'first':>10}{'second':>10}{'difference':>12}")
    print("\n".join(lines))
    if refused:
        for name, da, db in refused:
            print(f"REFUSED {name}: the two sides do not ask for the same permissions ({da} / {db}); the times say nothing", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
