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
# Every workflow is valid YAML (an invalid file makes GitHub run nothing, and the pull request shows no check at all).
try:
    import yaml
except ImportError:
    print('skip every workflow parses as YAML: PyYAML is not installed')
else:
    for wf in sorted(path.parent.glob('*.yml')):
        try:
            yaml.safe_load(wf.read_text())
        except yaml.YAMLError as e:
            bad.append(f'{wf.name} is not valid YAML: {e}')
    if not any('not valid YAML' in b for b in bad):
        print('ok   every workflow is valid YAML')
# Every test of the repository runs in at least one level of the CI, or is listed in tests/ci-coverage.txt with its kind and its reason.
root = path.parent.parent.parent
# Comments do not count: a file named in a comment is not run.
workflows = '\n'.join(l for wf in sorted(path.parent.glob('*.yml')) for l in wf.read_text().splitlines() if not l.lstrip().startswith('#'))
listed = {}
for line in (root / 'tests/ci-coverage.txt').read_text().splitlines():
    if line.strip() and not line.startswith('#'):
        parts = [x.strip() for x in line.split('|')]
        if len(parts) != 3 or parts[1] not in ('helper', 'manual') or not parts[2]:
            bad.append(f'tests/ci-coverage.txt: bad line {line!r}')
        else:
            listed[parts[0]] = parts[1]
for f in sorted(list((root / 'tests').rglob('*.py')) + list((root / 'tests').rglob('*.sh'))):
    rel = str(f.relative_to(root))
    if '__pycache__' in rel or f.name == '__init__.py':
        continue
    in_ci = (rel in workflows) or (f.name in workflows and not f.name.startswith('test_')) or (f.parent.name == 'apps' and f.name.startswith('test_') and "-s tests/apps -p 'test_*.py'" in workflows)
    if in_ci and rel in listed:
        bad.append(f'{rel} is in a workflow and also listed in tests/ci-coverage.txt: remove the line')
    elif not in_ci and rel not in listed:
        bad.append(f'{rel} runs in no workflow and is not listed in tests/ci-coverage.txt (add it to a level, or list it with a reason)')
for rel in listed:
    if not (root / rel).exists():
        bad.append(f'tests/ci-coverage.txt names {rel}, which does not exist')
# A script that a step wraps in with_timeout.py must exist (the wrapper runs it by the interpreter), and every step has its own limit.
for m in re.finditer(r'with_timeout\.py (\d+) (tests/[\w/.-]+\.py)', workflows):
    if not (root / m.group(2)).is_file():
        bad.append(f'a step runs {m.group(2)} through with_timeout.py, which does not exist')
    if int(m.group(1)) < 60:
        bad.append(f'{m.group(2)}: a limit of {m.group(1)} s is too short')
steps = re.findall(r'^      - (?:name|uses): .*(?:\n(?!      - ).*)*', jobs_text, re.M)
for st in steps:
    if 'timeout-minutes:' not in st:
        bad.append(f'a step has no timeout-minutes: {st.splitlines()[0].strip()}')
# A test runs in a job that has built the programs it starts: a script that starts the debug build (`target/<triple>/debug`, the only one that takes a stand-in window tool) in a
# job that builds only the release fails there with "read stub: No such file", and passes on a machine that has both. The same for the release. The step that builds comes before it.
step_re = r'^      - (?:name|uses): .*(?:\n(?!      - ).*)*'
for jname, jb in ((m.group(1), b) for b in blocks for m in [re.match(r'\n?  ([A-Za-z0-9_-]+):\n', b)] if m):
    have_debug = have_release = False
    for st in re.findall(step_re, jb, re.M):
        for run_line in re.findall(r'cargo (?:build|test|clippy)[^\n]*', st):
            if '--target x86_64-unknown-linux-musl' in run_line and '--release' not in run_line and 'target-dir' not in run_line and 'clippy' not in run_line:
                have_debug = True
            if '--release' in run_line and '--target-dir' not in run_line:
                have_release = True
        for m in re.finditer(r'with_timeout\.py \d+ (tests/[\w/.-]+\.py)', st):
            f = root / m.group(1)
            if not f.is_file():
                continue
            src = f.read_text()
            if 'x86_64-unknown-linux-musl/debug' in src and not have_debug:
                bad.append(f'{m.group(1)} starts the debug build, and no step before it in the job {jname} builds it (build it there, or run the test in the job that does)')
            if re.search(r'from incremental import[^\n]*\bBIN\b', src) and 'x86_64-unknown-linux-musl/debug' not in src and not have_release:
                bad.append(f'{m.group(1)} starts the release build, and no step before it in the job {jname} builds it')
# A step that runs the fuzzer for FUZZ_SECONDS has a limit above them, with room for the build.
for b in blocks:
    if 'FUZZ_SECONDS' not in b:
        continue
    secs = [int(x) for x in re.findall(r"FUZZ_SECONDS: [^\n]*", b) for x in re.findall(r"'(\d+)'", x)]
    for st in re.findall(step_re, b, re.M):
        t = re.search(r'timeout-minutes: (\d+)', st)
        if 'fuzz run' in st and t and secs and int(t.group(1)) * 60 < max(secs) + 180:
            bad.append(f'the fuzz step has a limit of {t.group(1)} minutes and runs the fuzzer for {max(secs)} s: it needs at least {(max(secs) + 180 + 59) // 60} minutes with the build')
if not any('ci-coverage' in b or 'runs in no workflow' in b for b in bad):
    print(f'ok   every test runs in a level of the CI or is listed with a reason ({len(listed)} listed)')
for b in bad:
    print('FAIL', b)
if not bad:
    print('ok   every action is pinned to a full commit, every job has a time limit')
sys.exit(1 if bad else 0)
