#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The helpers of tests/isolation.py: a work folder that holds read-only trees (the store makes its trees read-only) is removed whole, a link in it is
not followed, and nothing outside the work folder is removed or changed.

Usage: python3 tests/isolation_test.py
"""
import os
import stat
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import isolation  # noqa: E402


def main():
    base = Path(tempfile.mkdtemp(prefix='isolation-test-'))
    try:
        outside = base / 'outside'
        (outside / 'keep').mkdir(parents=True)
        (outside / 'keep/file').write_text('precious')
        os.chmod(outside / 'keep', 0o555)
        work = base / 'work'
        tree = work / 'home/.local/share/panpak/trees/abc'
        (tree / 'bin').mkdir(parents=True)
        (tree / 'bin/app').write_text('x')
        os.chmod(tree / 'bin/app', 0o555)
        os.chmod(tree / 'bin', 0o555)
        os.chmod(tree, 0o555)
        os.symlink(outside / 'keep', work / 'home/link-to-outside')

        # 1. A work folder with read-only trees is removed whole: a second run of a test does not meet what the first left.
        isolation.safe_rmtree(work, base)
        assert not work.exists(), f'left behind: {sorted(str(p) for p in work.rglob("*"))}'
        # 2. The link was not followed: what it pointed to is there and was not given other rights.
        assert (outside / 'keep/file').read_text() == 'precious'
        assert stat.S_IMODE(os.stat(outside / 'keep').st_mode) == 0o555, 'a folder outside the work folder was changed'
        # 3. Outside the work folder nothing is removed.
        try:
            isolation.safe_rmtree(outside, work)
        except SystemExit:
            pass
        else:
            raise AssertionError('a folder outside the work folder was removed')
        assert (outside / 'keep/file').exists()
        print('safe_rmtree: read-only trees are removed, links are not followed, outside is refused: ok')
    finally:
        for root, dirs, _ in os.walk(base):
            for d in dirs:
                p = os.path.join(root, d)
                if not os.path.islink(p):
                    os.chmod(p, 0o755)
        import shutil
        shutil.rmtree(base, ignore_errors=True)
    print('all ok')


if __name__ == '__main__':
    main()
