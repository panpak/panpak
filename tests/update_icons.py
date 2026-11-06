#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak update` with icons: a changed icon, a removed icon, a new size, and no icons at all.

Usage: python3 tests/update_icons.py        (build the release binaries first)
Everything happens in a scratch HOME. Four versions of one app are packed with `[desktop] icons`; the first is installed
from a file, the others are fetched by `panpak update` from a local test server (tests/range_server.py). After each step
the icon files in the user's icon folder, the `Icon=` line of the entry and the files of another app are checked.
"""
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.updicons'
OTHER = 'org.example.other'


def need(cond, msg):
    if not cond:
        sys.exit('FAIL: ' + msg)


def png(side, seed):
    """A valid RGBA PNG of `side` pixels whose bytes depend on `seed`."""
    def chunk(kind, body):
        return struct.pack('>I', len(body)) + kind + body + struct.pack('>I', zlib.crc32(kind + body))
    rows = b''.join(b'\0' + bytes((x * 7 + y * 3 + seed) & 255 for x in range(side * 4)) for y in range(side))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', side, side, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b'')


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-upd-icons-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_RUNTIME_DIR=str(tmp / 'run'))
        (tmp / 'run').mkdir(mode=0o700)
        s = socket.socket()
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
        s.close()

        def run(*args, ok=True):
            p = subprocess.run([str(a) for a in args], env=env, capture_output=True, text=True, timeout=60)
            need((p.returncode == 0) == ok, f'{args[1:3]} returned {p.returncode}: {p.stdout}{p.stderr}')
            return p

        key = tmp / 'secret.key'
        pub = next(l.split()[-1] for l in run(BIN / 'panpak', 'keygen', '-o', key).stdout.splitlines() if l.startswith('key:'))

        def pack(version, icons):
            tree = tmp / f'tree-{version}'
            (tree / 'bin').mkdir(parents=True)
            (tree / 'bin/app').write_text('#!/bin/sh\nexit 0\n')
            (tree / 'bin/app').chmod(0o755)
            names = []
            for side, seed in icons:
                (tree / f'icon{side}.png').write_bytes(png(side, seed))
                names.append(f'"icon{side}.png"')
            desktop = f'[desktop]\nicons = [{", ".join(names)}]\n' if names else ''
            (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Update Icons"\nversion = "{version}"\nentry = "bin/app"\n{desktop}'
                                              f'[update]\nkey = "{pub}"\nurl = "http://127.0.0.1:{port}/remote.panpak"\n')
            out = tmp / f'{version}.panpak'
            run(BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', out)
            return out

        icon_dir = tmp / 'data/icons/hicolor'

        def icon_file(side, app=ID):
            return icon_dir / f'{side}x{side}/apps/panpak.{app}.png'

        def entry_icon():
            text = (tmp / f'data/applications/panpak.{ID}.desktop').read_text()
            return next(l for l in text.splitlines() if l.startswith('Icon='))

        # A file of another app in the same folders must never be touched.
        other = icon_file(48, OTHER)
        other.parent.mkdir(parents=True)
        other.write_bytes(b'not ours')

        v = {
            '1': pack('1', [(16, 1), (48, 1)]),
            '2': pack('2', [(16, 2), (32, 1)]),     # 16 changed, 48 removed, 32 new
            '3': pack('3', []),                      # no icons at all
            '4': pack('4', [(48, 4), (256, 4)]),     # icons again, other sizes
        }
        run(BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', v['1'])
        need(icon_file(16).read_bytes() == png(16, 1) and icon_file(48).read_bytes() == png(48, 1), 'install: icons are not there')
        need(entry_icon() == f'Icon=panpak.{ID}', entry_icon())

        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', tmp, str(port), 'normal'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.4)

            def update(version):
                shutil.copy(v[version], tmp / 'remote.panpak')
                run(BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '10', ID)

            update('2')
            need(icon_file(16).read_bytes() == png(16, 2), 'update 2: the changed icon was not replaced')
            need(icon_file(32).read_bytes() == png(32, 1), 'update 2: the new size is missing')
            need(not icon_file(48).exists(), 'update 2: the icon of the removed size is still there')
            need(entry_icon() == f'Icon=panpak.{ID}', entry_icon())
            print('  update with a changed icon, a removed size and a new size: ok')

            update('3')
            need(not any(icon_dir.glob(f'*/apps/panpak.{ID}.png')), 'update 3: icons are left although the new version has none')
            need(entry_icon() == 'Icon=application-x-executable', entry_icon())
            print('  update to a version with no icons: all removed, generic icon: ok')

            update('4')
            need(icon_file(48).read_bytes() == png(48, 4) and icon_file(256).read_bytes() == png(256, 4), 'update 4: icons are missing')
            need(not icon_file(16).exists() and not icon_file(32).exists(), 'update 4: old sizes came back')
            need(entry_icon() == f'Icon=panpak.{ID}', entry_icon())
            print('  update back to icons of other sizes: ok')
        finally:
            server.terminate()
        need(other.read_bytes() == b'not ours', 'the icon of another app was touched')
        run(BIN / 'panpak', 'remove', ID)
        need(not any(icon_dir.glob(f'*/apps/panpak.{ID}.png')), 'remove: icons are left')
        need(other.read_bytes() == b'not ours', 'remove: the icon of another app was touched')
        print('remove deletes the icons and leaves the other app alone: ok')
    print('all update icon tests passed')


if __name__ == '__main__':
    main()
