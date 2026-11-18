#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The release table over HTTP (PANPAK-FORMAT.md 3.1.10): what `panpak update`, `apply` and `rollback` refuse when the package that arrives is not
the next release of the installed one: another publication of the same revision, an older revision, a package that does not continue the history,
another channel, a revision below the floor; and what they mark and keep (the security number, the floor). Real binaries (build the release first),
an isolated store, and a local server with Range support. The packages are made by `panpak pack --previous`, and so are not signed again.
"""
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC  # noqa: E402

ID = 'org.example.release'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok is True:
        assert p.returncode == 0, p.stdout + p.stderr
    elif ok is False:
        assert p.returncode != 0, p.stdout + p.stderr
    return p


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-release-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()

        def pack(name, release='', previous=None):
            tree = tmp / f'tree-{name}'
            tree.mkdir()
            (tree / 'hello').write_text(f'#!/bin/sh\necho "{name}"\n')
            (tree / 'hello').chmod(0o755)
            (tree / 'panpak.toml').write_text(
                f'[app]\nid="{ID}"\nname="Release"\nversion="{name}"\nentry="hello"\n{release}'
                f'[update]\nurl="http://127.0.0.1:{port}/remote.panpak"\nkey="ed25519:{PUBLIC}"\n')
            out = tmp / f'{name}.panpak'
            args = [BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out]
            if previous:
                args += ['--previous', previous]
            run(args, env)
            return out

        r1 = pack('r1')
        r2 = pack('r2', previous=r1)
        r2fork = pack('r2fork', '[release]\nrevision = 2\n', previous=r1)
        r3forked = pack('r3forked', previous=r2fork)
        r3bare = pack('r3bare', '[release]\nrevision = 3\n')
        beta = pack('beta9', '[release]\nrevision = 9\nchannel = "beta"\n')
        r3 = pack('r3', '[release]\nrevision = 3\nsecurity = 3\n', previous=r2)
        r4 = pack('r4', '[release]\nrevision = 4\nfloor = 4\n', previous=r3)

        serve = tmp / 'serve'
        serve.mkdir()

        def serving(p):
            (serve / 'remote.panpak').write_bytes(Path(p).read_bytes())

        store = tmp / 'data/panpak'
        appdir = store / 'apps' / ID

        def update(flags=(), ok=True):
            return run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', *flags, ID], env, ok=ok)

        def current_version():
            return run([BIN / 'panpak', 'inspect', appdir / 'current'], env).stdout

        def untouched(before):
            assert not (appdir / 'pending').exists() and not (appdir / 'pending-state').exists(), 'nothing is staged after a refusal'
            assert (appdir / 'current').resolve() == before, 'current did not move'

        run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', r1], env)
        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', serve, str(port), 'normal'], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.4)
            # 1. The next release, with its history, enters.
            serving(r2)
            r = update()
            assert 'updated' in r.stdout, r.stdout
            installed = (appdir / 'current').resolve()
            print('update to the next revision (with a history) PASS')

            # 2. The same revision with other bytes: a fork or a replay. The same package again: nothing to do.
            serving(r2fork)
            r = update(ok=False)
            assert 'PANPAK-E4004' in r.stderr, r.stderr
            untouched(installed)
            serving(r2)
            assert 'already at' in update().stdout
            print('two publications of one revision are refused, the same one is "already at" PASS')

            # 3. An older revision.
            serving(r1)
            r = update(ok=False)
            assert 'PANPAK-E4001' in r.stderr and '--allow-downgrade' in r.stderr, r.stderr
            untouched(installed)
            print('an older revision is refused PASS')

            # 4. A package that does not continue the installed one: the history lacks it, or is missing. The downloaded record goes.
            before_records = sorted((store / 'packages').iterdir())
            for pkg, why in ((r3forked, 'not in the history'), (r3bare, 'has no history')):
                serving(pkg)
                r = update(ok=False)
                assert 'PANPAK-E4006' in r.stderr and why in r.stderr, r.stderr
                untouched(installed)
                assert sorted((store / 'packages').iterdir()) == before_records, 'the record of a refused update was taken away'
            print('a package that does not continue the history is refused and leaves nothing PASS')

            # 5. Another channel.
            serving(beta)
            r = update(ok=False)
            assert 'PANPAK-E4005' in r.stderr and '--switch-channel' in r.stderr, r.stderr
            untouched(installed)
            print('another channel is refused PASS')

            # 6. The next release carries a security number above the installed revision: the staging marks it.
            serving(r3)
            r = run([BIN / 'panpak', 'update', '--stage', '--allow-insecure-http', '--timeout', '5', ID], env)
            assert 'staged' in r.stdout, r.stdout
            state = (appdir / 'pending-state').read_text()
            assert 'security=1' in state, state
            run([BIN / 'panpak', 'apply', ID], env)
            assert (appdir / 'previous').exists()
            print('a security number above the installed revision marks the staging PASS')

            # 7. A floor: the next release sets it at 4. After it nothing below comes back, by update or by rollback.
            serving(r4)
            update()
            assert (appdir / 'floor').read_text().strip() == '4'
            r = run([BIN / 'panpak', 'rollback', ID], env, ok=False)
            assert 'PANPAK-E4003' in r.stderr and 'floor 4' in r.stderr, r.stderr
            installed = (appdir / 'current').resolve()
            serving(r3)
            r = update(['--allow-downgrade'], ok=False)
            assert 'PANPAK-E4003' in r.stderr, r.stderr
            untouched(installed)
            print('nothing below the floor: rollback and update both refuse, --allow-downgrade does not open it PASS')
        finally:
            server.terminate()
            server.wait(timeout=5)


if __name__ == '__main__':
    main()
