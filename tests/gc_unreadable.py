#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`panpak gc` never collects what it cannot read the links of (P2-04), with real binaries.

A link of an app (`current`, `previous`, `pending`) that cannot be read is not "no link": the collection used to skip it, found no record alive for the app and took its
record, tree and farm. After PR #31 an absolute link (a store of an earlier version) is an error of the reader, so a store of that kind lost its apps at the first
`update`, `apply`, `remove` or `gc`. Checked: an absolute `current`, a `current` that is a file, a folder in `apps/` that is not an app id: nothing is collected, and the
report says why; a store with only readable links is collected as before.

Nothing here deletes or replaces a file: the odd stores are made new, beside the installed one (one rename of a folder to a name that is free).
"""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import isolation

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import BIN, PUBLIC  # noqa: E402

ID = 'org.example.incremental'


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def lab(tag):
    """A fresh store with the app installed, and the pieces that a case needs."""
    work = Path(tempfile.mkdtemp(prefix=f'panpak-gcu-{tag}-', dir=ROOT / 'target' / 'tmp-tests'))
    isolation.require_isolated_home(work, work)
    runtime = work / 'run'
    runtime.mkdir(mode=0o700)
    env = dict(os.environ, HOME=str(work), XDG_DATA_HOME=str(work / 'data'), XDG_CACHE_HOME=str(work / 'cache'), XDG_CONFIG_HOME=str(work / 'config'),
               XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
    env.pop('WAYLAND_DISPLAY', None)
    (work / 'secret.key').write_text((bytes([41]) * 32).hex() + '\n')
    (work / 'secret.key').chmod(0o600)
    tree = work / 'tree'
    tree.mkdir()
    (tree / 'hello').write_text('#!/bin/sh\necho hi\n')
    (tree / 'hello').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid="{ID}"\nname="T"\nversion="1"\nentry="hello"\n[update]\nkey="ed25519:{PUBLIC}"\n')
    (tree / 'data.bin').write_bytes(bytes(range(256)) * 200)
    pkg = work / 'v1.panpak'
    run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', work / 'secret.key', '-o', pkg], env)
    run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', pkg], env)
    return work, env, work / 'data/panpak'


def odd_app_folder(store, work, make_current):
    """Move the folder of the app out of the store (to a free name), make a new one with the same files except `current`, which `make_current` makes."""
    appdir = store / 'apps' / ID
    aside = work / 'aside'
    aside.mkdir()
    old_current = (appdir / 'current').resolve()
    appdir.rename(aside / ID)
    appdir.mkdir()
    for f in (aside / ID).iterdir():
        if f.name != 'current' and f.is_file() and not f.is_symlink():
            shutil.copy2(f, appdir / f.name)
    make_current(appdir / 'current', old_current)


def main():
    (ROOT / 'target' / 'tmp-tests').mkdir(parents=True, exist_ok=True)
    cases = {
        'an absolute current (a store of an earlier version)': lambda link, rec: link.symlink_to(rec),
        'a current that is a file': lambda link, rec: link.write_bytes(b'not a link\n'),
        'a current that is a folder': lambda link, rec: link.mkdir(),
    }
    failures = []
    for tag, (what, make) in enumerate(cases.items()):
        work, env, store = lab(f'c{tag}')
        records = sorted(p.name for p in (store / 'packages').iterdir())
        trees = sorted(p.name for p in (store / 'trees').iterdir() if not p.name.endswith('.lock'))
        farms = sorted(p.name for p in (store / 'farms').iterdir())
        odd_app_folder(store, work, make)
        dry = run([BIN / 'panpak', 'gc', '--dry-run'], env).stdout
        real = run([BIN / 'panpak', 'gc'], env).stdout
        left = (sorted(p.name for p in (store / 'packages').iterdir()), sorted(p.name for p in (store / 'trees').iterdir() if not p.name.endswith('.lock')),
                sorted(p.name for p in (store / 'farms').iterdir()))
        if left != (records, trees, farms) or 'would remove' in dry:
            failures.append(f'{what}: the collection took what it could not read the link of: dry run {dry.strip()!r}; left {left}')
        elif 'cannot be read' not in real and 'kept whole' not in real:
            failures.append(f'{what}: nothing was taken, but the report says nothing about it: {real.strip()!r}')
        else:
            print(f'  {what}: nothing collected; {real.strip().splitlines()[0][:100]}')
    # A folder of `apps/` that is not an app id: its links are not read, so every record is kept.
    work, env, store = lab('strange')
    before = sorted(p.name for p in (store / 'packages').iterdir())
    (store / 'apps' / ID).rename(store / 'apps' / 'not an app')   # a folder that holds the links, under a name that is not an app id
    real = run([BIN / 'panpak', 'gc'], env).stdout
    after = sorted(p.name for p in (store / 'packages').iterdir())
    if after != before:
        failures.append(f'a folder of apps/ that is not an app id: the records went: {before} -> {after}')
    # Readable links: the collection works as before (an unreferenced record goes).
    work, env, store = lab('plain')
    stray = store / 'packages' / f'{ID}@9.9@{"ee" * 32}.rec'
    stray.write_bytes(b'junk')
    out = run([BIN / 'panpak', 'gc'], env).stdout
    if stray.exists() or 'record' not in out:
        failures.append(f'readable links: the stray record was not collected: {out.strip()!r}')
    assert not failures, 'FAILED:\n' + '\n'.join(failures)
    print('an unreadable link keeps the whole app and says so; readable stores are collected as before PASS')
    for d in sorted((ROOT / 'target' / 'tmp-tests').glob('panpak-gcu-*')):
        subprocess.run(['chmod', '-R', 'u+w', str(d)], check=False)


if __name__ == '__main__':
    main()
