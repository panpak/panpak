#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak update` with icons: a changed icon, a removed icon, a new size, and no icons at all. The entry names the largest icon of the tree by absolute path
(nothing is copied to the user's hicolor); the copies that an earlier panpak made there are taken away at the first update, only the ones that panpak made.

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
                                              f'[release]\nrevision = {version}\n[update]\nkey = "{pub}"\nurl = "http://127.0.0.1:{port}/remote.panpak"\n')
            out = tmp / f'{version}.panpak'
            run(BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', out)
            return out

        icon_dir = tmp / 'data/icons/hicolor'

        def icon_file(side, app=ID):
            return icon_dir / f'{side}x{side}/apps/panpak.{app}.png'

        def entry_icon():
            text = (tmp / f'data/applications/panpak.{ID}.desktop').read_text()
            return next(l for l in text.splitlines() if l.startswith('Icon='))

        def need_icon(side, seed, step):
            """The entry names, by absolute path, a regular file of the tree (no link on the way) that holds the icon of this side."""
            v = entry_icon()[len('Icon='):]
            need(v.startswith('/'), f'{step}: Icon= is not an absolute path: {v}')
            f = Path(v)
            need(f.is_file() and not f.is_symlink() and f.read_bytes() == png(side, seed), f'{step}: {v} is not the icon of side {side}')
            need(f.resolve() == f, f'{step}: the path of the icon goes through a link: {v}')
            need('/trees/' in v and f.name == f'{side}.png', f'{step}: the icon is not a file of the tree: {v}')
            need(set(icon_dir.glob(f'*/apps/panpak.{ID}.png')) <= set(placed), f'{step}: an icon was copied to the hicolor folder')

        # A file of another app in the same folders must never be touched.
        other = icon_file(48, OTHER)
        other.parent.mkdir(parents=True)
        other.write_bytes(b'not ours')

        # The copies that an earlier panpak made for this app (the bytes of version 1) and one that someone changed: only the first two go.
        icon_file(16).parent.mkdir(parents=True, exist_ok=True)
        icon_file(16).write_bytes(png(16, 1))
        icon_file(48).write_bytes(png(48, 1))
        icon_file(256).parent.mkdir(parents=True, exist_ok=True)
        icon_file(256).write_bytes(b'changed by somebody')
        placed = [icon_file(16), icon_file(48), icon_file(256)]      # panpak copies nothing: the files of the folder stay among these
        icon_file(64, OTHER).parent.mkdir(parents=True, exist_ok=True)
        icon_file(64, OTHER).write_bytes(png(16, 1))
        v = {
            '1': pack('1', [(16, 1), (48, 1)]),
            '2': pack('2', [(16, 2), (32, 1)]),     # 16 changed, 48 removed, 32 new
            '3': pack('3', []),                      # no icons at all
            '4': pack('4', [(48, 4), (256, 4)]),     # icons again, other sizes
        }
        run(BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', v['1'])
        need_icon(48, 1, 'install')
        need(icon_file(16).exists() and icon_file(48).exists(), 'install: the old copies went before the first update')

        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', tmp, str(port), 'normal'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.4)

            def update(version):
                shutil.copy(v[version], tmp / 'remote.panpak')
                run(BIN / 'panpak', 'update', '--allow-insecure-http', '--timeout', '10', ID)

            update('2')
            need_icon(32, 1, 'update 2')       # 16 changed, 48 removed, 32 new: the largest is the new 32
            need(not icon_file(16).exists() and not icon_file(48).exists(), 'update 2: the copies that panpak made (bytes of version 1) are still in hicolor')
            need(icon_file(256).read_bytes() == b'changed by somebody', 'update 2: a file with our name and other bytes was taken away')
            need(icon_file(64, OTHER).exists(), 'update 2: the file of another app was taken away')
            print('  update: the entry names the largest icon of the tree; the old copies of panpak go, the changed one and the other app stay: ok')

            update('3')
            need(entry_icon() == 'Icon=application-x-executable', entry_icon())
            print('  update to a version with no icons: generic icon: ok')

            update('4')
            need_icon(256, 4, 'update 4')
            print('  update back to icons of other sizes: the largest of the tree: ok')
        finally:
            server.terminate()
        need(other.read_bytes() == b'not ours', 'the icon of another app was touched')
        run(BIN / 'panpak', 'remove', ID)
        need(not (tmp / f'data/applications/panpak.{ID}.desktop').exists(), 'remove: the entry is left')
        need(other.read_bytes() == b'not ours' and icon_file(256).read_bytes() == b'changed by somebody', 'remove: a file that panpak did not make was touched')
        print('remove takes the entry and leaves the other files alone: ok')
    print('all update icon tests passed')


if __name__ == '__main__':
    main()
