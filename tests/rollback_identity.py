#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak rollback` does not take the record of another app as the previous version (P2-09), with real binaries.

`rollback` swapped `current` and `previous` after looking at the tree of `previous` and nothing else. If `previous` of one app names the record of another app, the swap made
`current` of the first app name the other's record: the app could no longer start (the start refuses a record of another app, PP-01), and no command said why.

Checked: after both apps are installed, `previous` of the first is made to name the record of the second; `rollback` fails, says the record belongs to another app, and both links stay
as they were. A real rollback (an update installed over the first version) still works.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN  # noqa: E402

A, B = 'org.example.ra', 'org.example.rb'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-rollback-id-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
        env.pop('WAYLAND_DISPLAY', None)
        keys = {}
        for who in ('a', 'b'):
            key = tmp / f'{who}.key'
            out = run([BIN / 'panpak', 'keygen', '-o', key], env).stdout
            keys[who] = (key, next(line.split()[-1] for line in out.splitlines() if line.startswith('key:')))

        def package(app_id, version, who, name):
            tree = tmp / f'tree-{name}'
            tree.mkdir()
            (tree / 'hello').write_text(f'#!/bin/sh\necho {name}\n')
            (tree / 'hello').chmod(0o755)
            (tree / 'panpak.toml').write_text(f'[app]\nid="{app_id}"\nname="T"\nversion="{version}"\nentry="hello"\n[release]\nrevision={version}\n[update]\nkey="{keys[who][1]}"\n')
            out = tmp / f'{name}.panpak'
            run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', keys[who][0], '-o', out], env)
            return out

        install = lambda pkg: run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', pkg], env)
        install(package(A, '1', 'a', 'a1'))
        install(package(B, '1', 'b', 'b1'))
        store = tmp / 'data/panpak'
        a_dir, b_dir = store / 'apps' / A, store / 'apps' / B
        assert not (a_dir / 'previous').exists(), 'a first install has no previous version'
        # `previous` of A names the record of B (made new: there was none).
        b_record = (b_dir / 'current').resolve()
        os.symlink(os.path.relpath(b_record, a_dir), a_dir / 'previous')
        cur_before, prev_before = os.readlink(a_dir / 'current'), os.readlink(a_dir / 'previous')
        p = run([BIN / 'panpak', 'rollback', A], env, ok=False)
        assert p.returncode != 0, f'rollback took the record of another app as the previous version:\n{p.stdout}{p.stderr}'
        assert 'another app' in p.stderr or 'does not match' in p.stderr, p.stderr
        assert os.readlink(a_dir / 'current') == cur_before and os.readlink(a_dir / 'previous') == prev_before, 'a link was changed by a refused rollback'
        print('  `previous` that names the record of another app: rollback refused, both links unchanged')

        # A real rollback still works: `previous` of B names the record of B's first version (made new, there was none), and the swap brings it back.
        b1_record = (b_dir / 'current').resolve()
        install(package(B, '2', 'b', 'b2'))
        assert (b_dir / 'current').resolve() != b1_record and b1_record.exists(), 'the second version has its own record'
        if not (b_dir / 'previous').exists():
            os.symlink(os.path.relpath(b1_record, b_dir), b_dir / 'previous')
        run([BIN / 'panpak', 'rollback', B], env)
        assert run([store / 'bin/panpak-stub', 'run', b_dir / 'current'], env).stdout.strip() == 'b1', 'the rollback did not bring the first version back'
        print('  a real rollback still brings the previous version back')
        print('rollback identity PASS')
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
