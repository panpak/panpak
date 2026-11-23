#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Patches (PANPAK-FORMAT.md §4.1) with real binaries: `pack --previous` writes `<package>.patches`, `update` downloads the patch of a file instead of its chunks,
and a patches file that is missing, damaged or of another release leaves the update to the chunks.

Usage: python3 tests/patches.py   (build the release binaries first; an isolated HOME and store)
"""
import json
import os
from pathlib import Path
import random
import socket
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC  # noqa: E402

ID = 'org.example.patched'


def run(args, env, ok=True, timeout=120):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    else:
        assert p.returncode != 0, p.stdout
    return p


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-patches-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        (tmp / 'run').mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_RUNTIME_DIR=str(tmp / 'run'), PANPAK_NO_BACKGROUND='1')
        env['PANPAK_RANGE_LOG'] = str(tmp / 'ranges.jsonl')
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        rnd = random.Random(7)
        big = bytearray(rnd.randbytes(10 << 20))
        small = rnd.randbytes(200_000)
        lib = bytearray(rnd.randbytes(2 << 20))
        tree = tmp / 'tree'
        tree.mkdir()
        (tree / 'hello').write_text('#!/bin/sh\nexit 0\n')
        (tree / 'hello').chmod(0o755)

        def write_tree(version):
            (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="P"\nversion="{version}"\nentry="hello"\n'
                                              f'[update]\nkey="ed25519:{PUBLIC}"\nurl="http://127.0.0.1:{port}/remote.panpak"\n')

        write_tree('1.0')
        (tree / 'big').write_bytes(big)
        (tree / 'small').write_bytes(small)
        (tree / 'libx.so.1').write_bytes(lib)
        pack = [BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key']
        run([*pack, '-o', tmp / 'old.panpak'], env)
        assert not (tmp / 'old.panpak.patches').exists(), 'a first release has no patches'
        # The new version changes one byte in every 16 KiB of `big`: no chunk of it is the same, and the patch is tiny.
        for i in range(0, len(big), 16 << 10):
            big[i] ^= 0x5A
        for i in range(0, len(lib), 16 << 10):
            lib[i] ^= 0x33
        write_tree('1.1')
        (tree / 'big').write_bytes(big)
        # A library that the new version renames: the patch starts from the old name.
        (tree / 'libx.so.1').unlink()
        (tree / 'libx.so.2').write_bytes(lib)
        out = run([*pack, '--previous', tmp / 'old.panpak', '-o', tmp / 'remote.panpak'], env).stdout
        patches = tmp / 'remote.panpak.patches'
        assert patches.exists() and 'patches from the earlier release' in out, out
        assert 0 < patches.stat().st_size < 100_000, patches.stat().st_size
        original = patches.read_bytes()
        store = tmp / 'data/panpak'
        new_root = (tmp / 'remote.panpak').read_bytes()[-32:].hex()

        def update_with(patches_bytes):
            """A fresh install of the old version, an update from the server, and what came over the network."""
            if store.exists():
                subprocess.run([BIN / 'panpak', 'remove', '--yes', ID], env=env, capture_output=True)
                isolation.safe_rmtree(store, tmp)
            if patches_bytes is None:
                patches.unlink(missing_ok=True)
            else:
                patches.write_bytes(patches_bytes)
            run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', tmp / 'old.panpak'], env)
            log = tmp / 'ranges.jsonl'
            log.unlink(missing_ok=True)
            server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', tmp, str(port)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                time.sleep(0.3)
                p = run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', ID], env)
            finally:
                server.terminate()
                server.wait(timeout=5)
            rows = [json.loads(l) for l in log.read_text().splitlines()]
            got = (store / 'trees' / new_root / 'big').read_bytes()
            assert got == bytes(big), 'the file is not the new one'
            assert (store / 'trees' / new_root / 'small').read_bytes() == small
            assert (store / 'trees' / new_root / 'libx.so.2').read_bytes() == bytes(lib)
            return sum(r['bytes'] for r in rows), [r['path'] for r in rows], p

        with_patch, paths, p = update_with(original)
        assert any(x.endswith('.patches') for x in paths), paths
        # The rest of the update is the footer, the manifest, the signature, the index: no chunk of `big` is downloaded.
        assert with_patch < 200_000, with_patch
        print(f'patch: {with_patch} bytes downloaded')

        without, paths, p = update_with(None)
        assert without > 8_000_000, f'without a patches file the chunks of the file come: {without}'
        print(f'no patches file (404): {without} bytes, the update works from the chunks')

        damaged = bytearray(original)
        damaged[len(damaged) // 2] ^= 1
        bad, paths, p = update_with(bytes(damaged))
        assert bad > 8_000_000, f'a damaged patch is not used and the chunks come: {bad}'
        print(f'damaged patches file: {bad} bytes, the update works from the chunks')

        short, paths, p = update_with(original[:len(original) // 2])
        assert short > 8_000_000, f'a short patches file is not used: {short}'
        print('short patches file: PASS')
        print('patches PASS')
        isolation.safe_rmtree(tmp / 'data', tmp)


if __name__ == '__main__':
    main()
