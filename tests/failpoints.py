#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Every command stopped at every named point (`panpak_core::failpoint!`, compiled only in a test build), and the store is whole afterwards.

The points are read from the sources, so a point that is added is run without a change here. For each point and each command in which it can be hit
(`install`, `update`, `gc`, `update --stage`, the start that applies a staged update, an update that replaces the key, `rollback`, `repair`), the command is stopped there (`abort`, as a kill: no destructor runs), and then:

- the app is not installed, or `current` names a record whose tree is complete and whose app starts, and `previous` too if it exists
  (for `repair`: the tree is whole or the start says PANPAK-E2007 and `panpak repair`);
- the same command run again, with no point, finishes and leaves the app on the version it was asked for (`doctor` reads it);
- every point was hit in at least one command (a point that nothing reaches is a point that proves nothing).

Needs a build with the feature: `cargo build --locked --release --target x86_64-unknown-linux-musl --target-dir target/failpoints
--features panpak/failpoints,panpak-stub/failpoints -p panpak -p panpak-stub` (`PANPAK_FAILPOINT_BIN` names the folder of the two binaries).
"""
import os
from pathlib import Path
import random
import re
import socket
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import PUBLIC, resign  # noqa: E402

BIN = Path(os.environ.get('PANPAK_FAILPOINT_BIN', ROOT / 'target/failpoints/x86_64-unknown-linux-musl/release'))
ID = 'org.example.incremental'
FILES = 30


def points():
    names = set()
    for src in list((ROOT / 'panpak/src').rglob('*.rs')) + list((ROOT / 'panpak-stub/src').rglob('*.rs')) + list((ROOT / 'panpak-core/src').rglob('*.rs')):
        names |= set(re.findall(r'failpoint!\("([a-z_.]+)"\)', src.read_text()))
    return sorted(names)


def commands_for(name):
    kind = name.split('.')[0]
    if name == 'apply.pin_both':
        return ['rotate']
    return {'install': ['install'], 'update': ['update'], 'rollback': ['rollback'], 'repair': ['repair'],
            'gc': ['gc'], 'stage': ['update', 'stage'], 'apply': ['update', 'apply_start'],
            'prepare': ['install', 'update', 'stage', 'repair'], 'durable': ['install', 'update', 'stage']}[kind]


def run(args, env, ok=True, timeout=120):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def pack(tmp, env, version):
    tree = tmp / f'tree{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(5)
    for i in range(FILES):
        data = bytearray(rng.randbytes(120_000))
        if version == '2' and i in (3, 17):
            data[100:200] = b'2' * 100
        (tree / f'payload{i}').write_bytes(bytes(data))
    out = tmp / f'packed{version}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes()


class Lab:
    def __init__(self, tmp, env, port, v1, v2):
        self.tmp, self.env, self.port, self.v1, self.v2 = tmp, env, port, v1, v2
        self.n = 0

    def fresh(self):
        """A new store (a new home, so nothing of an earlier run is there)."""
        self.n += 1
        home = self.tmp / f'home{self.n}'
        home.mkdir()
        (home / 'run').mkdir(mode=0o700)
        env = dict(self.env, HOME=str(home), XDG_DATA_HOME=str(home / 'data'), XDG_CACHE_HOME=str(home / 'cache'), XDG_CONFIG_HOME=str(home / 'config'), XDG_RUNTIME_DIR=str(home / 'run'))
        return env, home / 'data/panpak'

    def install_v1(self, env):
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', self.tmp / 'v1.panpak'], env)

    def update(self, env, extra=None):
        return run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', ID], dict(env, **(extra or {})), ok=extra is None)


def whole(store, env, allowed, allow_missing_files=False):
    """`current` (and `previous`) name records with complete trees, and the app starts one of `allowed`."""
    apps = store / 'apps' / ID
    for link in ('current', 'previous'):
        l = apps / link
        if not l.is_symlink():
            continue
        rec = l.resolve()
        assert rec.parent == (store / 'packages').resolve() and rec.name.endswith('.rec'), f'{link} -> {rec}'
        root = rec.read_bytes()[-32:].hex()
        if link == 'current' and allow_missing_files and not (store / 'trees' / root / '.complete').exists():
            out = run([BIN / 'panpak-stub', 'run', apps / 'current'], env, ok=False)
            assert 'PANPAK-E2007' in out.stderr and 'panpak repair' in out.stderr, out.stderr
            continue
        assert (store / 'trees' / root / '.complete').exists(), f'{link} names a record whose tree is not complete'
    if not (apps / 'current').is_symlink():
        return None
    out = run([BIN / 'panpak-stub', 'run', apps / 'current'], env, ok=False)
    if allow_missing_files and 'PANPAK-E2007' in out.stderr:
        return 'missing'
    assert out.stdout.strip() in allowed, (out.stdout, out.stderr)
    return out.stdout.strip()


def doctor_ok(env):
    d = run([BIN / 'panpak', 'doctor', ID], env, ok=False)
    assert 'verified package: org.example.incremental' in d.stdout and 'PANPAK-E2007' not in d.stdout, d.stdout


def pack_rotated(tmp, env, port):
    """Version 2 signed by a second key, with the proof of the first one that the second may take its place."""
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    sk2 = bytes([42]) * 32
    pub2 = Ed25519PrivateKey.from_private_bytes(sk2).public_key().public_bytes_raw().hex()
    (tmp / 'secret2.key').write_text(sk2.hex() + '\n')
    (tmp / 'secret2.key').chmod(0o600)
    tree = tmp / 'tree-rotated'
    tree.mkdir()
    (tree / 'hello').write_text('#!/bin/sh\necho "version 2"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="1.1"\nentry="hello"\n[release]\nrevision=2\n[update]\nkey="ed25519:{pub2}"\nurl="http://127.0.0.1:{port}/remote.panpak"\n')
    rng = random.Random(5)
    for i in range(FILES):
        (tree / f'payload{i}').write_bytes(rng.randbytes(120_000))
    out = tmp / 'packed-rotated.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret2.key', '--rotate-from-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes(), pub2


def main():
    if not (BIN / 'panpak').exists():
        sys.exit(f'no failpoint build in {BIN}: see the top of this file')
    names = points()
    assert len(names) >= 20, names
    with tempfile.TemporaryDirectory(prefix='panpak-failpoints-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        env = dict(os.environ, LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        env.pop('PANPAK_FAILPOINT', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        base = dict(env, XDG_DATA_HOME=str(tmp / 'pack-data'), HOME=str(tmp))
        v1 = resign(pack(tmp, base, '1'), '1.0', port)
        v2 = resign(pack(tmp, base, '2'), '1.1', port)
        (tmp / 'v1.panpak').write_bytes(v1)
        serve = tmp / 'serve'
        serve.mkdir()
        (serve / 'remote.panpak').write_bytes(v1)
        vr, pub2 = pack_rotated(tmp, base, port)
        lab = Lab(tmp, env, port, v1, v2)
        stub = BIN / 'panpak-stub'
        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', serve, str(port), 'normal'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        hit = {n: [] for n in names}
        try:
            time.sleep(0.4)
            for name in names:
                for cmd in commands_for(name):
                    e, store = lab.fresh()
                    fp = dict(PANPAK_FAILPOINT=name)
                    if cmd == 'install':
                        p = run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak'], dict(e, **fp), ok=False)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 1'})
                            run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak'], e)
                        assert whole(store, e, {'version 1'}) == 'version 1', f'{name}: install again did not finish'
                    elif cmd == 'update':
                        (serve / 'remote.panpak').write_bytes(v2)
                        lab.install_v1(e)
                        p = lab.update(e, fp)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 1', 'version 2'})
                            run([BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', ID], e)
                        assert whole(store, e, {'version 2'}) == 'version 2', f'{name}: update again did not finish'
                    elif cmd == 'rollback':
                        (serve / 'remote.panpak').write_bytes(v2)
                        lab.install_v1(e)
                        lab.update(e)
                        p = run([BIN / 'panpak', 'rollback', ID], dict(e, **fp), ok=False)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 1', 'version 2'})
                            run([BIN / 'panpak', 'rollback', ID], e)
                        assert whole(store, e, {'version 1', 'version 2'}) in ('version 1', 'version 2')
                    elif cmd == 'stage':
                        (serve / 'remote.panpak').write_bytes(v2)
                        lab.install_v1(e)
                        args = [BIN / 'panpak', 'update', '--stage', '--allow-insecure-http', '--timeout', '5', ID]
                        p = run(args, dict(e, **fp), ok=False)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 1', 'version 2'})  # a start applies what is staged
                            run(args, e)
                        assert whole(store, e, {'version 1', 'version 2'}) in ('version 1', 'version 2')
                        assert run([stub, 'run', store / 'apps' / ID / 'current'], e).stdout.strip() == 'version 2', f'{name}: the staged update was not applied at a start'
                    elif cmd == 'apply_start':
                        (serve / 'remote.panpak').write_bytes(v2)
                        lab.install_v1(e)
                        run([BIN / 'panpak', 'update', '--stage', '--allow-insecure-http', '--timeout', '5', ID], e)
                        p = run([stub, 'run', store / 'apps' / ID / 'current'], dict(e, **fp), ok=False)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 1', 'version 2'})
                        assert run([stub, 'run', store / 'apps' / ID / 'current'], e).stdout.strip() == 'version 2', f'{name}: the staged update was not applied'
                        assert whole(store, e, {'version 2'}) == 'version 2'
                    elif cmd == 'gc':
                        (serve / 'remote.panpak').write_bytes(v2)
                        lab.install_v1(e)
                        lab.update(e)
                        # Garbage: a record that no link names, an orphan tree, an orphan blob.
                        stray = store / 'packages' / f'{ID}-9.9-{"ee" * 6}.rec'
                        stray.write_bytes((store / 'apps' / ID / 'previous').resolve().read_bytes())
                        orphan = store / 'trees' / ('ab' * 32)
                        orphan.mkdir()
                        (orphan / 'x').write_text('x')
                        blob = store / 'blobs' / (('cd' * 32) + '-5')
                        blob.write_text('hello')
                        args = [BIN / 'panpak', 'gc']
                        p = run(args, dict(e, **fp), ok=False)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 2'})   # whatever was done, the app and its previous version are whole
                        run(args, e)
                        assert not stray.exists() and not orphan.exists() and not blob.exists(), f'{name}: the second run finishes the collection'
                        assert whole(store, e, {'version 2'}) == 'version 2'
                    elif cmd == 'rotate':
                        (serve / 'remote.panpak').write_bytes(vr)
                        lab.install_v1(e)
                        args = [BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '5', ID]
                        p = run(args, dict(e, **fp), ok=False)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 1', 'version 2'})
                            run(args, e)
                        assert whole(store, e, {'version 2'}) == 'version 2', f'{name}: the update that replaces the key did not finish'
                        pin = [l.split('=', 1)[1] for l in (store / 'apps' / ID / 'trust').read_text().split() if l.startswith('root=')]
                        assert pin == [f'ed25519:{pub2}'], pin
                    else:  # repair
                        (serve / 'remote.panpak').write_bytes(v1)
                        lab.install_v1(e)
                        root = v1[-32:].hex()
                        victim = store / 'trees' / root / 'payload4'
                        original = victim.read_bytes()
                        victim.chmod(0o644)
                        victim.write_bytes(b'damaged')
                        victim.chmod(0o444)
                        args = [BIN / 'panpak', 'repair', '--allow-insecure-http', '--timeout', '5', ID]
                        p = run(args, dict(e, **fp), ok=False)
                        fired = f'failpoint {name}' in p.stderr
                        if fired:
                            whole(store, e, {'version 1'}, allow_missing_files=True)
                            run(args, e)
                        assert (store / 'trees' / root / 'payload4').read_bytes() == original, f'{name}: repair again did not make the file right'
                        assert whole(store, e, {'version 1'}) == 'version 1'
                    if fired:
                        hit[name].append(cmd)
                    doctor_ok(e)
            # A collection while an install is in the middle of its work (paused after the blobs are named and before they are linked into the tree):
            # it must not touch anything, and say that it was left for the next time. Nothing here depends on how long the install takes.
            e, store = lab.fresh()
            pause = tmp / 'pause'
            pause.mkdir()
            slow = subprocess.Popen([str(BIN / 'panpak'), 'install', '--yes', '--trust-first-key', '--runtime', str(BIN / 'panpak-stub'), str(tmp / 'v1.panpak')],
                                    env=dict(e, PANPAK_FAILPOINT_PAUSE='prepare.blobs_named', PANPAK_FAILPOINT_DIR=str(pause)), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            for _ in range(200):
                if (pause / 'reached').exists():
                    break
                time.sleep(0.1)
            assert (pause / 'reached').exists(), 'the install reached the pause'
            blobs = sorted(p.name for p in (store / 'blobs').iterdir() if p.is_file())
            assert blobs and all(os.stat(store / 'blobs' / b).st_nlink == 1 for b in blobs), 'in the middle: blobs named, no tree links them yet'
            orphan = store / 'trees' / ('ab' * 32)
            orphan.mkdir()
            (orphan / 'x').write_text('x')
            before = sorted(str(p.relative_to(store)) for p in store.rglob('*'))
            out = run([BIN / 'panpak', 'gc'], e)
            assert 'left for the next time' in out.stdout, out.stdout
            assert sorted(str(p.relative_to(store)) for p in store.rglob('*')) == before, 'the collection touched a store with an install in the middle'
            (pause / 'go').write_text('')
            so, se = slow.communicate(timeout=60)
            assert slow.returncode == 0, so + se
            out = run([BIN / 'panpak', 'gc'], e).stdout
            assert not orphan.exists() and 'removed' in out, out
            assert run([stub, 'run', store / 'apps' / ID / 'current'], e).stdout.strip() == 'version 1'
            print('a collection run in the middle of a slow install touches nothing and says so; after the install it collects what is garbage and the app starts PASS')
        finally:
            server.terminate()
            server.wait(timeout=5)
        never = [n for n, c in hit.items() if not c]
        assert not never, f'points that no command reached: {never}'
        total = sum(len(c) for c in hit.values())
        print(f'{len(names)} points, {total} stops, in {sorted({c for v in hit.values() for c in v})}: the store is whole after each, and the command run again finishes PASS')
        for n in names:
            print(f'  {n}: {", ".join(hit[n])}')
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
