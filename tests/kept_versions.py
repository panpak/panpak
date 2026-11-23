#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Kept versions (PANPAK-FORMAT.md 6.10) with real binaries: three good starts make a version the one that is known to work; two failed starts in a row of the next
version ask the person (a stand-in `zenity` answers yes), and `rollback` goes back to the version that worked; `gc` keeps what it must.

Usage: python3 tests/kept_versions.py   (debug musl binaries built first; an isolated HOME and store; PANPAK_QUICK_SECS makes a good start 0.3 s, not 20 s)
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import PUBLIC  # noqa: E402

# A debug build: only there `PANPAK_TEST_TOOLS_DIR` puts the stand-in window tool in place of the real one, so no real window is ever shown.
BIN = ROOT / 'target/x86_64-unknown-linux-musl/debug'

ID = 'org.example.kept'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, f'{[str(x) for x in args][:3]} ended with status {p.returncode}\n{p.stdout}{p.stderr}'
    return p


def main():
    base = ROOT / 'target' / 'tmp-tests'
    base.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='panpak-kept-', dir=base) as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        home = tmp / 'home'
        home.mkdir()
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        fake = tmp / 'fakebin'
        fake.mkdir()
        # The window: a stand-in for zenity that says yes and leaves a trace of what it was asked.
        (fake / 'zenity').write_text(f'#!/bin/sh\nprintf "%s\\n" "$@" > {tmp}/asked.txt\nexit 0\n')
        (fake / 'zenity').chmod(0o755)
        env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'), XDG_CACHE_HOME=str(home / '.cache'), XDG_CONFIG_HOME=str(home / '.config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_QUICK_SECS='0.3', PANPAK_NO_BACKGROUND='1', PANPAK_TEST_TOOLS_DIR=str(fake))
        env.pop('WAYLAND_DISPLAY', None)
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        tree = tmp / 'tree'
        tree.mkdir()
        previous = None

        def release(version, body):
            nonlocal previous
            (tree / 'hello').write_text('#!/bin/sh\n' + body)
            (tree / 'hello').chmod(0o755)
            (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="Kept App"\nversion="{version}"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
            out = tmp / f'{version}.panpak'
            cmd = [BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out]
            if previous:
                cmd += ['--previous', previous]
            run(cmd, env)
            previous = out
            return out

        v1 = release('1.0', 'sleep 0.6\nexit 0\n')
        v2 = release('2.0', 'exit 3\n')
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', v1], env)
        store = home / '.local/share/panpak'
        appdir = store / 'apps' / ID
        start = [store / 'bin/panpak-stub', 'run', appdir / 'current']

        def starts(n):
            for _ in range(n):
                run(start, env, ok=False)

        starts(2)
        assert not (appdir / 'last_good').exists(), 'two good starts do not prove a version'
        starts(1)
        assert (appdir / 'last_good').is_symlink(), 'three good starts do'
        good = os.readlink(appdir / 'last_good')
        # The new version fails to start twice in a row: the person is asked, and the answer (yes) goes back.
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--accept-new-permissions', '--runtime', BIN / 'panpak-stub', v2], env)
        assert os.readlink(appdir / 'current') != good
        starts(2)
        for _ in range(100):
            if os.readlink(appdir / 'current') == good:
                break
            time.sleep(0.1)
        assert os.readlink(appdir / 'current') == good, 'the version that worked is current again: ' + (appdir / 'starts').read_text() + str(sorted(p.name for p in tmp.iterdir()))
        asked = (tmp / 'asked.txt').read_text()
        assert "Kept App doesn't start" in asked and 'Go back to 1.0' in asked and 'Try again' in asked and 'failed to start twice' in asked, asked
        # The line of `panpak rollback` and the choice of the target.
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--accept-new-permissions', '--runtime', BIN / 'panpak-stub', v2], env, ok=False)
        p = run([BIN / 'panpak', 'rollback', ID], env, ok=False)
        print((p.stdout + p.stderr).strip().splitlines()[-1] if (p.stdout + p.stderr).strip() else '')
        # `gc` keeps current and last_good, and `previous` while the current one has not worked.
        run([BIN / 'panpak', 'gc'], env)
        assert (appdir / 'current').exists() and (appdir / 'last_good').exists()
        run([BIN / 'panpak', 'keep-previous', ID], env)
        assert (appdir / 'keep_previous').exists()
        run([BIN / 'panpak', 'keep-previous', '--off', ID], env)
        assert not (appdir / 'keep_previous').exists()
        print('kept versions PASS')
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
