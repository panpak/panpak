#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak open --write-rename` and the folder it opens (PP-07), with real binaries.

The grant of `--write-rename` is create, rename, remove and write in the folder of the file, and a Landlock rule on a folder covers everything below it. With
a file in `$HOME` the app could write `~/.bashrc` and create `~/.config/autostart/x.desktop`, and the warning only spoke of the neighbours. Now a folder that holds, at
any depth, a place where a write is a way to run a program later (the home itself, `.config`, `.local`, and the files and folders of the list in the stub) is refused,
with the reason and what to do; an ordinary folder works as before and the app cannot reach the rest of the home.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC  # noqa: E402

ID = 'org.example.renamer'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def main():
    # Not under /tmp: the view gives the app a private /tmp, and the home of the test must be a path that the view binds as it is.
    base = ROOT / 'target' / 'tmp-tests'
    base.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='panpak-openrename-', dir=base) as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        home = tmp / 'home'
        home.mkdir()
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'), XDG_CACHE_HOME=str(home / '.cache'), XDG_CONFIG_HOME=str(home / '.config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
        env.pop('WAYLAND_DISPLAY', None)
        (home / '.config/autostart').mkdir(parents=True)
        (home / 'Documents').mkdir()
        (home / '.bashrc').write_text('# mine\n')
        for f in (home / 'doc.txt', home / '.config/app.conf', home / 'Documents/note.txt', home / 'Documents/neighbour.txt'):
            f.write_text('x\n')
        probes = {'bashrc': home / '.bashrc', 'autostart': home / '.config/autostart/evil.desktop', 'neighbour': home / 'Documents/neighbour.txt'}
        script = '#!/bin/sh\n' + ''.join(f'if ( echo x >> {p} ) 2>/dev/null; then echo {k}=ok; else echo {k}=denied; fi\n' for k, p in probes.items())
        tree = tmp / 'tree'
        tree.mkdir()
        (tree / 'hello').write_text(script)
        (tree / 'hello').chmod(0o755)
        (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="R"\nversion="1"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        pkg = tmp / 'r.panpak'
        run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', pkg], env)
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', pkg], env)

        # The same checks with bubblewrap made unusable (the app then runs with Landlock alone, and the folder is open as the rule says).
        mask = ['/usr/bin/bwrap', '--unshare-user', '--bind', '/', '/', '--dev-bind', '/dev', '/dev', '--proc', '/proc', '--ro-bind', '/dev/null', '/usr/bin/bwrap', '--']

        def opened(path, flag='--write-rename', landlock_only=False):
            p = run([*(mask if landlock_only else []), BIN / 'panpak', 'open', flag, ID, path], env, ok=False)
            return p, dict(line.split('=', 1) for line in p.stdout.split() if '=' in line)

        failures = []
        for only in (False, True):
            how = 'Landlock alone' if only else 'isolated view'
            # Folders that hold a place where a write is a way to run a program later: refused, and the app never starts.
            for path in (home / 'doc.txt', home / '.config/app.conf'):
                p, got = opened(path, landlock_only=only)
                if p.returncode == 0 or got:
                    failures.append(f'{how}: {path.relative_to(home)}: the app started with write access to the folder ({got}); stderr: {p.stderr.strip()[-120:]}')
                elif 'refus' not in p.stderr:
                    failures.append(f'{how}: {path.relative_to(home)}: refused without saying why: {p.stderr.strip()[-200:]}')
            # In the view the app has a private home at the same path: a write outside the grant succeeds there and never reaches
            # the real file (a folder of the real home that the private home does not have, like `.config/autostart`, is not there at all).
            # With Landlock alone the same write is denied.
            outside = 'ok' if not only else 'denied'
            real = {'bashrc': home / '.bashrc', 'autostart': home / '.config/autostart/evil.desktop', 'neighbour': home / 'Documents/neighbour.txt'}

            def real_changed():
                return {'bashrc': real['bashrc'].read_text() != '# mine\n', 'autostart': real['autostart'].exists(), 'neighbour': real['neighbour'].read_text() != 'x\n'}

            def reset():
                real['bashrc'].write_text('# mine\n')
                real['autostart'].unlink(missing_ok=True)
                real['neighbour'].write_text('x\n')

            # An ordinary folder works as before: its neighbours can be written (that is the grant), the rest of the home cannot be reached.
            p, got = opened(home / 'Documents/note.txt', landlock_only=only)
            if got != {'bashrc': outside, 'autostart': 'denied', 'neighbour': 'ok'} or real_changed() != {'bashrc': False, 'autostart': False, 'neighbour': True}:
                failures.append(f'{how}: Documents/note.txt: {got} {real_changed()} {p.stderr.strip()[-200:]}')
            reset()
            # Without --write-rename nothing but the file is writable.
            p, got = opened(home / 'doc.txt', '--write', landlock_only=only)
            if got != {'bashrc': outside, 'autostart': 'denied', 'neighbour': outside} or any(real_changed().values()):
                failures.append(f'{how}: --write only: {got} {real_changed()} {p.stderr.strip()[-200:]}')
            reset()
        assert not failures, 'FAILED:\n' + '\n'.join(failures)
        for f in (home / '.bashrc',):
            assert f.read_text() == '# mine\n', 'the file of the user was not touched'
        print('--write-rename: a folder that holds the home, .config or another place that starts programs is refused with the reason; an ordinary folder works, the rest of the home is out of reach; --write grants the file only PASS')
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
