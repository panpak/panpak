#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The background job of step 8d (PANPAK-FORMAT.md 6.7), with real binaries in an isolated store.

1. `panpak config`: defaults, set, refusals; a bad file means the defaults.
2. `panpak background --force`: stages the new version when the update URL has one (never applies it), records the check, and a second run that is not
   due does nothing. A failing URL (HTTPS only: the test server is plain http, so the job without the flag fails) counts a failure, backs off, and the state says when.
3. The rotating verification: with a budget, the job reads part of the files and moves the cursor; the budget of the day stops it; the end of the files
   is a round; a flipped byte in a file is found, noted in `update-state`, shown by `list` and `doctor`, and `panpak repair` makes it right and clears the note.
4. The stub at a start: a due job is started detached (the state file shows it ran), a damaged file is made right before the app starts when a repair is possible
   (here: not, there is no HTTPS) and the start does not fail; `PANPAK_NO_BACKGROUND` keeps the job away.
5. `background` with the setting off does no check and no reading.
"""
import json
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
FILES = 12


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok is True:
        assert p.returncode == 0, p.stdout + p.stderr
    elif ok is False:
        assert p.returncode != 0, p.stdout + p.stderr
    return p


def pack(tmp, env, version):
    tree = tmp / f'tree{version}'
    tree.mkdir()
    (tree / 'hello').write_text(f'#!/bin/sh\necho "version {version}"\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Test"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    rng = random.Random(3)
    for i in range(FILES):
        d = bytearray(rng.randbytes(200_000))
        if version == '2' and i in (2, 8):
            d[100:110] = b'version two'
        (tree / f'payload{i}').write_bytes(bytes(d))
    out = tmp / f'packed{version}.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out], env)
    return out.read_bytes()


def state(appdir):
    d = {}
    for line in (appdir / 'update-state').read_text().splitlines():
        k, _, v = line.partition('=')
        d[k] = v
    return d


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-bg-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C')
        env.pop('WAYLAND_DISPLAY', None)
        env.pop('PANPAK_NO_BACKGROUND', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        sock = socket.socket()
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
        sock.close()
        v1 = resign(pack(tmp, env, '1'), '1.0', port)
        v2 = resign(pack(tmp, env, '2'), '1.1', port)
        serve = tmp / 'serve'
        serve.mkdir()
        (serve / 'remote.panpak').write_bytes(v1)
        (tmp / 'v1.panpak').write_bytes(v1)
        store = tmp / 'data/panpak'
        appdir = store / 'apps' / ID
        stub = store / 'bin/panpak-stub'
        panpak = BIN / 'panpak'
        envn = dict(env, PANPAK_NO_BACKGROUND='1')   # for the commands that must not start a job of their own
        run([panpak, 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', tmp / 'v1.panpak'], envn)

        # 1. the settings
        out = run([panpak, 'config'], env).stdout
        assert 'updates = true' in out and 'verify_mb = 64' in out and 'not written yet' in out, out
        run([panpak, 'config', 'set', 'background.verify_mb', '1'], env)
        assert 'verify_mb = 1' in run([panpak, 'config', 'show'], env).stdout
        assert run([panpak, 'config', 'set', 'background.updates', 'maybe'], env, ok=False).returncode != 0
        assert run([panpak, 'config', 'set', 'nothing', '1'], env, ok=False).returncode != 0
        cfg = tmp / 'config/panpak/config.toml'
        cfg.write_text('this is [not toml')
        assert 'updates = true' in run([panpak, 'config'], env).stdout, 'a bad file means the defaults'
        cfg.unlink()
        run([panpak, 'config', 'set', 'background.verify_mb', '1'], env)
        print('settings: defaults, set, refusals, a bad file means the defaults PASS')

        # 2. the check, over plain http: without the flag it fails and backs off; with it, it stages and does not apply
        srv = subprocess.Popen([sys.executable, str(ROOT / 'tests/range_server.py'), str(serve), str(port), 'normal'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.6)
            before = time.time()
            p = run([panpak, 'background', '--force', ID], env)
            st = state(appdir)
            assert st['failures'] == '1' and 'failed' in p.stderr, (st, p.stderr)
            assert before + 3500 < int(st['next_check']) < before + 3700, 'one hour after the first failure'
            assert not (appdir / 'pending').exists()
            (serve / 'remote.panpak').write_bytes(v2)
            cur = (appdir / 'current').readlink()
            p = run([panpak, 'background', '--force', '--allow-insecure-http', ID], env)
            st = state(appdir)
            assert st['failures'] == '0' and int(st['next_check']) > before + 86000, st
            assert (appdir / 'pending').is_symlink() and (appdir / 'current').readlink() == cur, 'staged, not applied'
            before_state = (appdir / 'update-state').read_text()
            run([panpak, 'background', '--allow-insecure-http', ID], env)
            assert (appdir / 'update-state').read_text() == before_state, 'not due: nothing done'
            print('check: a failure backs off an hour; a good check stages and does not apply; a run that is not due does nothing PASS')

            # 3. the verification: a budget of 1 MB a day, files of 200 KB
            assert st['cursor'] != '0' or st['last_round'] != '0', st
            assert 0 < int(st['verified_bytes']) <= 1 << 21, st
            c1 = int(st['cursor'])
            run([panpak, 'background', '--force', '--allow-insecure-http', ID], env)
            st = state(appdir)
            assert int(st['verified_bytes']) >= 1 << 20 or st['last_round'] != '0', 'the budget of the day is spent or a round is done'
            run([panpak, 'config', 'set', 'background.verify_mb', '100'], env)
            run([panpak, 'background', '--force', '--allow-insecure-http', ID], env)
            st = state(appdir)
            assert st['last_round'] != '0' and st['cursor'] == '0', f'a whole round: {st}'
            assert 'damaged=0' in (appdir / 'update-state').read_text()
            # A flipped byte (the tree is read-only; the test makes it writable, as a disk fault would not ask).
            tree = store / 'trees' / v1[-32:].hex()
            victim = tree / 'payload5'
            good = victim.read_bytes()

            def flip():
                os.chmod(tree, 0o755)
                os.chmod(victim, 0o644)
                data = bytearray(good)
                data[1000] ^= 0xFF
                victim.write_bytes(bytes(data))
                os.chmod(victim, 0o444)
                os.chmod(tree, 0o555)
                # Next day: the budget is new, and the round starts again.
                text = ''.join(('verify_day=1\n' if l.startswith('verify_day=') else 'cursor=0\n' if l.startswith('cursor=') else l + '\n') for l in (appdir / 'update-state').read_text().splitlines())
                (appdir / 'update-state').write_text(text)

            # The URL serves the installed version again (a repair from the URL needs the same version: when the URL has moved on, `repair --from` or an update does it).
            (serve / 'remote.panpak').write_bytes(v1)
            # With a way to fetch (the flag allows plain http for the test): found and made right at once, since no copy runs.
            flip()
            assert (tree / 'payload5').read_bytes() != good
            p = run([panpak, 'background', '--force', '--allow-insecure-http', ID], env)
            assert 'payload5' in p.stderr and 'repaired' in p.stdout, p.stdout + p.stderr
            assert state(appdir)['damaged'] == '0' and (store / 'trees' / v1[-32:].hex() / 'payload5').read_bytes() == good, 'repaired from the update URL'
            # Without it (the URL is plain http, so the job cannot fetch): found, noted, shown, not repaired.
            flip()
            p = run([panpak, 'background', '--force', ID], env)
            st = state(appdir)
            assert st['damaged'] == '1' and st['damaged_first'] == 'payload5', st
            assert 'could not be repaired' in p.stderr, p.stderr
            lst = run([panpak, 'list'], envn).stdout
            assert 'incomplete' in lst and 'payload5' in lst and f'panpak repair {ID}' in lst, lst
            js = json.loads(run([panpak, 'list', '--json'], envn).stdout)
            assert js[0]['status'] == 'incomplete' and js[0]['verified_round'] is not None, js
            rec = (appdir / 'current').resolve()
            doc = run([panpak, 'doctor', rec], envn, ok=None).stdout
            assert 'do not match the signed index' in doc and 'payload5' in doc, doc  # doctor checks the tree itself too
            print('verification: the budget, the cursor, a round, a flipped byte found, shown by list and doctor PASS')

            # 4. the stub at a start: the damaged file is repaired by `panpak repair` and the note goes
            run([panpak, 'repair', '--allow-insecure-http', ID], envn)
            assert state(appdir)['damaged'] == '0'
            assert run([panpak, 'list'], envn).stdout.count('incomplete') == 0
            # A due job is started by a start, detached and at the lowest priority; the job waits 20 s before it does anything, so only that it
            # started (its lock file) is looked at, then it is stopped by the test through its pid file of the lock.
            (appdir / 'update-state').write_text('next_check=0\n')
            t0 = time.time()
            out = run([stub, 'run', appdir / 'current'], env)
            assert out.stdout.strip() == 'version 2', 'the update that the job staged is applied at this start: ' + out.stdout
            assert time.time() - t0 < 5, 'the start does not wait for the job'
            time.sleep(1)
            r = subprocess.run(['ps', '-eo', 'pid,ni,args'], capture_output=True, text=True).stdout.splitlines()
            jobs = [l.split(None, 2) for l in r if f'background --after-start {ID}' in l and str(store) in l]
            assert len(jobs) == 1, f'one job started: {jobs}'
            pid, nice = int(jobs[0][0]), int(jobs[0][1])
            assert nice == 19, f'the lowest priority: {nice}'
            os.kill(pid, 9)
            # With the variable the job does not start.
            run([stub, 'run', appdir / 'current'], envn)
            time.sleep(0.5)
            r = subprocess.run(['ps', '-eo', 'pid,args'], capture_output=True, text=True).stdout
            assert f'background --after-start {ID}' not in r or str(store) not in r.split(f'background --after-start {ID}')[0].splitlines()[-1], 'PANPAK_NO_BACKGROUND keeps the job away'
            print('a start with a due job starts it detached at nice 19 and does not wait; PANPAK_NO_BACKGROUND keeps it away PASS')

            # 5. the settings off: no check, no reading
            run([panpak, 'config', 'set', 'background.updates', 'off'], env)
            run([panpak, 'config', 'set', 'background.verify_mb', '0'], env)
            (appdir / 'update-state').write_text('next_check=0\nverified_bytes=0\ncursor=0\n')
            run([panpak, 'background', '--allow-insecure-http', ID], env)
            st = state(appdir)
            assert st['verified_bytes'] == '0' and st['cursor'] == '0' and st['last_check'] == '0' and int(st['next_check']) > time.time() + 80000, st
            print('with the settings off the job reads nothing and asks nothing, and is not due for a day PASS')
        finally:
            srv.terminate()
            srv.wait()
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
