#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Real X11 client under Landlock. Build release binaries first.

Starts isolated Xvfb; optionally also tests XWayland with --xwayland.
No screenshots or input events on the user's desktop.
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import PUBLIC, run


def main():
    xvfb = shutil.which('Xvfb') or str(Path.home() / 'xvfb/usr/bin/Xvfb')
    with tempfile.TemporaryDirectory(prefix='panpak-x11-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        fd = os.open(tmp / 'display', os.O_CREAT | os.O_RDWR, 0o600)
        server = subprocess.Popen([xvfb, '-displayfd', str(fd), '-ac', '-screen', '0', '1280x800x24'],
                                  pass_fds=[fd], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            import time
            for _ in range(100):
                text = (tmp / 'display').read_text().strip()
                if text: break
                time.sleep(0.1)
            assert text, 'Xvfb did not start'
            original = dict(os.environ)
            env = dict(original, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), DISPLAY=':' + text)
            env.pop('XAUTHORITY', None)
            tree = tmp / 'tree'
            tree.mkdir()
            (tree / 'show').write_text('#!/bin/sh\nexec /usr/bin/xdpyinfo\n')
            (tree / 'show').chmod(0o755)
            (tmp / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
            (tmp / 'secret.key').chmod(0o600)
            for x11 in (False, True):
                (tree / 'panpak.toml').write_text(
                    '[app]\nid="org.example.xclient"\nname="X11"\nversion="1"\nentry="show"\n'
                    f'[permissions]\nx11={str(x11).lower()}\n[update]\nkey="ed25519:{PUBLIC}"\n')
                run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file',
                     tmp / 'secret.key', '-o', tmp / 'x.panpak'], env)
                result = run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', tmp / 'x.panpak'], env)
                assert ('WARNING X11' in result.stdout) == x11
                result = run([BIN / 'panpak-stub', 'run', tmp / 'x.panpak'], env, ok=x11)
                if x11:
                    assert 'number of screens' in result.stdout
                print('Xvfb x11=', x11, 'PASS')
                if x11 and '--xwayland' in sys.argv:
                    # Dedicated query only, no observation of desktop contents.
                    env['DISPLAY'] = original.get('DISPLAY', ':0')
                    auth = original.get('XAUTHORITY')
                    if auth:
                        shutil.copy(auth, tmp / 'auth')
                        env['XAUTHORITY'] = str(tmp / 'auth')
                    result = run([BIN / 'panpak-stub', 'run', tmp / 'x.panpak'], env)
                    assert 'number of screens' in result.stdout
                    print('XWayland', env['DISPLAY'], 'PASS')
        finally:
            os.close(fd)
            server.terminate()
            server.wait(timeout=5)


if __name__ == '__main__':
    main()
