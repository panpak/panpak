#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak gc` (PANPAK-FORMAT.md 6.8), with real binaries in an isolated store.

Reachability from the records: what `current`, `previous` and `pending` name is alive, and so are its tree, its farm (a farm lives while any record with its
root lives) and the blobs that a kept tree links. Checked: a dry run changes nothing; what the third version made unreachable goes (record, tree, farm, the
blobs only it linked) and what is shared stays; the app starts and rolls back after; a second run has nothing to do; a command at work (the store lock held
shared) makes the collection wait for the next time and touch nothing; an app with a copy running keeps everything; packages of the old layout (`*.panpak`) go; abandoned download folders, cache entries of a farm that is gone, work folders of dead processes and runtimes that
nobody names go; names that are not ours are left alone.
"""
import fcntl
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
from incremental import BIN, PUBLIC, resign  # noqa: E402

ID = 'org.example.incremental'
OTHER = 'org.example.other'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def pack(tmp, env, app_id, version, changed=()):
    tree = tmp / f'tree-{app_id}-{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{app_id}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(11)
    for i in range(12):
        d = bytearray(rng.randbytes(160_000))
        if i in changed:
            d[50:60] = f'v{version}-{i}'.encode().ljust(10, b'.')[:10]
        (tree / f'payload{i}').write_bytes(bytes(d))
    out = tmp / f'packed-{app_id}-{version}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes()


def names(d):
    return sorted(p.name for p in d.iterdir()) if d.exists() else []


def snapshot(store):
    out = {}
    for sub in ('packages', 'trees', 'farms', 'blobs', 'abi-cache', 'staging', 'runtimes'):
        out[sub] = names(store / sub)
    return out


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-gc-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        v = {n: resign(pack(tmp, env, ID, n, ch), f'1.{i}', port) for i, (n, ch) in enumerate((('1', ()), ('2', (2, 8)), ('3', (2, 8, 5))))}
        other = pack(tmp, env, OTHER, '1', (11,))
        (tmp / 'v1.panpak').write_bytes(v['1'])
        (tmp / 'other.panpak').write_bytes(other)
        serve = tmp / 'serve'
        serve.mkdir()
        store = tmp / 'data/panpak'
        appdir = store / 'apps' / ID
        stub = store / 'bin/panpak-stub'
        panpak = BIN / 'panpak'
        srv = subprocess.Popen([sys.executable, str(ROOT / 'tests/range_server.py'), str(serve), str(port), 'normal'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.6)
            run([panpak, 'install', '--yes', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak'], env)
            for n in ('2', '3'):
                (serve / 'remote.panpak').write_bytes(v[n])
                run([panpak, 'update', '--allow-insecure-http', '--timeout', '5', ID], env)
            roots = {n: v[n][-32:].hex() for n in v}
            assert run([stub, 'run', appdir / 'current'], env).stdout.strip() == 'version 3'
            assert not (store / 'trees' / roots['1']).exists() and not (store / 'farms' / roots['1']).exists(), 'the update that made the first version unreachable collected it'
            assert len(names(store / 'packages')) == 2, names(store / 'packages')
            assert all(os.stat(store / 'blobs' / b).st_nlink > 1 for b in names(store / 'blobs') if (store / 'blobs' / b).is_file()), 'no blob that nothing links'
            for n in ('2', '3'):
                assert (store / 'trees' / roots[n] / '.complete').exists() and (store / 'farms' / roots[n]).is_dir(), f'version {n} is kept whole'
            assert run([stub, 'run', appdir / 'current'], env).stdout.strip() == 'version 3'
            run([panpak, 'rollback', ID], env)
            assert run([stub, 'run', appdir / 'current'], env).stdout.strip() == 'version 2', 'previous still starts'
            run([panpak, 'rollback', ID], env)
            print('install and two updates: the update that made the first version unreachable collected its record, tree, farm and blobs; the other two start and roll back PASS')

            # 1. garbage of every kind: a dry run changes nothing and says what it would do; then the real run, then an empty one
            stray = store / 'packages' / f'{ID}-9.9-{"ee" * 6}.rec'
            stray.write_bytes((appdir / 'previous').resolve().read_bytes())
            old_package = store / 'packages' / 'old-layout.panpak'
            old_package.write_bytes(v['1'])
            orphan_tree = store / 'trees' / ('ab' * 32)
            orphan_tree.mkdir()
            (orphan_tree / 'x').write_text('x')
            orphan_blob = store / 'blobs' / (('cd' * 32) + '-5')
            orphan_blob.write_text('hello')
            work = store / 'blobs' / '.work-2147483000-1'
            work.mkdir()
            (work / 'part').write_text('p')
            stranger = store / 'trees' / 'README'
            stranger.write_text('not ours')
            before = snapshot(store)
            out = run([panpak, 'gc', '--dry-run'], env).stdout
            assert 'would remove 2 record' in out and 'would remove 1 tree' in out, out
            assert snapshot(store) == before, 'a dry run changes nothing'
            out = run([panpak, 'gc'], env).stdout
            assert 'removed 2 record' in out and 'removed 1 tree' in out and 'blob' in out and 'freed about' in out, out
            assert not (stray.exists() or old_package.exists() or orphan_tree.exists() or orphan_blob.exists() or work.exists()), 'all of it went, at once: no clock decides'
            assert stranger.exists(), 'a name that is not ours is left alone'
            stranger.unlink()
            assert run([stub, 'run', appdir / 'current'], env).stdout.strip() == 'version 3'
            assert 'nothing to collect' in run([panpak, 'gc'], env).stdout, 'a second run has nothing to do'
            print('dry run changes nothing; a stray record, an old-layout package, an orphan tree and blob and the work folder of a dead process go at once; a stranger stays; a second run is empty PASS')

            # 1b. a command at work (the store lock held shared): the collection waits for the next time and touches nothing
            stray.write_bytes((appdir / 'previous').resolve().read_bytes())
            orphan_blob.write_text('hello')
            with open(store / 'locks' / '.store', 'a') as at_work:
                fcntl.flock(at_work, fcntl.LOCK_SH)
                before = snapshot(store)
                p = run([panpak, 'gc'], env)
                assert 'left for the next time' in p.stdout and snapshot(store) == before, p.stdout
            run([panpak, 'gc'], env)
            assert not stray.exists() and not orphan_blob.exists()
            print('a command at work holds the store lock: the collection says it was left for the next time and changes nothing; the next one collects PASS')

            # 2. a second app that shares files: what is shared stays when it is removed
            run([panpak, 'install', '--yes', tmp / 'other.panpak'], env)
            shared_before = len(names(store / 'blobs'))
            run([panpak, 'remove', '--yes', OTHER], env)
            run([panpak, 'gc'], env)
            assert len(names(store / 'blobs')) <= shared_before
            assert run([stub, 'run', appdir / 'current'], env).stdout.strip() == 'version 3'
            assert 'does not match' not in run([panpak, 'doctor', (appdir / 'current').resolve()], env, ok=False).stdout
            print('a removed app: its own blobs go, the ones it shared with the other app stay, the other app runs and checks out PASS')

            # 4. a copy that runs keeps everything of its app
            (serve / 'remote.panpak').write_bytes(v['3'])
            # Make a record unreachable again: stage nothing; instead copy the record of version 2 under another name and ask gc to keep it while a copy runs.
            stray.write_bytes((appdir / 'previous').resolve().read_bytes())
            copies = runtime / 'panpak' / ID
            copies.mkdir(parents=True, exist_ok=True)
            copies.chmod(0o700)
            with open(copies / 'lock', 'a') as copy:
                fcntl.flock(copy, fcntl.LOCK_SH)
                run([panpak, 'gc'], env)
                assert stray.exists(), 'a copy runs: everything of the app stays'
            run([panpak, 'gc'], env)
            assert not stray.exists(), 'with no copy the unreachable record goes'
            print('with a copy running the app keeps everything; when it ends the unreachable record goes PASS')

            # 5. download folders, the cache of a farm that is gone, runtimes nobody names
            old = store / 'staging' / ID / ('01' * 32)
            old.mkdir(parents=True)
            (old / 'chunk').write_text('c')
            fresh = store / 'staging' / ID / ('02' * 32)
            fresh.mkdir()
            (fresh / 'chunk').write_text('c')
            week = time.time() - 9 * 86400
            for p in (old / 'chunk', old):
                os.utime(p, (week, week))
            cache = store / 'abi-cache' / 'gone.toml'
            cache.write_text(f'stamp = "{store}/farms/{"ef" * 32}"\n')
            keep_cache = [p for p in (store / 'abi-cache').iterdir() if p.name != 'gone.toml']
            fake_rt = store / 'runtimes' / '77'
            fake_rt.mkdir()
            (fake_rt / 'panpak-stub').write_text('x')
            run([panpak, 'gc'], env)
            assert not old.exists() and fresh.exists(), 'the abandoned download folder goes, the one in use stays'
            assert not cache.exists() and all(p.exists() for p in keep_cache), 'the cache entry of a farm that is gone goes; the others stay'
            assert not fake_rt.exists(), 'a runtime that nobody names goes'
            best = names(store / 'runtimes')
            assert best and run([stub, 'run', appdir / 'current'], env).stdout.strip() == 'version 3', f'the runtime that is named stays: {best}'
            print('download folders, a cache entry of a farm that is gone and an unnamed runtime go; the named runtime stays PASS')

            # 6. a collection at the same time as starts and rollbacks: every start is a whole version, nothing is taken from under it
            import threading
            stop = threading.Event()
            gc_runs = []

            def collector():
                while not stop.is_set():
                    gc_runs.append(subprocess.run([str(panpak), 'gc'], env=env, capture_output=True, text=True, timeout=60))

            t = threading.Thread(target=collector)
            t.start()
            seen = set()
            for i in range(24):
                if i % 3 == 0:
                    subprocess.run([str(panpak), 'rollback', ID], env=env, capture_output=True, text=True, timeout=60)   # may be refused while a copy runs: fine
                p = subprocess.run([str(stub), 'run', str(appdir / 'current')], env=env, capture_output=True, text=True, timeout=60)
                assert p.returncode == 0 and p.stdout.strip() in ('version 2', 'version 3'), p.stdout + p.stderr
                seen.add(p.stdout.strip())
            stop.set()
            t.join()
            assert gc_runs and all(r.returncode == 0 for r in gc_runs), [r.stderr for r in gc_runs if r.returncode]
            print(f'{len(gc_runs)} collections while the app was started 24 times and rolled back 8 times: every start was a whole version ({sorted(seen)}) PASS')
        finally:
            srv.terminate()
            srv.wait()
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
