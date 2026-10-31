#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Check the links between the Markdown files of the repository.

Usage: python3 scripts/check-doc-links.py

For every tracked .md file: each relative link `[text](path#anchor)` must point to a file
that exists and, for a Markdown target, to a heading that exists (GitHub's anchor rules).
`src` and `srcset` attributes of HTML in the text are checked like links.
Paths in backticks that name a tracked file with a directory (for example
`docs/BENCHMARKS.md`) are checked too. Exit status 1 if anything is missing.
"""
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
files = subprocess.run(['git', '-C', str(ROOT), 'ls-files'], capture_output=True, text=True, check=True).stdout.split('\n')
tracked = {f for f in files if f}
mds = sorted(f for f in tracked if f.endswith('.md'))


def slug(heading):
    h = re.sub(r'[`*_~]|<[^>]+>', '', heading.strip().lower())
    h = re.sub(r'\[([^\]]*)\]\([^)]*\)', r'\1', h)
    h = re.sub(r'[^\w\- ]', '', h)
    return h.replace(' ', '-')


def anchors(path):
    out, seen = set(), {}
    fence = False
    for line in (ROOT / path).read_text().splitlines():
        if line.startswith('```'):
            fence = not fence
        if fence:
            continue
        m = re.match(r'#{1,6}\s+(.*)', line)
        if m:
            s = slug(m.group(1))
            n = seen.get(s, 0)
            seen[s] = n + 1
            out.add(s if n == 0 else f'{s}-{n}')
    return out


# Example paths used in the text, not files of the repository.
EXAMPLES = {'hello/panpak.toml'}
bad = []
for md in mds:
    text = (ROOT / md).read_text()
    base = Path(md).parent
    fence = False
    for ln, line in enumerate(text.splitlines(), 1):
        if line.startswith('```'):
            fence = not fence
        if fence:
            continue
        for m in re.finditer(r'(?<!\!)\[[^\]]*\]\(([^)\s]+)\)|!\[[^\]]*\]\(([^)\s]+)\)|(?:src|srcset)="([^"]+)"', line):
            target = m.group(1) or m.group(2) or m.group(3)
            if re.match(r'[a-z][a-z0-9+.-]*:', target):
                continue
            path, _, frag = target.partition('#')
            dest = (base / path).as_posix() if path else md
            dest = str(Path(dest.replace('//', '/')))
            parts = []
            for p in dest.split('/'):
                if p == '..':
                    if parts:
                        parts.pop()
                elif p != '.':
                    parts.append(p)
            dest = '/'.join(parts)
            if dest not in tracked and not (ROOT / dest).is_dir():
                bad.append(f'{md}:{ln}: link to {target}: no such file')
            elif frag and dest.endswith('.md') and frag not in anchors(dest):
                bad.append(f'{md}:{ln}: link to {target}: no such heading')
        for m in re.finditer(r'`([A-Za-z0-9_./-]+/[A-Za-z0-9_.-]+\.(?:md|py|rs|sh|toml|yml|svg|csv))`', line):
            t = m.group(1)
            if t not in EXAMPLES and t not in tracked and (base / t).as_posix() not in tracked:
                bad.append(f'{md}:{ln}: `{t}` is not a tracked file')
print('\n'.join(bad) if bad else 'links ok')
sys.exit(1 if bad else 0)
