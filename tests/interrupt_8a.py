#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`install` and `update` stopped by SIGKILL at many moments (a sweep of delays, then random ones): the app is never left on something that is not
whole. After each stop, `current` is absent (not installed) or names a record whose tree is complete and whose app starts; the same command run
again finishes; the app then runs the new version. The store is made durable in an order (PANPAK-FORMAT.md section 6.2) so that this holds also for
a power loss; a kill does not test that, only the order of the steps. Real binaries (build the release first).
"""
import os
from pathlib import Path
import random
import signal
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
FILES = 60


def run(args, env, ok=True, timeout=120):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def pack(tmp, env, version, changed):
    tree = tmp / f'tree{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(5)
    for i in range(FILES):
        data = bytearray(rng.randbytes(150_000))
        if version == '2' and i in changed:
            data[1000:1100] = b'2' * 100
        (tree / f'payload{i}').write_bytes(bytes(data))
    out = tmp / f'packed{version}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes()


def killed(args, env, delay):
    """Run a command and kill its whole process group after `delay` seconds (or let it end first)."""
    p = subprocess.Popen([str(x) for x in args], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        p.wait(timeout=delay)
        return True
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()
        return False


def invariant(store, env, allowed):
    """After a stop: not installed, or whole and running one of the `allowed` versions."""
    cur = store / 'apps' / ID / 'current'
    if not cur.is_symlink():
        return None
    rec = cur.resolve()
    assert rec.parent == (store / 'packages').resolve() and rec.name.endswith('.rec'), f'current -> {rec}'
    root = rec.read_bytes()[-32:].hex()
    assert (store / 'trees' / root / '.complete').exists(), f'current names a record whose tree is not complete ({root[:12]})'
    out = run([BIN / 'panpak-stub', 'run', cur], env, ok=False)
    version = out.stdout.strip()
    assert version in allowed, (version, out.stderr)
    return version


def main():
    seed = int(os.environ.get('PANPAK_TEST_SEED', time.time()))
    print('seed', seed)
    rnd = random.Random(seed)
    with tempfile.TemporaryDirectory(prefix='panpak-interrupt-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'), XDG_RUNTIME_DIR=str(runtime), LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        v1 = resign(pack(tmp, env, '1', ()), '1.0', port)
        v2 = resign(pack(tmp, env, '2', (3, 17, 40)), '1.1', port)
        (tmp / 'v1.panpak').write_bytes(v1)
        serve = tmp / 'serve'
        serve.mkdir()
        (serve / 'remote.panpak').write_bytes(v2)
        store = tmp / 'data/panpak'
        install = [BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak']
        # How long an install takes when nothing stops it: the sweep goes over that.
        t = time.time()
        run(install, env)
        total = time.time() - t
        run([BIN / 'panpak', 'remove', '--yes', ID], env)
        print(f'an install takes {total * 1000:.0f} ms here')

        # 1. install killed at a sweep of moments and at random ones; then again to the end.
        delays = [total * f / 40 for f in range(0, 44)] + [rnd.uniform(0, total * 1.1) for _ in range(30)]
        stopped = 0
        stages = {'not installed': 0, 'installed': 0}
        for n, d in enumerate(delays):
            finished = killed(install, env, d)
            stopped += not finished
            v = invariant(store, env, {'version 1'})
            stages['installed' if v else 'not installed'] += 1
            run(install, env)
            assert invariant(store, env, {'version 1'}) == 'version 1', f'the install after a stop did not finish (delay {d:.3f} s)'
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            left = [p.name for p in (store / 'packages').iterdir()] if (store / 'packages').exists() else []
            assert not left, f'a record is left after remove: {left}'
        print(f'install: {len(delays)} stops ({stopped} before the end), {stages}: never a link before its tree PASS')

        # 2. update killed the same way.
        run(install, env)
        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', serve, str(port), 'normal'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.4)
            update = [BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', ID]
            t = time.time()
            run(update, env)
            total = time.time() - t
            run([BIN / 'panpak', 'rollback', ID], env)  # back to version 1 with version 2 in the store
            run([BIN / 'panpak', 'remove', '--yes', ID], env)
            print(f'an update takes {total * 1000:.0f} ms here')
            delays = [total * f / 40 for f in range(0, 44)] + [rnd.uniform(0, total * 1.1) for _ in range(30)]
            outcome = {'old': 0, 'new': 0}
            for d in delays:
                run(install, env)
                finished = killed(update, env, d)
                v = invariant(store, env, {'version 1', 'version 2'})
                outcome['new' if v == 'version 2' else 'old'] += 1
                if v == 'version 1':
                    run(update, env)
                assert invariant(store, env, {'version 2'}) == 'version 2', f'the update after a stop did not finish (delay {d:.3f} s, finished {finished})'
                run([BIN / 'panpak', 'remove', '--yes', ID], env)
            print(f'update: {len(delays)} stops, {outcome}: the app is on one whole version, and the next update finishes PASS')
        finally:
            server.terminate()
            server.wait(timeout=5)
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
