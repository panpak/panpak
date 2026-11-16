#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak convert` against an AppImage whose links lead out of the tree only together (P2-03), with real binaries.

`convert` unpacks the AppImage into a private temporary folder and then reads the desktop file and the icons from it by path. The check of a link was lexical, one
link at a time. Here two links are each inside the tree, and together they leave it:

    d/e/l       -> ..                     (lexically `d`, really `d/e/..`, which is `d`)
    app.desktop -> d/e/l/../../secret     (lexically `d/secret`, really the `secret` next to the tree)

The temporary folder is made in a TMPDIR that this test owns, with a file `secret` next to it (a desktop file with a name that must never reach a package). Before the
fix the name of that file went into the manifest. Now the link is not taken, and the note says so.

Checked: the output names nothing of the secret, the manifest of the package neither, and the note lists `app.desktop` among the links that were not taken. A link that stays
inside the tree after going through another link is still taken (the desktop file is read through it).
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

ID = 'org.example.leak'
SECRET = '[Desktop Entry]\nName=LEAKED-FROM-A-HOST-FILE\nExec=app\nVersion=9.9\n'
GOOD = '[Desktop Entry]\nName=Honest Name\nExec=app\nVersion=1.2\n'


def run(args, env, ok=True):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=120)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def manifest_of(package, tmp, env):
    out = tmp / (package.name + '.toml')
    run([BIN / 'panpak', 'dump-manifest', package, out], env)
    return out.read_text()


def squash(src, out):
    run(['mksquashfs', src, out, '-noappend', '-no-progress', '-quiet', '-all-root'], os.environ)


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-convert-links-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        scratch = tmp / 'tmp'
        scratch.mkdir(mode=0o700)
        (scratch / 'secret').write_text(SECRET)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   TMPDIR=str(scratch), LC_ALL='C')
        (tmp / 'secret.key').write_text((bytes([43]) * 32).hex() + '\n')
        (tmp / 'secret.key').chmod(0o600)

        # The hostile AppImage.
        evil = tmp / 'evil-src'
        (evil / 'usr/bin').mkdir(parents=True)
        (evil / 'usr/bin/app').write_text('#!/bin/sh\necho hi\n')
        (evil / 'usr/bin/app').chmod(0o755)
        (evil / 'd/e').mkdir(parents=True)
        os.symlink('..', evil / 'd/e/l')
        os.symlink('d/e/l/../../secret', evil / 'app.desktop')
        squash(evil, tmp / 'evil.squashfs')
        out = tmp / 'evil.panpak'
        p = run([BIN / 'panpak', 'convert', tmp / 'evil.squashfs', '--id', ID, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '--entry', 'usr/bin/app', '-o', out], env)
        said = p.stdout + p.stderr
        manifest = manifest_of(out, tmp, env)
        assert 'LEAKED' not in said and 'LEAKED' not in manifest, f'a line of a file outside the tree reached the output:\n{said}\n{manifest}'
        assert 'app.desktop' in said, f'the link that leaves the tree is not listed among the links that were not taken:\n{said}'
        print('  two links that leave the tree together: not taken, nothing of the file outside reaches the package')

        # A chain of links that stays inside is still read.
        fine = tmp / 'fine-src'
        (fine / 'usr/bin').mkdir(parents=True)
        (fine / 'usr/bin/app').write_text('#!/bin/sh\necho hi\n')
        (fine / 'usr/bin/app').chmod(0o755)
        (fine / 'usr/share').mkdir(parents=True)
        (fine / 'usr/share/real.desktop').write_text(GOOD)
        os.symlink('usr/share/real.desktop', fine / 'mid.desktop')
        os.symlink('mid.desktop', fine / 'app.desktop')
        squash(fine, tmp / 'fine.squashfs')
        out2 = tmp / 'fine.panpak'
        p = run([BIN / 'panpak', 'convert', tmp / 'fine.squashfs', '--id', ID, '--stub', BIN / 'panpak-stub', '--secret-file', tmp / 'secret.key', '-o', out2], env)
        manifest = manifest_of(out2, tmp, env)
        assert 'Honest Name' in manifest, f'a chain of links inside the tree is no longer read:\n{p.stdout}{p.stderr}\n{manifest}'
        print('  a chain of links that stays inside the tree: read as before')
        print('convert links PASS')
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
