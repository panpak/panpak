#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Static check of .github/workflows/ci.yml (stdlib only, no YAML parser).

An `if:` without a status function (!cancelled(), always(), success(), failure())
means success(), and success() is false when ANY job above it in the `needs` chain
was skipped, not only the direct ones. So a job that depends, however far, on a job
that can be skipped must say !cancelled() or always() in its `if:`, or it is
skipped together with it. The CI hit this: the heavy jobs depended on `changes`,
which depended on `monday`, which is skipped on every event but the schedule.

Usage: python3 tests/ci-workflow-test.py [workflow-file]
"""
import re
import sys
from pathlib import Path

path = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parents[1] / '.github/workflows/ci.yml')
text = path.read_text()
jobs_text = text.split('\njobs:\n', 1)[1]
blocks = re.split(r'\n(?=  [A-Za-z0-9_-]+:\n)', '\n' + jobs_text)
jobs = {}
for b in blocks:
    m = re.match(r'\n?  ([A-Za-z0-9_-]+):\n', b)
    if not m:
        continue
    head = b.split('\n    steps:', 1)[0]
    needs = re.search(r'^    needs: (.+)$', head, re.M)
    cond = re.search(r'^    if: (.+)$', head, re.M)
    jobs[m.group(1)] = {
        'needs': re.findall(r'[A-Za-z0-9_-]+', needs.group(1)) if needs else [],
        'if': cond.group(1) if cond else None,
    }

STATUS = re.compile(r'!cancelled\(\)|always\(\)|failure\(\)|success\(\)')
def has_status(c):
    return c is not None and STATUS.search(c) is not None

def can_be_skipped(name, seen=()):
    j = jobs[name]
    if j['if'] is not None:
        return True
    return any(can_be_skipped(n, seen + (name,)) for n in j['needs'] if n not in seen)

def ancestors(name):
    out = set()
    for n in jobs[name]['needs']:
        out |= {n} | ancestors(n)
    return out

bad = []
for name, j in jobs.items():
    above = ancestors(name)
    skippable = sorted(a for a in above if jobs[a]['if'] is not None)
    if skippable and not has_status(j['if']):
        bad.append(f"{name}: depends on {', '.join(skippable)} (can be skipped) but its if is {j['if']!r}: add !cancelled()")
    elif not skippable:
        print(f'ok   {name}: nothing above it can be skipped')
        continue
    else:
        print(f'ok   {name}: if has a status function ({", ".join(skippable)} above)')
if 'gate' not in jobs or not (jobs['gate']['if'] and 'always()' in jobs['gate']['if']):
    bad.append('gate must exist and run with always()')
# Every action is pinned to a full commit (40 hexadecimal digits) with its version in a comment: a tag can be moved, a commit cannot
# (GitHub's setting "Require actions to be pinned to a full-length commit SHA" refuses anything else). Every job has a time limit.
for wf in sorted(path.parent.glob('*.yml')):
    for n, line in enumerate(wf.read_text().splitlines(), 1):
        m = re.match(r'\s*(?:-\s*)?uses:\s*(\S+)\s*(#.*)?$', line)
        if not m:
            continue
        ref, comment = m.group(1), m.group(2)
        if ref.startswith('./') or ref.startswith('docker://'):
            continue
        if not re.fullmatch(r'[\w.-]+/[\w./-]+@[0-9a-f]{40}', ref):
            bad.append(f'{wf.name}:{n}: {ref} is not pinned to a full commit')
        elif not comment or not re.search(r'v?\d+(\.\d+)*', comment):
            bad.append(f'{wf.name}:{n}: {ref} has no version in a comment')
    for name, body in re.findall(r'\n  ([A-Za-z0-9_-]+):\n((?:    [^\n]*\n|\n)+)', wf.read_text().split('\njobs:\n', 1)[-1] if '\njobs:\n' in wf.read_text() else ''):
        head = body.split('\n    steps:', 1)[0]
        if 'timeout-minutes:' not in head and 'uses:' not in head.split('\n    steps:')[0]:
            bad.append(f'{wf.name}: job {name} has no timeout-minutes')
for b in bad:
    print('FAIL', b)
if not bad:
    print('ok   every action is pinned to a full commit, every job has a time limit')
sys.exit(1 if bad else 0)
