#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Packages carry NOTICE and license texts under .panpak/licenses/; they are
shown by `<package> --panpak-licenses` and `panpak licenses <package>`, and
the app never sees --panpak-licenses. Build release first."""
import os
from pathlib import Path
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC, run

EXPECTED = ['LICENSE-APACHE', 'LICENSE-MIT', 'NOTICE', 'THIRD-PARTY-LICENSES']


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-lic-e2e-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'))
        tree = tmp / 'tree'
        tree.mkdir()
        (tree / 'go').write_text('#!/bin/sh\necho "args=$*"\n')
        (tree / 'go').chmod(0o755)
        (tree / 'panpak.toml').write_text('[app]\nid="org.example.lic"\nname="lic"\nversion="1"\nentry="go"\n'
                                          f'[update]\nkey="ed25519:{PUBLIC}"\n')
        key = tmp / 'secret.key'
        key.write_text((bytes([41]) * 32).hex() + '\n')
        key.chmod(0o600)
        pkg = tmp / 'l.panpak'
        run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', pkg], env)
        for cmd in ([pkg, '--panpak-licenses'], [BIN / 'panpak', 'licenses', pkg]):
            out = run(cmd, env).stdout
            for name in EXPECTED:
                assert f'===== {name} =====' in out, (cmd, name)
            assert 'The panpak Authors' in out and 'Apache License' in out, cmd
        # Same texts as the CLI's own copy.
        def sections(text):
            parts = text.split('===== ')[1:]
            return dict(part.split(' =====\n', 1) for part in parts)
        assert sections(run([BIN / 'panpak', 'licenses'], env).stdout) == sections(run([pkg, '--panpak-licenses'], env).stdout)
        # Stub flag is consumed by the stub; the app still runs and gets other args.
        out = run([pkg, 'a', 'b'], env).stdout
        assert 'args=a b' in out, out
        print('licenses embedded/shown PASS')


if __name__ == '__main__':
    main()
