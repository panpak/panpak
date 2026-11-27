#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The levels of `scripts/ci-local.sh`: a documents-only change gets the `docs` level, a change of code the `full` one, the flags win, and a round that touches the build is refused.

Usage: python3 tests/ci_local_levels.py
It makes a throwaway git repository in a temporary folder with copies of `scripts/ci-local.sh` and `scripts/ci-changes.sh`, and runs `ci-local.sh --print-level` there: nothing is built.
"""
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def need(cond, msg):
    if not cond:
        sys.exit('FAIL: ' + msg)


def git(repo, *args):
    env = dict(os.environ, GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@example.org', GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@example.org')
    return subprocess.run(['git', '-C', str(repo), *args], env=env, check=True, capture_output=True, text=True).stdout


def level(repo, *flags):
    p = subprocess.run([str(repo / 'scripts/ci-local.sh'), '--print-level', *flags], capture_output=True, text=True, cwd=repo)
    return p.returncode, (p.stdout + p.stderr)


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-ci-levels-') as tmp:
        repo = Path(tmp)
        git(repo, 'init', '-q', '-b', 'main')
        (repo / 'scripts').mkdir()
        for f in ('ci-local.sh', 'ci-changes.sh'):
            shutil.copy(ROOT / 'scripts' / f, repo / 'scripts' / f)
        (repo / 'README.md').write_text('x\n')
        git(repo, 'add', '-A')
        git(repo, 'commit', '-q', '-m', 'base')
        git(repo, 'checkout', '-q', '-b', 'docs-only')
        (repo / 'docs').mkdir()
        (repo / 'docs/a.md').write_text('a\n')
        git(repo, 'add', '-A')
        git(repo, 'commit', '-q', '-m', 'docs')
        rc, out = level(repo)
        need(rc == 0 and 'level docs' in out, f'a documents-only change is not level docs: {out}')
        rc, out = level(repo, '--full')
        need(rc == 0 and 'level full (--full)' in out, f'--full does not win: {out}')
        rc, out = level(repo, '--quick')
        need(rc == 0 and 'level quick' in out, out)
        # A hashes file alone is documentation for the classification, and the double build is then asked for (repro=true): the level stays docs.
        (repo / 'REPRODUCIBLE-HASHES.md').write_text('h\n')
        git(repo, 'add', '-A')
        git(repo, 'commit', '-q', '-m', 'hashes')
        rc, out = level(repo)
        need(rc == 0 and 'level docs' in out, out)
        # Code makes it full.
        git(repo, 'checkout', '-q', '-b', 'code', 'main')
        (repo / 'panpak-core').mkdir()
        (repo / 'panpak-core/lib.rs').write_text('fn main() {}\n')
        git(repo, 'add', '-A')
        git(repo, 'commit', '-q', '-m', 'code')
        rc, out = level(repo)
        need(rc == 0 and 'level full' in out and 'code, tests or scripts changed' in out, out)
        # A round: the level is round; and it is refused when the last commit touches the build or the CI itself.
        rc, out = level(repo, '--round')
        need(rc == 0 and 'level round (--round=HEAD~1)' in out, out)
        (repo / 'scripts/other.sh').write_text('#!/bin/sh\n')
        git(repo, 'add', '-A')
        git(repo, 'commit', '-q', '-m', 'script')
        rc, out = level(repo, '--round')
        need(rc == 2 and 'Use --full' in out, f'a round that touches scripts/ must be refused (exit 2, with the advice), got {rc}: {out}')
        # A crate's own Cargo.toml and a build.rs are refused too, and a REF that looks like an option is not a commit.
        git(repo, 'reset', '-q', '--hard', 'HEAD~1')
        (repo / 'panpak-core/Cargo.toml').write_text('[package]\n')
        git(repo, 'add', '-A')
        git(repo, 'commit', '-q', '-m', 'crate manifest')
        rc, out = level(repo, '--round')
        need(rc == 2 and 'Use --full' in out, f'a round that touches a crate Cargo.toml must be refused: {rc} {out}')
        rc, out = level(repo, '--round=--output=x')
        need(rc == 2 and 'not a commit' in out and not (repo / 'x').exists(), f'an option as REF: {rc} {out}')
        # README.md is read by a cargo test: the level is full.
        git(repo, 'checkout', '-q', '-b', 'readme', 'main')
        (repo / 'README.md').write_text('y\n')
        git(repo, 'add', '-A')
        git(repo, 'commit', '-q', '-m', 'readme')
        rc, out = level(repo)
        need(rc == 0 and 'level full' in out and 'README.md' in out, out)
        # An unknown base is not a reason to skip anything: the level is full.
        rc, out = level(repo, 'no-such-branch')
        need(rc == 0 and 'level full' in out, out)
    print('the levels of ci-local.sh: ok')


if __name__ == '__main__':
    main()
