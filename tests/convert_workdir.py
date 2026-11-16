#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak convert` unpacks on the disk of the user, not in $TMPDIR, and counts before it writes, with real binaries.

The temporary folder is often in memory (tmpfs), and an AppImage that is large, or made to be, filled it and froze the machine. Now the tree is unpacked in a private folder of
the cache (`$XDG_CACHE_HOME/panpak/convert/`), a cache that is in memory is refused, and the index of the squashfs (the number of entries, the sizes of the files) is checked
before any file is made: an AppImage over the limits stops with nothing written, not even the work folder.

Checked: (1) with TMPDIR pointing at a folder that does not exist, convert works, and the work folder is empty afterwards; (2) a cache folder in memory is refused and nothing is
written there; (3) a sparse file of 9 GiB (a few kilobytes in the squashfs) is refused from the index, and the cache folder stays empty. `--no-big` leaves out (3): a binary
without the check would write 8 GiB before it stops.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN  # noqa: E402

def disk_dir():
    """A folder on disk for the work of the test: `convert` refuses a cache folder that is in memory, and /tmp is in memory on many machines."""
    d = ROOT / 'target' / 'test-work'
    d.mkdir(parents=True, exist_ok=True)
    return d


ID = 'org.example.workdir'


def run(args, env, ok=True, timeout=600):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def squash(src, out):
    run(['mksquashfs', src, out, '-noappend', '-no-progress', '-quiet', '-all-root'], os.environ)


def is_tmpfs(path):
    out = subprocess.run(['stat', '-f', '-c', '%T', str(path)], capture_output=True, text=True).stdout.strip()
    return out in ('tmpfs', 'ramfs')


def below(root):
    return sorted(str(p.relative_to(root)) for p in Path(root).rglob('*'))


def main(big=True):
    with tempfile.TemporaryDirectory(prefix='panpak-convert-workdir-', dir=disk_dir()) as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        (tmp / 'secret.key').write_text((bytes([44]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)

        def env_for(cache):
            return dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(cache), XDG_CONFIG_HOME=str(tmp / 'config'),
                        TMPDIR=str(tmp / 'no-such-tmp'), LC_ALL='C')

        def convert(src, cache, out):
            return run([BIN / 'panpak', 'convert', src, '--id', ID, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '--entry', 'usr/bin/app', '-o', out],
                       env_for(cache), ok=False)

        small = tmp / 'small-src'
        (small / 'usr/bin').mkdir(parents=True)
        (small / 'usr/bin/app').write_text('#!/bin/sh\necho hi\n')
        (small / 'usr/bin/app').chmod(0o755)
        squash(small, tmp / 'small.squashfs')

        # (1) TMPDIR is not used.
        cache = tmp / 'cache'
        cache.mkdir()
        p = convert(tmp / 'small.squashfs', cache, tmp / 'small.panpak')
        assert p.returncode == 0, f'convert needs the temporary folder (TMPDIR is a folder that does not exist):\n{p.stdout}{p.stderr}'
        assert not (tmp / 'no-such-tmp').exists()
        left = below(cache / 'panpak/convert')
        assert not left, f'the work folder was not emptied: {left}'
        print('  TMPDIR is not used: convert works with a TMPDIR that does not exist, and the work folder is empty afterwards')

        # (2) A cache in memory is refused.
        if Path('/dev/shm').is_dir() and is_tmpfs('/dev/shm'):
            with tempfile.TemporaryDirectory(prefix='panpak-convert-mem-', dir='/dev/shm') as mem:
                p = convert(tmp / 'small.squashfs', mem, tmp / 'mem.panpak')
                assert p.returncode != 0 and 'memory' in (p.stdout + p.stderr), f'a cache folder in memory was used:\n{p.stdout}{p.stderr}'
                assert not below(mem), f'something was written in memory: {below(mem)}'
                assert not (tmp / 'mem.panpak').exists()
            print('  a cache folder in memory is refused, nothing is written there')
        else:
            print('  (no tmpfs at /dev/shm here: the cache-in-memory case is not run)')

        # (3) The index is counted before anything is written.
        if big:
            huge = tmp / 'huge-src'
            (huge / 'usr/bin').mkdir(parents=True)
            (huge / 'usr/bin/app').write_text('#!/bin/sh\necho hi\n')
            (huge / 'usr/bin/app').chmod(0o755)
            with open(huge / 'usr/share-big', 'wb') as f:
                f.truncate(9 << 30)
            squash(huge, tmp / 'huge.squashfs')
            assert (tmp / 'huge.squashfs').stat().st_size < 64 << 20, 'the squashfs of the sparse file is not small'
            cache2 = tmp / 'cache2'
            cache2.mkdir()
            p = convert(tmp / 'huge.squashfs', cache2, tmp / 'huge.panpak')
            said = p.stdout + p.stderr
            assert p.returncode != 0 and 'more than' in said, f'an AppImage of 9 GiB was not refused from its index:\n{said}'
            assert not below(cache2), f'something was written before the refusal: {below(cache2)}'
            print('  a file of 9 GiB: refused from the index, nothing written, not even the work folder')
        print('convert work folder PASS')


if __name__ == '__main__':
    main(big='--no-big' not in sys.argv)
