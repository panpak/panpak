#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""One copy of an app in the store: install makes the app ready (tree, farm, cache of the resolution) and keeps a record, not the package;
the record is never a package; the files of an installed version are checked by `doctor` and made right by `repair`; `update` builds the new
tree from the old files and the missing chunks; `rollback` and `remove` follow. Real binaries (build the release first), an isolated store,
and a local server with Range support for the update URL.
"""
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC, resign  # noqa: E402

ID = 'org.example.incremental'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok is None:
        pass
    elif ok:
        assert p.returncode == 0, p.stdout + p.stderr
    else:
        assert p.returncode != 0, p.stdout + p.stderr
    return p


def pack(tmp, env, version):
    tree = tmp / f'tree{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    for i in range(10):
        # Distinct files; version 2 changes two of them.
        n = 50 + i if (version == '2' and i in (2, 8)) else i
        (tree / f'payload{i}').write_bytes(bytes([n]) * 180000)
    out = tmp / f'packed{version}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes()


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-storeflow-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), PANPAK_ABI_TRACE='1', LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        v1 = resign(pack(tmp, env, '1'), '1.0', port)
        v2 = resign(pack(tmp, env, '2'), '1.1', port)
        (tmp / 'v1.panpak').write_bytes(v1)
        serve = tmp / 'serve'
        serve.mkdir()
        (serve / 'remote.panpak').write_bytes(v1)
        store = tmp / 'data/panpak'
        stub = store / 'bin/panpak-stub'

        # 1. install: ready, one copy.
        p = run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak'], env)
        records = sorted((store / 'packages').iterdir())
        assert len(records) == 1 and records[0].name.endswith('.rec'), records
        assert not list((store / 'packages').glob('*.panpak')), 'the store keeps no copy of the package'
        root1 = v1[-32:].hex()
        tree1 = store / 'trees' / root1
        assert (tree1 / '.complete').exists() and (store / 'farms' / root1).is_dir(), 'tree and farm made by install'
        assert any((store / 'abi-cache').iterdir()), 'the resolution is cached by install'
        assert (store / 'apps' / ID / 'current').resolve() == records[0].resolve()
        # The first start finds everything made: a hit, and the app runs from the tree.
        out = run([stub, 'run', store / 'apps' / ID / 'current'], env)
        assert out.stdout.strip() == 'version 1', out.stdout
        assert 'ABI cache hit' in out.stderr and 'ABI cache miss' not in out.stderr, out.stderr
        print('install: record, tree, farm, cache; the first start is a hit PASS')

        # 2. the record is never a package.
        e = run([BIN / 'panpak', 'install', '--yes', records[0]], env, ok=False)
        assert 'PANPAK-E0010' in e.stderr and 'install the .panpak package' in e.stderr, e.stderr
        elsewhere = tmp / 'copy.rec'
        shutil.copy(records[0], elsewhere)
        e = run([stub, 'run', elsewhere], env, ok=False)
        assert 'PANPAK-E0010' in e.stderr, e.stderr
        e = run([BIN / 'panpak', 'inspect', records[0]], env)  # reading what is in it is allowed
        assert 'Test 1.0' in e.stdout, e.stdout
        e = run([BIN / 'panpak', 'licenses', records[0]], env, ok=False)
        assert 'PANPAK-E0010' in e.stderr, e.stderr
        print('a record is refused as a package, and runs only from the store PASS')

        # 3. the files of the app are missing: a start says what to do.
        moved = tree1.with_name('moved-away')
        tree1.rename(moved)
        e = run([stub, 'run', store / 'apps' / ID / 'current'], env, ok=False)
        assert 'PANPAK-E2007' in e.stderr and 'panpak repair' in e.stderr, e.stderr
        moved.rename(tree1)
        run([stub, 'run', store / 'apps' / ID / 'current'], env)
        print('a start without the files says PANPAK-E2007 and `repair` PASS')

        # 4. doctor and repair.
        d = run([BIN / 'panpak', 'doctor', ID], env, ok=None)  # host checks may fail on a bare machine; only the verified-package line matters
        assert 'verified package: org.example.incremental 1.0' in d.stdout, d.stdout
        victim = tree1 / 'payload4'
        original = victim.read_bytes()
        victim.chmod(0o644)
        victim.write_bytes(bytes([original[0] ^ 255]) + original[1:])
        victim.chmod(0o444)
        d = run([BIN / 'panpak', 'doctor', ID], env, ok=False)
        assert 'PANPAK-E2007' in d.stdout and 'panpak repair' in d.stdout, d.stdout
        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', serve, str(port), 'normal'], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.4)
            r = run([BIN / 'panpak', 'repair', '--allow-insecure-http', '--timeout', '5', ID], env)
            assert 'repaired' in r.stdout and 'fetched' in r.stdout, r.stdout
            assert (tree1 / 'payload4').read_bytes() == original
            assert not list((store / 'trees').glob('*.damaged*')), 'the damaged tree is gone'
            r = run([BIN / 'panpak', 'repair', '--allow-insecure-http', ID], env)
            assert 'nothing to repair' in r.stdout, r.stdout
            run([stub, 'run', store / 'apps' / ID / 'current'], env)
            # From a package file; one of another version is refused.
            victim = tree1 / 'payload6'
            victim.chmod(0o644)
            victim.write_bytes(b'x' * 10)
            victim.chmod(0o444)
            (tmp / 'v2.panpak').write_bytes(v2)
            r = run([BIN / 'panpak', 'repair', '--from', tmp / 'v2.panpak', ID], env, ok=False)
            assert 'not the installed version' in r.stderr, r.stderr
            r = run([BIN / 'panpak', 'repair', '--from', tmp / 'v1.panpak', ID], env)
            assert 'repaired' in r.stdout and (tree1 / 'payload6').read_bytes() == bytes([6]) * 180000, r.stdout
            # The server now serves another version: the repair refuses, and says what to run instead.
            (serve / 'remote.panpak').write_bytes(v2)
            victim = tree1 / 'payload6'
            victim.chmod(0o644)
            victim.write_bytes(b'y' * 10)
            victim.chmod(0o444)
            r = run([BIN / 'panpak', 'repair', '--allow-insecure-http', ID], env, ok=False)
            assert 'serves another version' in r.stderr and 'panpak update' in r.stderr, r.stderr
            run([BIN / 'panpak', 'repair', '--from', tmp / 'v1.panpak', ID], env)
            print('doctor finds a damaged file; repair makes it right from the URL and from a package PASS')

            # 5. update, rollback.
            before = (store / 'apps' / ID / 'current').readlink()
            r = run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', ID], env)
            assert 'updated' in r.stdout and '1.0 -> 1.1' in r.stdout, r.stdout
            after = (store / 'apps' / ID / 'current').readlink()
            assert after != before and (store / 'apps' / ID / 'previous').readlink() == before
            root2 = v2[-32:].hex()
            assert (store / 'trees' / root2 / '.complete').exists() and tree1.exists(), 'both trees are there'
            assert (store / 'trees' / root2 / 'payload2').read_bytes() == bytes([52]) * 180000
            assert len(list((store / 'packages').iterdir())) == 2 and not list((store / 'staging').glob('*/*')) if (store / 'staging').exists() else True
            out = run([stub, 'run', store / 'apps' / ID / 'current'], env)
            assert out.stdout.strip() == 'version 2' and 'ABI cache hit' in out.stderr, out.stdout + out.stderr
            # The files that the two versions share are the same inodes: one copy.
            assert (store / 'trees' / root2 / 'payload0').stat().st_ino == (tree1 / 'payload0').stat().st_ino
            # One command at a time changes an app: while another holds the lock of the app, a command waits (and changes nothing), then goes on.
            import fcntl
            (store / 'locks').mkdir(exist_ok=True)
            with open(store / 'locks' / f'{ID}.lock', 'w') as held:
                fcntl.flock(held, fcntl.LOCK_EX)
                before_link = (store / 'apps' / ID / 'current').readlink()
                waiting = subprocess.Popen([str(BIN / 'panpak'), 'rollback', ID], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                time.sleep(1.0)
                assert waiting.poll() is None, 'the command did not wait for the lock of the app'
                assert (store / 'apps' / ID / 'current').readlink() == before_link
            out, err = waiting.communicate(timeout=30)
            assert waiting.returncode == 0, out + err
            assert (store / 'apps' / ID / 'current').readlink() != before_link
            assert run([stub, 'run', store / 'apps' / ID / 'current'], env).stdout.strip() == 'version 1'
            run([BIN / 'panpak', 'rollback', ID], env)
            assert run([stub, 'run', store / 'apps' / ID / 'current'], env).stdout.strip() == 'version 2'
            # The previous version's files are gone: no rollback.
            gone = (store / 'trees' / root1).with_name('away')
            (store / 'trees' / root1).rename(gone)
            e = run([BIN / 'panpak', 'rollback', ID], env, ok=False)
            assert 'PANPAK-E2007' in e.stderr, e.stderr
            gone.rename(store / 'trees' / root1)
            print('update builds the tree from the old files and the missing chunks; rollback follows PASS')
        finally:
            server.terminate()
            server.wait(timeout=5)

        # 6. remove takes it all.
        run([BIN / 'panpak', 'remove', '--yes', ID], env)
        assert not list((store / 'packages').iterdir()) and not list((store / 'trees').iterdir()), (list((store / 'packages').iterdir()), list((store / 'trees').iterdir()))
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)
        print('remove: no record, no tree, no lock file PASS')


if __name__ == '__main__':
    main()
