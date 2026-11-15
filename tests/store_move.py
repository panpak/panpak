#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A store that is copied to another path still works (PANPAK-FORMAT.md 6.1), with real binaries.

The links that the store writes (`apps/<id>/current`, `previous`, `pending`, the links of a farm, `trees/<root>/.farm`, `bin/panpak-stub`) are relative to
their own folder. Checked: after an install, an update (a `previous`) and a staged update (a `pending`) no link of the store names the path it was made in; the
whole home is copied to another path; there, `panpak list`, `panpak doctor` and a start work and read only the copy (the original is not touched);
`apply` and `rollback` work in the copy; an older store with an absolute `current` still starts.
"""
import os
from pathlib import Path
import random
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
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def pack(tmp, env, version, changed=()):
    tree = tmp / f'tree-{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(11)
    for i in range(12):
        d = bytearray(rng.randbytes(160_000))
        if i in changed:
            d[50:60] = f'v{version}-{i}'.encode().ljust(10, b'.')[:10]
        (tree / f'payload{i}').write_bytes(bytes(d))
    out = tmp / f'packed-{version}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes()


def env_for(home):
    runtime = home / 'run'
    runtime.mkdir(mode=0o700, exist_ok=True)
    env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / 'data'), XDG_CACHE_HOME=str(home / 'cache'), XDG_CONFIG_HOME=str(home / 'config'),
               XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
    env.pop('WAYLAND_DISPLAY', None)
    return env


def links_of(store):
    """Every link that the store writes itself (not the links inside the trees, which belong to the apps)."""
    out = []
    for sub in ('apps', 'farms', 'bin', 'packages', 'runtimes'):
        for p in sorted((store / sub).rglob('*')) if (store / sub).exists() else []:
            if p.is_symlink() and 'trees' not in p.parts[len(store.parts):]:
                out.append(p)
    for t in sorted((store / 'trees').iterdir()):
        if (t / '.farm').is_symlink():
            out.append(t / '.farm')
    return out


def listing(store):
    out = {}
    for sub in ('apps', 'packages', 'trees', 'farms', 'bin', 'runtimes', 'abi-cache', 'locks'):
        base = store / sub
        for p in sorted(base.rglob('*')) if base.exists() else []:
            rel = str(p.relative_to(store))
            out[rel] = os.readlink(p) if p.is_symlink() else (p.stat().st_size if p.is_file() else 'dir')
    return out


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-move-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        home_a, home_b = tmp / 'one', tmp / 'two' / 'elsewhere'
        home_a.mkdir()
        env_a = env_for(home_a)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        v = {n: resign(pack(tmp, env_a, n, ch), f'1.{i}', port) for i, (n, ch) in enumerate((('1', ()), ('2', (2, 8)), ('3', (2, 8, 5))))}
        (tmp / 'v1.panpak').write_bytes(v['1'])
        serve = tmp / 'serve'
        serve.mkdir()
        panpak = BIN / 'panpak'
        srv = subprocess.Popen([sys.executable, str(ROOT / 'tests/range_server.py'), str(serve), str(port), 'normal'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.6)
            store_a = home_a / 'data/panpak'
            run([panpak, 'install', '--yes', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak'], env_a)
            (serve / 'remote.panpak').write_bytes(v['2'])
            run([panpak, 'update', '--allow-insecure-http', '--timeout', '5', ID], env_a)
            (serve / 'remote.panpak').write_bytes(v['3'])
            run([panpak, 'update', '--stage', '--allow-insecure-http', '--timeout', '5', ID], env_a)
            appdir_a = store_a / 'apps' / ID
            for name in ('current', 'previous', 'pending'):
                assert (appdir_a / name).is_symlink(), f'{name} exists'

            # 1. no link of the store names the path that it was made in
            links = links_of(store_a)
            assert any(p.name == 'current' for p in links) and any(p.name == 'pending' for p in links) and any(p.name == '.farm' for p in links), links
            bad = [f'{p.relative_to(store_a)} -> {os.readlink(p)}' for p in links if os.readlink(p).startswith('/')]
            assert not bad, f'links with an absolute path: {bad}'
            assert not [p for p in links if str(home_a) in os.readlink(p)]
            print(f'{len(links)} links of the store (current, previous, pending, .farm, the runtime link, ...), none with an absolute path PASS')

            # 2. the home is copied to another path
            shutil.copytree(home_a, home_b, symlinks=True)
            env_b = env_for(home_b)
            store_b = home_b / 'data/panpak'
            before = listing(store_a)
            assert str(home_a) not in run([panpak, 'list'], env_b).stdout
            out = run([panpak, 'doctor', ID], env_b).stdout
            assert 'OK   verified package' in out and str(home_a) not in out, out
            p = run([store_b / 'bin/panpak-stub', 'run', store_b / 'apps' / ID / 'current'], env_b)
            # The first start applies the staged update (nothing runs), as it does in the original: it is the third version.
            assert p.stdout.strip().endswith('version 3'), p.stdout + p.stderr
            assert listing(store_a) == before, 'the copy was started and the original store did not change'
            print('the copy at another path: list, doctor and a start work (the staged update is applied there), and the original is not touched PASS')

            # 3. rolled back there
            run([panpak, 'rollback', ID], env_b)
            assert run([store_b / 'bin/panpak-stub', 'run', store_b / 'apps' / ID / 'current'], env_b).stdout.strip() == 'version 2'
            assert not [p for p in links_of(store_b) if os.readlink(p).startswith('/')], 'the links that the commands write are relative too'
            assert listing(store_a) == before, 'apply and rollback in the copy did not change the original store'
            assert run([store_a / 'bin/panpak-stub', 'run', store_a / 'apps' / ID / 'current'], env_a).stdout.strip().endswith('version 3'), 'the original still starts, from its own place'
            print('rollback in the copy works and writes relative links; the original still starts PASS')

            # 4. an older store, with an absolute current, still starts and is read
            cur = store_b / 'apps' / ID / 'current'
            target = (cur.parent / os.readlink(cur)).resolve()
            cur.unlink()
            cur.symlink_to(target)
            assert os.readlink(cur).startswith('/')
            assert run([store_b / 'bin/panpak-stub', 'run', cur], env_b).stdout.strip() == 'version 2'
            run([panpak, 'doctor', ID], env_b)
            run([panpak, 'rollback', ID], env_b)
            assert not os.readlink(cur).startswith('/'), 'the first command that moves current writes it relative'
            print('a store with an absolute current (older) starts, is read, and is made relative by the next rollback PASS')
        finally:
            srv.terminate()
            srv.wait()
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
