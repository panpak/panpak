#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Private SQLITE_TMPDIR: the stub points it at <appdata>/tmp (writable),
respects an explicit user value. Build release first."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC, run


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-sqlitetmp-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'))
        env.pop('SQLITE_TMPDIR', None)
        tree = tmp / 'tree'
        tree.mkdir()
        (tree / 'go').write_text('#!/bin/sh\necho "TMPDIR=$SQLITE_TMPDIR"\n'
                                 'touch "$SQLITE_TMPDIR/probe" && echo PROBE-OK\n')
        (tree / 'go').chmod(0o755)
        (tree / 'panpak.toml').write_text('[app]\nid="org.example.sqlitetmp"\nname="tmp"\nversion="1"\nentry="go"\n'
                                         f'[update]\nkey="ed25519:{PUBLIC}"\n')
        pkg = tmp / 't.panpak'
        (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)
        run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', pkg], env)
        run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', pkg], env)
        appdata = tmp / 'data/org.example.sqlitetmp'
        p = run([BIN / 'panpak-stub', 'run', pkg], env)
        assert f'TMPDIR={appdata}/tmp' in p.stdout, p.stdout
        assert 'PROBE-OK' in p.stdout, p.stdout
        assert (appdata / 'tmp/probe').is_file()
        # Explicit value respected: no override.
        env['SQLITE_TMPDIR'] = '/tmp'
        if (appdata / 'tmp').exists():
            for f in (appdata / 'tmp').iterdir():
                f.unlink()
        p = run([BIN / 'panpak-stub', 'run', pkg], env)
        assert 'TMPDIR=/tmp' in p.stdout, p.stdout
        assert 'PROBE-OK' in p.stdout, p.stdout
        print('SQLITE_TMPDIR private/respected PASS')


if __name__ == '__main__':
    main()
