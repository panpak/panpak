#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A blob is visible in `blobs/` only when it is on disk (PANPAK-FORMAT.md 6.2), shown without any hook in the program: `strace` sends SIGKILL to
`panpak-stub prepare` at the first rename that would give a blob its name, and a trace of a whole run shows the order of the system calls.

1. Killed between the writing of the blobs and their naming: `blobs/` holds no blob (they are in the work folder), no tree is published, and the
   next run makes everything and removes the work folder that the dead process left.
2. In a whole run, every blob is synced before the first rename into `blobs/`, and `blobs/` is synced after the last.
3. A second package that shares most of its files with the first: its blobs that are in `blobs/` are linked and not synced again (the number of
   `fsync` calls is a fraction of the first install's), and its tree is whole.
Needs `strace` (the test says so and ends when it is missing). Real binaries: build the release first.
"""
import os
from pathlib import Path
import random
import re
import shutil
import subprocess
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC  # noqa: E402

FILES = 80


def run(args, env, ok=True, timeout=120):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def pack(tmp, env, name, changed):
    tree = tmp / name
    tree.mkdir()
    (tree / 'hello').write_text('#!/bin/sh\necho hi\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="org.example.blobs"\nname="Test"\nversion="1"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(9)
    for i in range(FILES):
        data = bytearray(rng.randbytes(90_000))
        if i in changed:
            data[10:20] = b'changed...'
        (tree / f'payload{i}').write_bytes(bytes(data))
    out = tmp / f'{name}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return tree, out


def trace(env, store, package, log, kill_at_first_rename):
    args = ['strace', '-f', '-qq', '-o', log, '-e', 'trace=fsync,fdatasync,rename,renameat,renameat2']
    if kill_at_first_rename:
        args += ['-e', 'inject=rename,renameat,renameat2:signal=KILL:when=1']
    return subprocess.run(args + [str(BIN / 'panpak-stub'), 'prepare', str(package)], env=env, capture_output=True, text=True, timeout=120)


def visible(store):
    blobs = store / 'blobs'
    return sorted(p.name for p in blobs.iterdir() if p.is_file()) if blobs.exists() else []


def work_dirs(store):
    blobs = store / 'blobs'
    return sorted(p for p in blobs.iterdir() if p.is_dir() and p.name.startswith('.work-')) if blobs.exists() else []


def main():
    if not shutil.which('strace'):
        print('strace is not installed: the test needs it to stop the program between two system calls (SKIP)')
        return
    with tempfile.TemporaryDirectory(prefix='panpak-blobvis-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        store = tmp / 'data/panpak'
        tree1, pkg1 = pack(tmp, env, 'one', ())
        tree2, pkg2 = pack(tmp, env, 'two', (3, 40))

        # 1. killed at the first rename that would name a blob.
        p = trace(env, store, pkg1, tmp / 'killed.log', True)
        assert p.returncode in (137, -9, 9), (p.returncode, p.stderr)
        assert visible(store) == [], f'a blob is visible in blobs/ although the start died before naming any: {visible(store)[:3]}'
        work = work_dirs(store)
        n_work = len(list(work[0].iterdir())) if len(work) == 1 else 0
        assert n_work >= FILES, 'the blobs are in the work folder of the dead start'
        assert not [t for t in (store / 'trees').iterdir() if t.is_dir() and not t.name.endswith('.partial')], 'no tree was published'
        # The next run finishes, takes away the work folder of the dead process, and the tree is what the package holds.
        p = trace(env, store, pkg1, tmp / 'whole.log', False)
        assert p.returncode == 0, p.stderr
        assert work_dirs(store) == [], 'the work folder of the dead start is removed by the next one'
        root1 = pkg1.read_bytes()[-32:].hex()
        for f in tree1.iterdir():
            if f.name != 'panpak.toml':
                assert (store / 'trees' / root1 / f.name).read_bytes() == f.read_bytes(), f.name
        print(f'killed before the naming: no blob visible ({n_work} in the work folder), the next run is whole PASS')

        # 2. the order in a whole run: every blob synced before the first one is named, blobs/ synced after the last.
        lines = (tmp / 'whole.log').read_text().splitlines()
        first_name = next(i for i, l in enumerate(lines) if re.search(r'rename\w*\(.*/blobs/\.work-.*, ".*/blobs/[0-9a-f]{64}-\d+"', l))
        last_name = max(i for i, l in enumerate(lines) if re.search(r'rename\w*\(.*/blobs/\.work-.*, ".*/blobs/[0-9a-f]{64}-\d+"', l))
        synced_before = sum(1 for l in lines[:first_name] if re.search(r'\bfsync\(', l))
        unique_blobs = len(visible(store))
        assert synced_before >= unique_blobs, f'{synced_before} fsync calls before the first blob is named, {unique_blobs} blobs'
        after = sum(1 for l in lines[last_name + 1:] if re.search(r'\bfsync\(', l))
        assert after >= 1, 'blobs/ is synced after the last blob is named'
        print(f'order: {synced_before} fsync calls before the first of {unique_blobs} blobs is named, and blobs/ is synced after the last PASS')

        # 3. a second package that shares most of its files: the blobs that are there are only linked.
        p = trace(env, store, pkg2, tmp / 'second.log', False)
        assert p.returncode == 0, p.stderr
        second = sum(1 for l in (tmp / 'second.log').read_text().splitlines() if re.search(r'\bfsync\(', l))
        first = sum(1 for l in lines if re.search(r'\bfsync\(', l))
        assert second * 4 < first, f'the second install made {second} fsync calls, the first {first}'
        root2 = pkg2.read_bytes()[-32:].hex()
        for f in tree2.iterdir():
            if f.name != 'panpak.toml':
                assert (store / 'trees' / root2 / f.name).read_bytes() == f.read_bytes(), f.name
        assert (store / 'trees' / root2 / 'payload0').stat().st_ino == (store / 'trees' / root1 / 'payload0').stat().st_ino, 'a shared file is one inode'
        print(f'a second package that shares its files: {second} fsync calls against {first}, its tree whole PASS')
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
