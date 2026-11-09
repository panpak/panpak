#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""How much of two packages is the same: the chunks the two have in common, compared by hash, and what both take if each chunk is stored once.

Usage: python3 bench/shared_chunks.py PACKAGE_A PACKAGE_B [--panpak PATH] [--work DIR]

The chunk table of a package comes from `panpak dump-index`: a u32 count and then, for each chunk, 48 bytes (32 hash, 8 offset, 4 raw
size, 4 stored size; PANPAK-FORMAT.md). The sizes below are the stored (compressed) sizes. The packages should be built in the same
image, since the gain only exists when the libraries are byte for byte the same.
"""
import argparse
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def chunks(panpak, package, work):
    out = Path(work) / (Path(package).name + '.idx')
    subprocess.run([panpak, 'dump-index', package, out], check=True, capture_output=True)
    d = out.read_bytes()
    n, = struct.unpack_from('<I', d, 0)
    table = {}
    for i in range(n):
        o = 4 + i * 48
        table[d[o:o + 32]] = struct.unpack_from('<I', d, o + 40)[0]
    return table


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('a')
    ap.add_argument('b')
    ap.add_argument('--panpak', default=str(ROOT / 'target/x86_64-unknown-linux-musl/release/panpak'))
    ap.add_argument('--work', default=None, help='folder for the dumps (default: a temporary folder)')
    a = ap.parse_args()
    with tempfile.TemporaryDirectory(dir=a.work) as work:
        A, B = chunks(a.panpak, a.a, work), chunks(a.panpak, a.b, work)
    shared = set(A) & set(B)
    sa, sb = sum(A.values()), sum(B.values())
    sh_a = sum(A[h] for h in shared)
    sh_b = sum(B[h] for h in shared)
    once = sum({**A, **B}.values())
    print(f'{Path(a.a).name}: {len(A)} chunks, {sa / 1e6:.1f} MB stored')
    print(f'{Path(a.b).name}: {len(B)} chunks, {sb / 1e6:.1f} MB stored')
    print(f'shared: {len(shared)} chunks, {sh_a / 1e6:.1f} MB; {100 * sh_a / sa:.0f}% of the first, {100 * sh_b / sb:.0f}% of the second (stored bytes)')
    print(f'stored separately {(sa + sb) / 1e6:.1f} MB, each chunk once {once / 1e6:.1f} MB: {100 * (1 - once / (sa + sb)):.0f}% less')


if __name__ == '__main__':
    sys.exit(main())
